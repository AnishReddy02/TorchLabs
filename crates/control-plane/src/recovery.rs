use crate::{api::AppState, grpc::NodeHub};
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use bench_protocol::bench::{
    control_message, ControlMessage, Envelope, ExecuteRemediation, RemediationAcknowledged,
    RemediationCompleted, ServiceObservation,
};
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

pub const RESTART_SERVICE: &str = "RESTART_SERVICE";
const MAX_EVIDENCE_BYTES: usize = 8 * 1024;
type ActiveIncident = (Uuid, String, i32, Option<chrono::DateTime<Utc>>, i32);

pub async fn disconnect(pool: &PgPool, asset: &str) -> anyhow::Result<()> {
    let ids:Vec<Uuid>=sqlx::query_scalar("UPDATE incidents SET status='ESCALATED',escalation_reason='node disconnected; physical execution outcome uncertain',updated_at=now() WHERE asset_id=(SELECT id FROM assets WHERE name=$1) AND status NOT IN ('RESOLVED','ESCALATED','FAILED','CANCELLED') RETURNING id").bind(asset).fetch_all(pool).await?;
    for id in ids {
        event(
            pool,
            id,
            "NODE_DISCONNECTED",
            "control-plane",
            json!({"replayed":false}),
        )
        .await?;
        sqlx::query("UPDATE remediation_executions SET status='EXPIRED',error='node disconnected',updated_at=now() WHERE incident_id=$1 AND status IN ('AUTHORIZED','DISPATCHED','ACKNOWLEDGED')").bind(id).execute(pool).await?;
    }
    Ok(())
}

pub async fn start_reconciler(pool: PgPool) -> anyhow::Result<()> {
    // Never replay an operation whose physical outcome is uncertain after a restart.
    let ids: Vec<Uuid> = sqlx::query_scalar("UPDATE incidents SET status='ESCALATED',escalation_reason='control plane restarted; inspect physical state before retry',updated_at=now() WHERE status NOT IN ('RESOLVED','ESCALATED','FAILED','CANCELLED') RETURNING id").fetch_all(&pool).await?;
    for id in ids {
        event(
            &pool,
            id,
            "RESTART_ESCALATION",
            "control-plane",
            json!({"replayed":false}),
        )
        .await?;
    }
    sqlx::query("UPDATE remediation_executions SET status='EXPIRED',error='control plane restarted; execution outcome uncertain',updated_at=now() WHERE status IN ('AUTHORIZED','DISPATCHED','ACKNOWLEDGED')").execute(&pool).await?;
    tokio::spawn(async move {
        loop {
            if let Err(error) = reconcile(&pool).await {
                tracing::error!(%error,"recovery reconciliation failed");
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    });
    Ok(())
}

async fn reconcile(pool: &PgPool) -> anyhow::Result<()> {
    let ids: Vec<Uuid> = sqlx::query_scalar("UPDATE incidents SET status='ESCALATED',escalation_reason='recovery deadline expired or observations stopped; execution outcome may be uncertain',updated_at=now() WHERE status NOT IN ('RESOLVED','ESCALATED','FAILED','CANCELLED') AND (created_at < now()-interval '5 minutes' OR (status='VERIFYING' AND verification_deadline < now()) OR EXISTS(SELECT 1 FROM remediation_executions r WHERE r.incident_id=incidents.id AND r.status IN ('AUTHORIZED','DISPATCHED','ACKNOWLEDGED') AND r.deadline<now())) RETURNING id").fetch_all(pool).await?;
    for id in ids {
        event(
            pool,
            id,
            "DEADLINE_ESCALATION",
            "control-plane",
            json!({"replayed":false}),
        )
        .await?;
        sqlx::query("UPDATE remediation_executions SET status='EXPIRED',error='deadline expired',updated_at=now() WHERE incident_id=$1 AND status IN ('AUTHORIZED','DISPATCHED','ACKNOWLEDGED')").bind(id).execute(pool).await?;
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub struct RecoveryPolicy {
    pub enabled: bool,
    pub allowed_asset: String,
    pub allowed_service: String,
    pub max_attempts: i32,
    pub cooldown_seconds: i64,
    pub operation_deadline_seconds: i64,
    pub verification_timeout_seconds: i64,
    pub required_healthy_observations: i32,
}

impl RecoveryPolicy {
    pub fn from_env() -> Self {
        Self {
            enabled: env_bool("TORCH_RECOVERY_ENABLED", false),
            allowed_asset: std::env::var("TORCH_RECOVERY_ALLOWED_ASSET").unwrap_or_default(),
            allowed_service: std::env::var("TORCH_RECOVERY_ALLOWED_SERVICE").unwrap_or_default(),
            max_attempts: 1,
            cooldown_seconds: env_i64("TORCH_RECOVERY_COOLDOWN_SECONDS", 60).clamp(0, 3600),
            operation_deadline_seconds: env_i64("TORCH_RECOVERY_DEADLINE_SECONDS", 30)
                .clamp(5, 300),
            verification_timeout_seconds: env_i64("TORCH_RECOVERY_VERIFY_SECONDS", 30)
                .clamp(5, 300),
            required_healthy_observations: env_i64("TORCH_RECOVERY_HEALTHY_OBSERVATIONS", 3)
                .clamp(2, 10) as i32,
        }
    }

    fn evaluate(&self, request: &PolicyRequest<'_>) -> PolicyDecision {
        let reason = if !self.enabled || std::path::Path::new(".torch-recovery-disabled").exists() {
            Some("autonomous recovery emergency-disable switch is active")
        } else if request.action_id != RESTART_SERVICE {
            Some("action is not in the remediation allowlist")
        } else if request.asset != self.allowed_asset {
            Some("asset is not in the remediation allowlist")
        } else if request.service_id != self.allowed_service {
            Some("service is not in the remediation allowlist")
        } else if !matches!(request.asset_status, "AVAILABLE" | "DEGRADED") {
            Some("asset state does not permit autonomous remediation")
        } else if !request.connected {
            Some("asset node is disconnected")
        } else if request.attempts >= self.max_attempts {
            Some("maximum remediation attempts reached")
        } else if request.in_cooldown {
            Some("service remediation cooldown is active")
        } else if request.concurrent_operation {
            Some("another remediation operation is active")
        } else {
            None
        };
        PolicyDecision {
            authorized: reason.is_none(),
            reason: reason
                .unwrap_or("exact allowlist and runtime preconditions satisfied")
                .into(),
        }
    }
}

fn env_bool(name: &str, default: bool) -> bool {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}
fn env_i64(name: &str, default: i64) -> i64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

struct PolicyRequest<'a> {
    asset: &'a str,
    service_id: &'a str,
    action_id: &'a str,
    asset_status: &'a str,
    connected: bool,
    attempts: i32,
    in_cooldown: bool,
    concurrent_operation: bool,
}

#[derive(Debug, Serialize)]
struct PolicyDecision {
    authorized: bool,
    reason: String,
}

pub fn valid_transition(from: &str, to: &str) -> bool {
    if matches!(from, "RESOLVED" | "ESCALATED" | "FAILED" | "CANCELLED") {
        return false;
    }
    matches!(
        (from, to),
        ("DETECTED", "INVESTIGATING")
            | ("INVESTIGATING", "DIAGNOSED")
            | ("DIAGNOSED", "AUTHORIZATION_PENDING")
            | ("AUTHORIZATION_PENDING", "REMEDIATING")
            | ("REMEDIATING", "VERIFYING")
            | ("VERIFYING", "RESOLVED")
            | (_, "ESCALATED")
            | (_, "FAILED")
            | (_, "CANCELLED")
    )
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/incidents", get(list_incidents))
        .route("/api/incidents/{id}", get(get_incident))
        .route("/api/incidents/{id}/cancel", post(cancel_incident))
        .route("/api/recovery/policy/evaluate", post(evaluate_policy))
}

fn authorize(state: &AppState, headers: &HeaderMap) -> Result<(), (StatusCode, Json<Value>)> {
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    if token.is_some_and(|v| bench_common::hash_token(v) == state.admin_token_hash) {
        Ok(())
    } else {
        Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"A valid API token is required"})),
        ))
    }
}

async fn cancel_incident(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    authorize(&state, &headers)?;
    let changed=sqlx::query("UPDATE incidents SET status='CANCELLED',escalation_reason='operator cancelled; an already accepted repair may finish; inspect service health',updated_at=now() WHERE id=$1 AND status NOT IN ('RESOLVED','ESCALATED','FAILED','CANCELLED')").bind(id).execute(&state.pool).await.map_err(api_failure)?.rows_affected();
    if changed != 1 {
        return Err((
            StatusCode::CONFLICT,
            Json(json!({"error":"incident missing or terminal"})),
        ));
    }
    event(
        &state.pool,
        id,
        "OPERATOR_CANCELLED",
        "admin",
        json!({"physical_stop_guaranteed":false}),
    )
    .await
    .map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":"audit write failed"})),
        )
    })?;
    Ok(Json(
        json!({"cancelled":true,"notice":"Already accepted repairs may finish; recovery will not be declared."}),
    ))
}

async fn list_incidents(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    authorize(&state, &headers)?;
    let rows: Vec<Value> = sqlx::query_scalar(
        "SELECT jsonb_build_object('id',i.id,'asset_id',i.asset_id,'asset_name',a.name,'service_id',i.service_id,'incident_type',i.incident_type,'severity',i.severity,'status',i.status,'detected_at',i.detected_at,'resolved_at',i.resolved_at,'diagnosis',i.diagnosis,'proposed_remediation',i.proposed_remediation,'remediation_attempts',i.remediation_attempts,'verification_result',i.verification_result,'escalation_reason',i.escalation_reason,'correlation_id',i.correlation_id,'updated_at',i.updated_at) FROM incidents i JOIN assets a ON a.id=i.asset_id ORDER BY i.created_at DESC LIMIT 100",
    )
    .fetch_all(&state.pool)
    .await
    .map_err(api_failure)?;
    Ok(Json(json!(rows)))
}

async fn get_incident(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    authorize(&state, &headers)?;
    let mut incident: Value = sqlx::query_scalar(
        "SELECT to_jsonb(i) || jsonb_build_object('asset_name',a.name) FROM incidents i JOIN assets a ON a.id=i.asset_id WHERE i.id=$1",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await
    .map_err(api_failure)?
    .ok_or_else(|| (StatusCode::NOT_FOUND, Json(json!({"error":"Incident not found"}))))?;
    let events: Vec<Value> = sqlx::query_scalar(
        "SELECT to_jsonb(e) FROM incident_events e WHERE incident_id=$1 ORDER BY id",
    )
    .bind(id)
    .fetch_all(&state.pool)
    .await
    .map_err(api_failure)?;
    let executions: Vec<Value> = sqlx::query_scalar(
        "SELECT to_jsonb(r) FROM remediation_executions r WHERE incident_id=$1 ORDER BY created_at",
    )
    .bind(id)
    .fetch_all(&state.pool)
    .await
    .map_err(api_failure)?;
    incident["events"] = json!(events);
    incident["executions"] = json!(executions);
    Ok(Json(incident))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EvaluateRequest {
    asset: String,
    service_id: String,
    action_id: String,
}

async fn evaluate_policy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<EvaluateRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    authorize(&state, &headers)?;
    let row: Option<(String,)> = sqlx::query_as("SELECT status FROM assets WHERE name=$1")
        .bind(&request.asset)
        .fetch_optional(&state.pool)
        .await
        .map_err(api_failure)?;
    let status = row.as_ref().map(|r| r.0.as_str()).unwrap_or("OFFLINE");
    let decision = state.recovery_policy.evaluate(&PolicyRequest {
        asset: &request.asset,
        service_id: &request.service_id,
        action_id: &request.action_id,
        asset_status: status,
        connected: state.hub.is_connected(&request.asset).await,
        attempts: 0,
        in_cooldown: false,
        concurrent_operation: false,
    });
    sqlx::query("INSERT INTO audit_events(actor,action,resource_type,resource_id,metadata) VALUES('admin','RECOVERY_POLICY_PREVIEW','asset',$1,$2)").bind(&request.asset).bind(json!({"decision":decision,"action_id":request.action_id,"service_id":request.service_id,"executed":false})).execute(&state.pool).await.map_err(api_failure)?;
    Ok(Json(
        json!({"executed":false,"preview_only":true,"decision":decision}),
    ))
}

fn api_failure(error: sqlx::Error) -> (StatusCode, Json<Value>) {
    tracing::error!(%error, "recovery API database error");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({"error":"database operation failed"})),
    )
}

pub async fn handle_observation(
    pool: &PgPool,
    hub: &NodeHub,
    policy: &RecoveryPolicy,
    asset: &str,
    observation: ServiceObservation,
) -> anyhow::Result<()> {
    if observation.service_id.is_empty() || observation.service_id.len() > 200 {
        anyhow::bail!("invalid service observation target");
    }
    let summary = bounded(&observation.summary, 1024);
    let raw_evidence = bounded(&observation.evidence_json, MAX_EVIDENCE_BYTES);
    let evidence: Value =
        serde_json::from_str(&raw_evidence).unwrap_or_else(|_| json!({"unparsed":raw_evidence}));
    let asset_row: Option<(Uuid, String)> =
        sqlx::query_as("SELECT id,status FROM assets WHERE name=$1")
            .bind(asset)
            .fetch_optional(pool)
            .await?;
    let Some((asset_id, _asset_status)) = asset_row else {
        anyhow::bail!("observation references unknown asset");
    };

    // Preserve the bounded observation before incident processing. This gives
    // fleet views and future correlation a durable source of truth while the
    // incident state machine remains independently responsible for recovery.
    sqlx::query(
        "INSERT INTO service_observations(asset_id,service_id,healthy,summary,evidence) VALUES($1,$2,$3,$4,$5)",
    )
    .bind(asset_id)
    .bind(&observation.service_id)
    .bind(observation.healthy)
    .bind(&summary)
    .bind(&evidence)
    .execute(pool)
    .await?;

    let active: Option<ActiveIncident> = sqlx::query_as(
        "SELECT id,status,healthy_observations,verification_started_at,remediation_attempts FROM incidents WHERE asset_id=$1 AND service_id=$2 AND status NOT IN ('RESOLVED','ESCALATED','FAILED','CANCELLED') ORDER BY created_at DESC LIMIT 1",
    )
    .bind(asset_id)
    .bind(&observation.service_id)
    .fetch_optional(pool)
    .await?;

    if observation.healthy {
        sqlx::query("UPDATE incidents SET failure_latched=false WHERE asset_id=$1 AND service_id=$2 AND failure_latched=true AND status IN ('RESOLVED','ESCALATED','FAILED','CANCELLED')").bind(asset_id).bind(&observation.service_id).execute(pool).await?;
        if let Some((id, status, _, _, _)) = active {
            if status == "VERIFYING" {
                let healthy: i32 = sqlx::query_scalar("UPDATE incidents SET healthy_observations=healthy_observations+1,verification_result=jsonb_build_object('healthy',true,'summary',$2,'observed_at',now()),updated_at=now() WHERE id=$1 AND status='VERIFYING' RETURNING healthy_observations")
                    .bind(id).bind(&summary).fetch_one(pool).await?;
                event(
                    pool,
                    id,
                    "HEALTH_OBSERVED",
                    "node",
                    json!({"healthy":true,"summary":summary,"count":healthy}),
                )
                .await?;
                sqlx::query(
                    "UPDATE incidents SET healthy_since=COALESCE(healthy_since,now()) WHERE id=$1",
                )
                .bind(id)
                .execute(pool)
                .await?;
                let stable: bool = sqlx::query_scalar(
                    "SELECT healthy_since<=now()-interval '4 seconds' FROM incidents WHERE id=$1",
                )
                .bind(id)
                .fetch_one(pool)
                .await?;
                if healthy >= policy.required_healthy_observations && stable {
                    transition(pool,id,"VERIFYING","RESOLVED","recovery-orchestrator",json!({"message":"Recovery independently verified","healthy_observations":healthy})).await?;
                    sqlx::query(
                        "UPDATE incidents SET resolved_at=now(),updated_at=now() WHERE id=$1",
                    )
                    .bind(id)
                    .execute(pool)
                    .await?;
                    sqlx::query("UPDATE assets SET status='AVAILABLE',updated_at=now() WHERE id=$1 AND status='DEGRADED'").bind(asset_id).execute(pool).await?;
                }
            }
        }
        return Ok(());
    }

    sqlx::query(
        "UPDATE assets SET status='DEGRADED',updated_at=now() WHERE id=$1 AND status='AVAILABLE'",
    )
    .bind(asset_id)
    .execute(pool)
    .await?;
    if let Some((id, status, _, verifying_since, _)) = active {
        if status == "VERIFYING" {
            sqlx::query("UPDATE incidents SET healthy_since=NULL WHERE id=$1")
                .bind(id)
                .execute(pool)
                .await?;
            sqlx::query("UPDATE incidents SET healthy_observations=0,verification_result=jsonb_build_object('healthy',false,'summary',$2,'evidence',$3,'observed_at',now()),updated_at=now() WHERE id=$1")
                .bind(id).bind(&summary).bind(&evidence).execute(pool).await?;
            event(
                pool,
                id,
                "HEALTH_OBSERVED",
                "node",
                json!({"healthy":false,"summary":summary,"evidence":evidence}),
            )
            .await?;
            if verifying_since.is_some_and(|at| {
                Utc::now() - at > Duration::seconds(policy.verification_timeout_seconds)
            }) {
                sqlx::query("UPDATE incidents SET escalation_reason='service did not recover before verification deadline' WHERE id=$1").bind(id).execute(pool).await?;
                transition(
                    pool,
                    id,
                    "VERIFYING",
                    "ESCALATED",
                    "recovery-orchestrator",
                    json!({"reason":"verification deadline expired"}),
                )
                .await?;
            }
        }
        return Ok(());
    }

    let latched: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM incidents WHERE asset_id=$1 AND service_id=$2 AND failure_latched=true)").bind(asset_id).bind(&observation.service_id).fetch_one(pool).await?;
    if latched {
        return Ok(());
    }
    let incident: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO incidents(asset_id,service_id,incident_type,severity,status,diagnostic_evidence) VALUES($1,$2,'SERVICE_UNHEALTHY','HIGH','DETECTED',$3) ON CONFLICT DO NOTHING RETURNING id",
    )
    .bind(asset_id)
    .bind(&observation.service_id)
    .bind(json!({"summary":summary,"observation":evidence,"collected_at":Utc::now()}))
    .fetch_optional(pool)
    .await?;
    let Some(incident_id) = incident else {
        return Ok(());
    };
    event(
        pool,
        incident_id,
        "INCIDENT_DETECTED",
        "node",
        json!({"summary":summary,"evidence":evidence}),
    )
    .await?;
    transition(
        pool,
        incident_id,
        "DETECTED",
        "INVESTIGATING",
        "recovery-orchestrator",
        json!({"message":"Collecting bounded service evidence"}),
    )
    .await?;

    let context = json!({"summary":summary,"evidence":evidence,"asset":asset,"service_id":observation.service_id});
    event(
        pool,
        incident_id,
        "DIAGNOSIS_REQUEST",
        "orchestrator",
        context.clone(),
    )
    .await?;
    let diagnosis = match crate::recovery_model::diagnose(&context).await {
        Ok(value) => value,
        Err(error) => {
            sqlx::query("UPDATE incidents SET escalation_reason=$2 WHERE id=$1")
                .bind(incident_id)
                .bind(error.to_string())
                .execute(pool)
                .await?;
            transition(
                pool,
                incident_id,
                "INVESTIGATING",
                "ESCALATED",
                "diagnosis-provider",
                json!({"error":error.to_string()}),
            )
            .await?;
            return Ok(());
        }
    };
    sqlx::query("UPDATE incidents SET diagnosis=$2,updated_at=now() WHERE id=$1")
        .bind(incident_id)
        .bind(&diagnosis)
        .execute(pool)
        .await?;
    transition(
        pool,
        incident_id,
        "INVESTIGATING",
        "DIAGNOSED",
        "diagnosis-provider",
        json!({"diagnosis":diagnosis}),
    )
    .await?;
    if diagnosis["action_id"] != RESTART_SERVICE || diagnosis["escalation_recommended"] == true {
        sqlx::query(
            "UPDATE incidents SET escalation_reason='diagnosis recommends escalation' WHERE id=$1",
        )
        .bind(incident_id)
        .execute(pool)
        .await?;
        transition(
            pool,
            incident_id,
            "DIAGNOSED",
            "ESCALATED",
            "diagnosis-provider",
            json!({"diagnosis":diagnosis}),
        )
        .await?;
        return Ok(());
    }
    let proposal = json!({"action_id":RESTART_SERVICE,"asset":asset,"service_id":observation.service_id,"parameters":{}});
    sqlx::query("UPDATE incidents SET proposed_remediation=$2,updated_at=now() WHERE id=$1")
        .bind(incident_id)
        .bind(&proposal)
        .execute(pool)
        .await?;
    transition(
        pool,
        incident_id,
        "DIAGNOSED",
        "AUTHORIZATION_PENDING",
        "recovery-orchestrator",
        json!({"proposal":proposal}),
    )
    .await?;

    let recent: i64 = sqlx::query_scalar("SELECT count(*) FROM remediation_executions r JOIN incidents i ON i.id=r.incident_id WHERE i.asset_id=$1 AND r.service_id=$2 AND r.created_at > now()-make_interval(secs=>$3::int)")
        .bind(asset_id).bind(&observation.service_id).bind(policy.cooldown_seconds as i32).fetch_one(pool).await?;
    let concurrent: i64 = sqlx::query_scalar("SELECT count(*) FROM remediation_executions r JOIN incidents i ON i.id=r.incident_id WHERE i.asset_id=$1 AND r.status IN ('AUTHORIZED','DISPATCHED','ACKNOWLEDGED')")
        .bind(asset_id).fetch_one(pool).await?;
    let asset_status: String = sqlx::query_scalar("SELECT status FROM assets WHERE id=$1")
        .bind(asset_id)
        .fetch_one(pool)
        .await?;
    let decision = policy.evaluate(&PolicyRequest {
        asset,
        service_id: &observation.service_id,
        action_id: RESTART_SERVICE,
        asset_status: &asset_status,
        connected: hub.is_connected(asset).await,
        attempts: 0,
        in_cooldown: recent > 0,
        concurrent_operation: concurrent > 0,
    });
    event(
        pool,
        incident_id,
        "POLICY_EVALUATED",
        "policy-engine",
        json!({"decision":decision}),
    )
    .await?;
    if !decision.authorized {
        sqlx::query("UPDATE incidents SET escalation_reason=$2,updated_at=now() WHERE id=$1")
            .bind(incident_id)
            .bind(&decision.reason)
            .execute(pool)
            .await?;
        transition(
            pool,
            incident_id,
            "AUTHORIZATION_PENDING",
            "ESCALATED",
            "policy-engine",
            json!({"reason":decision.reason}),
        )
        .await?;
        return Ok(());
    }

    let operation_id = Uuid::new_v4();
    sqlx::query(
        "UPDATE incidents SET verification_deadline=now()+make_interval(secs=>$2::int) WHERE id=$1",
    )
    .bind(incident_id)
    .bind((policy.operation_deadline_seconds + policy.verification_timeout_seconds) as i32)
    .execute(pool)
    .await?;
    let idempotency_key = format!("{incident_id}:{RESTART_SERVICE}:1");
    let deadline = Utc::now() + Duration::seconds(policy.operation_deadline_seconds);
    sqlx::query("INSERT INTO remediation_executions(id,incident_id,action_id,service_id,status,idempotency_key,policy_decision,authorization_context,deadline) VALUES($1,$2,$3,$4,'AUTHORIZED',$5,$6,$7,$8)")
        .bind(operation_id).bind(incident_id).bind(RESTART_SERVICE).bind(&observation.service_id).bind(&idempotency_key)
        .bind(json!({"authorized":true,"reason":decision.reason})).bind(json!({"actor":"policy-engine","mode":"autonomous","asset":asset})).bind(deadline).execute(pool).await?;
    sqlx::query("UPDATE incidents SET remediation_attempts=remediation_attempts+1,updated_at=now() WHERE id=$1").bind(incident_id).execute(pool).await?;
    transition(
        pool,
        incident_id,
        "AUTHORIZATION_PENDING",
        "REMEDIATING",
        "policy-engine",
        json!({"operation_id":operation_id,"action_id":RESTART_SERVICE,"deadline":deadline}),
    )
    .await?;
    let message = ControlMessage {
        envelope: Some(Envelope {
            node_id: String::new(),
            asset_id: asset.into(),
            workflow_run_id: String::new(),
            timestamp_ms: Utc::now().timestamp_millis(),
        }),
        payload: Some(control_message::Payload::ExecuteRemediation(
            ExecuteRemediation {
                operation_id: operation_id.to_string(),
                incident_id: incident_id.to_string(),
                action_id: RESTART_SERVICE.into(),
                service_id: observation.service_id,
                deadline_ms: deadline.timestamp_millis(),
                idempotency_key,
            },
        )),
    };
    match hub.send(asset, message).await {
        Ok(()) => {
            sqlx::query("UPDATE remediation_executions SET status='DISPATCHED',updated_at=now() WHERE id=$1 AND status='AUTHORIZED'").bind(operation_id).execute(pool).await?;
            event(
                pool,
                incident_id,
                "REMEDIATION_DISPATCHED",
                "control-plane",
                json!({"operation_id":operation_id}),
            )
            .await?;
        }
        Err(error) => {
            sqlx::query("UPDATE remediation_executions SET status='FAILED',error=$2,completed_at=now(),updated_at=now() WHERE id=$1").bind(operation_id).bind(error).execute(pool).await?;
            sqlx::query("UPDATE incidents SET escalation_reason=$2 WHERE id=$1")
                .bind(incident_id)
                .bind(error)
                .execute(pool)
                .await?;
            transition(
                pool,
                incident_id,
                "REMEDIATING",
                "ESCALATED",
                "control-plane",
                json!({"reason":error}),
            )
            .await?;
        }
    }
    Ok(())
}

pub async fn handle_acknowledgement(
    pool: &PgPool,
    asset: &str,
    acknowledgement: RemediationAcknowledged,
) -> anyhow::Result<()> {
    let operation_id = Uuid::parse_str(&acknowledgement.operation_id)?;
    let incident_id: Option<Uuid> = sqlx::query_scalar("SELECT r.incident_id FROM remediation_executions r JOIN incidents i ON i.id=r.incident_id JOIN assets a ON a.id=i.asset_id WHERE r.id=$1 AND a.name=$2")
        .bind(operation_id).bind(asset).fetch_optional(pool).await?;
    let Some(incident_id) = incident_id else {
        anyhow::bail!("unknown remediation acknowledgement");
    };
    let status = if acknowledgement.accepted {
        "ACKNOWLEDGED"
    } else {
        "REJECTED"
    };
    sqlx::query("UPDATE remediation_executions SET status=$2,acknowledged_at=now(),error=NULLIF($3,''),updated_at=now() WHERE id=$1 AND status IN ('AUTHORIZED','DISPATCHED')")
        .bind(operation_id).bind(status).bind(&acknowledgement.reason).execute(pool).await?;
    event(pool,incident_id,"REMEDIATION_ACKNOWLEDGED","node",json!({"operation_id":operation_id,"accepted":acknowledgement.accepted,"reason":acknowledgement.reason})).await?;
    if !acknowledgement.accepted {
        sqlx::query("UPDATE incidents SET escalation_reason=$2 WHERE id=$1")
            .bind(incident_id)
            .bind(&acknowledgement.reason)
            .execute(pool)
            .await?;
        transition(
            pool,
            incident_id,
            "REMEDIATING",
            "ESCALATED",
            "node",
            json!({"reason":acknowledgement.reason}),
        )
        .await?;
    }
    Ok(())
}

pub async fn handle_completion(
    pool: &PgPool,
    asset: &str,
    completion: RemediationCompleted,
) -> anyhow::Result<()> {
    let operation_id = Uuid::parse_str(&completion.operation_id)?;
    let incident_id: Option<Uuid> = sqlx::query_scalar("SELECT r.incident_id FROM remediation_executions r JOIN incidents i ON i.id=r.incident_id JOIN assets a ON a.id=i.asset_id WHERE r.id=$1 AND a.name=$2")
        .bind(operation_id).bind(asset).fetch_optional(pool).await?;
    let Some(incident_id) = incident_id else {
        anyhow::bail!("unknown remediation completion");
    };
    let valid: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM remediation_executions r JOIN incidents i ON i.id=r.incident_id WHERE r.id=$1 AND r.status='ACKNOWLEDGED' AND i.status='REMEDIATING' AND r.deadline>now())").bind(operation_id).fetch_one(pool).await?;
    if !valid {
        event(
            pool,
            incident_id,
            "LATE_OR_DUPLICATE_RESULT",
            "node",
            json!({"operation_id":operation_id,"ignored":true}),
        )
        .await?;
        return Ok(());
    }
    let output = bounded(&completion.output, 4096);
    let error = bounded(&completion.error, 2048);
    sqlx::query("UPDATE remediation_executions SET status=$2,output=$3,error=NULLIF($4,''),completed_at=now(),updated_at=now() WHERE id=$1 AND status IN ('AUTHORIZED','DISPATCHED','ACKNOWLEDGED')")
        .bind(operation_id).bind(if completion.success{"EXECUTED"}else{"FAILED"}).bind(&output).bind(&error).execute(pool).await?;
    event(pool,incident_id,"REMEDIATION_COMPLETED","node",json!({"operation_id":operation_id,"success":completion.success,"output":output,"error":error})).await?;
    if completion.success {
        sqlx::query("UPDATE incidents SET verification_started_at=now(),healthy_observations=0,verification_result=jsonb_build_object('healthy',false,'status','stabilizing'),updated_at=now() WHERE id=$1").bind(incident_id).execute(pool).await?;
        transition(
            pool,
            incident_id,
            "REMEDIATING",
            "VERIFYING",
            "recovery-orchestrator",
            json!({"message":"Repair executed; awaiting independent health observations"}),
        )
        .await?;
    } else {
        sqlx::query("UPDATE incidents SET escalation_reason=$2 WHERE id=$1")
            .bind(incident_id)
            .bind(&error)
            .execute(pool)
            .await?;
        transition(
            pool,
            incident_id,
            "REMEDIATING",
            "ESCALATED",
            "recovery-orchestrator",
            json!({"reason":error}),
        )
        .await?;
    }
    Ok(())
}

async fn transition(
    pool: &PgPool,
    incident_id: Uuid,
    from: &str,
    to: &str,
    actor: &str,
    data: Value,
) -> anyhow::Result<()> {
    if !valid_transition(from, to) {
        anyhow::bail!("invalid incident transition {from} -> {to}");
    }
    let mut transaction = pool.begin().await?;
    let changed =
        sqlx::query("UPDATE incidents SET status=$3,updated_at=now() WHERE id=$1 AND status=$2")
            .bind(incident_id)
            .bind(from)
            .bind(to)
            .execute(&mut *transaction)
            .await?
            .rows_affected();
    if changed != 1 {
        anyhow::bail!("incident state changed while applying {from} -> {to}");
    }
    sqlx::query("INSERT INTO incident_events(incident_id,kind,actor,data,correlation_id) SELECT id,'STATUS_CHANGED',$2,$3,correlation_id FROM incidents WHERE id=$1").bind(incident_id).bind(actor).bind(json!({"from":from,"to":to,"detail":data})).execute(&mut *transaction).await?;
    transaction.commit().await?;
    Ok(())
}

async fn event(
    pool: &PgPool,
    incident_id: Uuid,
    kind: &str,
    actor: &str,
    data: Value,
) -> anyhow::Result<()> {
    sqlx::query("INSERT INTO incident_events(incident_id,kind,actor,data,correlation_id) SELECT id,$2,$3,$4,correlation_id FROM incidents WHERE id=$1")
        .bind(incident_id).bind(kind).bind(actor).bind(data).execute(pool).await?;
    Ok(())
}

fn bounded(value: &str, bytes: usize) -> String {
    let mut end = value.len().min(bytes);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[sqlx::test(migrations = "../../migrations")]
    async fn failed_and_inconsistent_results_never_resolve_incident(pool: PgPool) {
        let asset: Uuid = sqlx::query_scalar(
            "INSERT INTO assets(name,status) VALUES('failed-device','AVAILABLE') RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        for acknowledged in [false, true] {
            let id:Uuid=sqlx::query_scalar("INSERT INTO incidents(asset_id,service_id,incident_type,severity,status) VALUES($1,$2,'SERVICE_UNHEALTHY','HIGH','REMEDIATING') RETURNING id").bind(asset).bind(acknowledged.to_string()).fetch_one(&pool).await.unwrap();
            let op = Uuid::new_v4();
            sqlx::query("INSERT INTO remediation_executions(id,incident_id,action_id,service_id,status,idempotency_key,policy_decision,authorization_context,deadline) VALUES($1,$2,'RESTART_SERVICE','svc',$3,$4,'{}','{}',now()+interval '30 seconds')").bind(op).bind(id).bind(if acknowledged {"ACKNOWLEDGED"} else {"DISPATCHED"}).bind(op.to_string()).execute(&pool).await.unwrap();
            handle_completion(
                &pool,
                "failed-device",
                RemediationCompleted {
                    operation_id: op.to_string(),
                    success: !acknowledged,
                    output: String::new(),
                    error: "test failure".into(),
                },
            )
            .await
            .unwrap();
            let status: String = sqlx::query_scalar("SELECT status FROM incidents WHERE id=$1")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap();
            assert_eq!(
                status,
                if acknowledged {
                    "ESCALATED"
                } else {
                    "REMEDIATING"
                }
            );
        }
        disconnect(&pool, "failed-device").await.unwrap();
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM incidents WHERE status NOT IN ('ESCALATED')")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(count, 0);
    }
    #[sqlx::test(migrations = "../../migrations")]
    async fn deadlines_and_restart_escalate_without_replay(pool: PgPool) {
        let asset: Uuid = sqlx::query_scalar(
            "INSERT INTO assets(name,status) VALUES('deadline-device','AVAILABLE') RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let id:Uuid=sqlx::query_scalar("INSERT INTO incidents(asset_id,service_id,incident_type,severity,status,verification_deadline) VALUES($1,'svc','SERVICE_UNHEALTHY','HIGH','VERIFYING',now()-interval '1 second') RETURNING id").bind(asset).fetch_one(&pool).await.unwrap();
        reconcile(&pool).await.unwrap();
        let status: String = sqlx::query_scalar("SELECT status FROM incidents WHERE id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(status, "ESCALATED");
        sqlx::query("INSERT INTO incidents(asset_id,service_id,incident_type,severity,status) VALUES($1,'other','SERVICE_UNHEALTHY','HIGH','REMEDIATING')").bind(asset).execute(&pool).await.unwrap();
        start_reconciler(pool.clone()).await.unwrap();
        let active: i64 =
            sqlx::query_scalar("SELECT count(*) FROM incidents WHERE status='REMEDIATING'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(active, 0);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn unhealthy_samples_are_latched_and_policy_denial_is_audited(pool: PgPool) {
        sqlx::query("INSERT INTO assets(name,status) VALUES('test-device','AVAILABLE')")
            .execute(&pool)
            .await
            .unwrap();
        let hub = NodeHub::default();
        let observation = || ServiceObservation {
            service_id: "torch-demo".into(),
            healthy: false,
            summary: "HTTP 503".into(),
            evidence_json: "{}".into(),
        };
        handle_observation(&pool, &hub, &enabled(), "test-device", observation())
            .await
            .unwrap();
        handle_observation(&pool, &hub, &enabled(), "test-device", observation())
            .await
            .unwrap();
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM incidents")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 1);
        let status: String = sqlx::query_scalar("SELECT status FROM incidents")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(status, "ESCALATED");
        let observations: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM service_observations WHERE service_id='torch-demo'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(observations, 2);
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM incident_events WHERE kind='POLICY_EVALUATED'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 1);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn execution_success_requires_health_and_duplicate_results_do_not_reset_verification(
        pool: PgPool,
    ) {
        let asset: Uuid = sqlx::query_scalar(
            "INSERT INTO assets(name,status) VALUES('test-device','DEGRADED') RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let id:Uuid=sqlx::query_scalar("INSERT INTO incidents(asset_id,service_id,incident_type,severity,status) VALUES($1,'torch-demo','SERVICE_UNHEALTHY','HIGH','REMEDIATING') RETURNING id").bind(asset).fetch_one(&pool).await.unwrap();
        let op = Uuid::new_v4();
        sqlx::query("INSERT INTO remediation_executions(id,incident_id,action_id,service_id,status,idempotency_key,policy_decision,authorization_context,deadline) VALUES($1,$2,'RESTART_SERVICE','torch-demo','ACKNOWLEDGED','key','{}','{}',now()+interval '30 seconds')").bind(op).bind(id).execute(&pool).await.unwrap();
        let result = || RemediationCompleted {
            operation_id: op.to_string(),
            success: true,
            output: "restarted".into(),
            error: String::new(),
        };
        handle_completion(&pool, "test-device", result())
            .await
            .unwrap();
        handle_completion(&pool, "test-device", result())
            .await
            .unwrap();
        let status: String = sqlx::query_scalar("SELECT status FROM incidents WHERE id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(status, "VERIFYING");
        sqlx::query("UPDATE incidents SET healthy_since=now()-interval '5 seconds' WHERE id=$1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        for _ in 0..3 {
            handle_observation(
                &pool,
                &NodeHub::default(),
                &enabled(),
                "test-device",
                ServiceObservation {
                    service_id: "torch-demo".into(),
                    healthy: true,
                    summary: "HTTP 200".into(),
                    evidence_json: "{}".into(),
                },
            )
            .await
            .unwrap();
        }
        let status: String = sqlx::query_scalar("SELECT status FROM incidents WHERE id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(status, "RESOLVED");
    }

    fn enabled() -> RecoveryPolicy {
        RecoveryPolicy {
            enabled: true,
            allowed_asset: "test-device".into(),
            allowed_service: "torch-demo".into(),
            max_attempts: 1,
            cooldown_seconds: 60,
            operation_deadline_seconds: 30,
            verification_timeout_seconds: 30,
            required_healthy_observations: 3,
        }
    }
    fn request<'a>(action: &'a str) -> PolicyRequest<'a> {
        PolicyRequest {
            asset: "test-device",
            service_id: "torch-demo",
            action_id: action,
            asset_status: "DEGRADED",
            connected: true,
            attempts: 0,
            in_cooldown: false,
            concurrent_operation: false,
        }
    }
    #[test]
    fn incident_state_machine_rejects_skips() {
        assert!(valid_transition("DETECTED", "INVESTIGATING"));
        assert!(valid_transition("VERIFYING", "RESOLVED"));
        assert!(!valid_transition("DETECTED", "RESOLVED"));
        assert!(!valid_transition("RESOLVED", "REMEDIATING"));
    }
    #[test]
    fn policy_allows_only_exact_restart_target() {
        assert!(enabled().evaluate(&request(RESTART_SERVICE)).authorized);
        assert!(!enabled().evaluate(&request("RUN_COMMAND")).authorized);
        let mut wrong = enabled();
        wrong.allowed_service = "other".into();
        assert!(!wrong.evaluate(&request(RESTART_SERVICE)).authorized);
    }
    #[test]
    fn policy_enforces_disable_retry_cooldown_and_concurrency() {
        let mut policy = enabled();
        policy.enabled = false;
        assert!(!policy.evaluate(&request(RESTART_SERVICE)).authorized);
        let policy = enabled();
        let mut retry = request(RESTART_SERVICE);
        retry.attempts = 1;
        assert!(!policy.evaluate(&retry).authorized);
        let mut cooldown = request(RESTART_SERVICE);
        cooldown.in_cooldown = true;
        assert!(!policy.evaluate(&cooldown).authorized);
        let mut concurrent = request(RESTART_SERVICE);
        concurrent.concurrent_operation = true;
        assert!(!policy.evaluate(&concurrent).authorized);
    }
}
