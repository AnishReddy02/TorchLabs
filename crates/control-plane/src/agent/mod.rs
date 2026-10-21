mod model;
mod runtime;
#[cfg(test)]
mod tests;
mod tools;

use crate::api::AppState;
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

pub use runtime::start_worker;
type ApiResult<T> = Result<T, (StatusCode, Json<Value>)>;
fn failure(error: impl std::fmt::Display) -> (StatusCode, Json<Value>) {
    tracing::warn!(%error, "agent API request failed");
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error":error.to_string()})),
    )
}
fn auth(s: &AppState, headers: &HeaderMap) -> ApiResult<()> {
    let token = headers
        .get("authorization")
        .and_then(|x| x.to_str().ok())
        .and_then(|x| x.strip_prefix("Bearer "));
    if token.is_some_and(|t| bench_common::hash_token(t) == s.admin_token_hash) {
        Ok(())
    } else {
        Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"A valid API token is required"})),
        ))
    }
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/agent/sessions", get(list).post(create))
        .route("/api/agent/sessions/{id}", get(detail))
        .route("/api/agent/sessions/{id}/approval", post(decide))
        .route("/api/agent/sessions/{id}/pause", post(pause))
        .route("/api/agent/sessions/{id}/resume", post(resume))
        .route("/api/agent/sessions/{id}/instructions", post(instruct))
        .route("/api/agent/sessions/{id}/cancel", post(cancel))
        .route("/api/agent/workflows/{id}/approve", post(approve_workflow))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Create {
    prompt: String,
    #[serde(default)]
    target_asset: Option<String>,
    #[serde(default = "default_runtime")]
    max_runtime_minutes: i64,
    #[serde(default = "default_true")]
    stop_after_failure: bool,
}
fn default_runtime() -> i64 {
    20
}
fn default_true() -> bool {
    true
}
async fn create(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(req): Json<Create>,
) -> ApiResult<Json<Value>> {
    auth(&s, &h)?;
    model::OpenAiModel::from_env().map_err(failure)?;
    if req.prompt.trim().is_empty() || req.prompt.len() > 4000 {
        return Err(failure("Prompt must contain 1–4000 bytes"));
    }
    if !(5..=60).contains(&req.max_runtime_minutes) {
        return Err(failure("Maximum runtime must be between 5 and 60 minutes"));
    }
    let target = req
        .target_asset
        .as_deref()
        .map(str::trim)
        .filter(|x| !x.is_empty());
    if target.is_some_and(|x| x.len() > 100) {
        return Err(failure("Target asset must be at most 100 bytes"));
    }
    let constraints = json!({
        "target_asset": target,
        "max_runtime_minutes": req.max_runtime_minutes,
        "stop_after_failure": req.stop_after_failure,
        "software_changes_require_approval": true,
        "arbitrary_shell": "denied"
    });
    let context = json!([{"action":{"tool":"operator_constraints"},"result":constraints}]);
    let id: Uuid = sqlx::query_scalar("INSERT INTO agent_sessions(prompt,context,constraints,deadline) VALUES($1,$2,$3,now()+make_interval(mins=>$4::int)) RETURNING id")
        .bind(req.prompt)
        .bind(context)
        .bind(&constraints)
        .bind(req.max_runtime_minutes as i32)
        .fetch_one(&s.pool)
        .await
        .map_err(failure)?;
    event(&s.pool, id, "STATUS", json!({"message":"Request queued"}))
        .await
        .map_err(failure)?;
    Ok(Json(json!({"id":id})))
}
async fn list(State(s): State<AppState>, h: HeaderMap) -> ApiResult<Json<Value>> {
    auth(&s, &h)?;
    let rows: Vec<Value> = sqlx::query_scalar("SELECT jsonb_build_object('id',id,'prompt',prompt,'status',status,'paused',paused,'created_at',created_at) FROM agent_sessions ORDER BY created_at DESC LIMIT 50").fetch_all(&s.pool).await.map_err(failure)?;
    Ok(Json(json!(rows)))
}
async fn detail(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    auth(&s, &h)?;
    let mut row: Value =
        sqlx::query_scalar("SELECT to_jsonb(s)-'context' FROM agent_sessions s WHERE id=$1")
            .bind(id)
            .fetch_optional(&s.pool)
            .await
            .map_err(failure)?
            .ok_or_else(|| failure("Agent session not found"))?;
    let events: Vec<Value> = sqlx::query_scalar(
        "SELECT to_jsonb(e) FROM agent_events e WHERE session_id=$1 ORDER BY id",
    )
    .bind(id)
    .fetch_all(&s.pool)
    .await
    .map_err(failure)?;
    row["events"] = json!(events);
    Ok(Json(row))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Decision {
    approved: bool,
    request_id: Uuid,
}
async fn decide(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(d): Json<Decision>,
) -> ApiResult<Json<Value>> {
    auth(&s, &h)?;
    let changed = sqlx::query("UPDATE agent_sessions SET decision=$2,updated_at=now() WHERE id=$1 AND status='WAITING_FOR_APPROVAL' AND decision IS NULL AND NOT cancel_requested AND deadline>now() AND pending->>'request_id'=$3")
        .bind(id).bind(d.approved).bind(d.request_id.to_string()).execute(&s.pool).await.map_err(failure)?.rows_affected();
    if changed != 1 {
        return Err(failure(
            "Approval expired, already decided, or does not match this request",
        ));
    }
    event(
        &s.pool,
        id,
        if d.approved {
            "APPROVAL_GRANTED"
        } else {
            "APPROVAL_DENIED"
        },
        json!({"request_id":d.request_id,"actor":"authenticated admin"}),
    )
    .await
    .map_err(failure)?;
    Ok(Json(json!({"accepted":true})))
}
async fn cancel(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    auth(&s, &h)?;
    sqlx::query("UPDATE agent_sessions SET cancel_requested=true WHERE id=$1 AND status NOT IN ('COMPLETED','FAILED','CANCELLED')").bind(id).execute(&s.pool).await.map_err(failure)?;
    Ok(Json(json!({"cancellation_requested":true})))
}

async fn pause(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    auth(&s, &h)?;
    let changed = sqlx::query("UPDATE agent_sessions SET paused=true,control_version=control_version+1,updated_at=now() WHERE id=$1 AND status IN ('CREATED','RUNNING','WAITING_FOR_APPROVAL') AND NOT paused")
        .bind(id).execute(&s.pool).await.map_err(failure)?.rows_affected();
    if changed != 1 {
        return Err(failure("Session is already paused or cannot be paused"));
    }
    event(&s.pool,id,"PAUSED",json!({"message":"Operator paused agent decisions. Any workflow already running on the node continues."})).await.map_err(failure)?;
    Ok(Json(json!({"paused":true})))
}

async fn resume(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    auth(&s, &h)?;
    let changed = sqlx::query("UPDATE agent_sessions SET paused=false,control_version=control_version+1,updated_at=now() WHERE id=$1 AND status IN ('CREATED','RUNNING','WAITING_FOR_APPROVAL') AND paused")
        .bind(id).execute(&s.pool).await.map_err(failure)?.rows_affected();
    if changed != 1 {
        return Err(failure("Session is not paused or cannot be resumed"));
    }
    event(
        &s.pool,
        id,
        "RESUMED",
        json!({"message":"Operator resumed agent decisions."}),
    )
    .await
    .map_err(failure)?;
    Ok(Json(json!({"paused":false})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Instruction {
    message: String,
}
async fn instruct(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(req): Json<Instruction>,
) -> ApiResult<Json<Value>> {
    auth(&s, &h)?;
    let message = req.message.trim();
    if message.is_empty() || message.len() > 1000 {
        return Err(failure("Instruction must contain 1–1000 bytes"));
    }
    let observation =
        json!({"action":{"tool":"operator_instruction"},"result":{"message":message}});
    let changed = sqlx::query("UPDATE agent_sessions SET context=context || jsonb_build_array($2::jsonb),control_version=control_version+1,updated_at=now() WHERE id=$1 AND status IN ('CREATED','RUNNING')")
        .bind(id).bind(&observation).execute(&s.pool).await.map_err(failure)?.rows_affected();
    if changed != 1 {
        return Err(failure(
            "Instructions can be sent only while the agent is active and not awaiting approval",
        ));
    }
    event(
        &s.pool,
        id,
        "OPERATOR_INSTRUCTION",
        json!({"message":message}),
    )
    .await
    .map_err(failure)?;
    Ok(Json(json!({"accepted":true})))
}
async fn approve_workflow(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    auth(&s, &h)?;
    let yaml: String = sqlx::query_scalar("SELECT definition_yaml FROM workflows WHERE id=$1")
        .bind(id)
        .fetch_optional(&s.pool)
        .await
        .map_err(failure)?
        .ok_or_else(|| failure("Workflow not found"))?;
    let hash = bench_common::hash_token(&yaml);
    sqlx::query("UPDATE workflows SET agent_approved_hash=$2 WHERE id=$1 AND definition_yaml=$3")
        .bind(id)
        .bind(&hash)
        .bind(yaml)
        .execute(&s.pool)
        .await
        .map_err(failure)?;
    sqlx::query("INSERT INTO audit_events(actor,action,resource_type,resource_id,metadata) VALUES('admin','agent.workflow_approved','workflow',$1,$2)").bind(id.to_string()).bind(json!({"hash":hash})).execute(&s.pool).await.map_err(failure)?;
    Ok(Json(json!({"approved_hash":hash})))
}
pub async fn event(pool: &PgPool, id: Uuid, kind: &str, data: Value) -> anyhow::Result<()> {
    sqlx::query("INSERT INTO agent_events(session_id,kind,data) VALUES($1,$2,$3)")
        .bind(id)
        .bind(kind)
        .bind(data)
        .execute(pool)
        .await?;
    tracing::info!(session_id=%id,kind,"agent event");
    Ok(())
}
