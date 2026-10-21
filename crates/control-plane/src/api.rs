use crate::grpc::NodeHub;
use crate::recovery::RecoveryPolicy;
use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, Multipart, Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use bench_common::WorkflowDefinition;
use bench_protocol::bench::{
    control_message, CancelWorkflow, ControlMessage, Envelope, ReservationChanged, RunWorkflow,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{FromRow, PgPool};
use std::{path::PathBuf, sync::Arc};
use uuid::Uuid;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub hub: Arc<NodeHub>,
    pub bundle_dir: Arc<PathBuf>,
    pub bundle_base_url: String,
    pub artifact_dir: Arc<PathBuf>,
    pub admin_token_hash: String,
    pub recovery_policy: Arc<RecoveryPolicy>,
}

pub fn router() -> Router<AppState> {
    let routes = Router::new()
        .route("/health", get(health))
        .route("/fleet/overview", get(fleet_overview))
        .route("/telemetry/{asset}", get(asset_telemetry))
        .route("/install.sh", get(install_script))
        .route("/install/bench-node.service", get(systemd_service))
        .route("/nodes/{asset}/health", get(node_health))
        .route("/assets", get(list_assets))
        .route("/scheduler/reserve", post(schedule_reservation))
        .route("/assets/{id}", get(get_asset))
        .route("/assets/{id}/reserve", post(reserve))
        .route("/assets/{id}/release", post(release))
        .route("/reservations", get(list_reservations))
        .route("/reservations/{id}", get(get_reservation))
        .route("/reservations/{id}/release", post(release_reservation))
        .route("/workflows", get(list_workflows).post(create_workflow))
        .route(
            "/workflows/{id}",
            get(get_workflow)
                .put(update_workflow)
                .delete(delete_workflow),
        )
        .route(
            "/bundles",
            get(list_bundles)
                .post(upload_bundle)
                .layer(DefaultBodyLimit::max(100 * 1024 * 1024)),
        )
        .route("/bundles/{id}/download", get(download_bundle))
        .route("/runs", get(list_runs).post(create_run))
        .route("/runs/{id}", get(get_run))
        .route("/runs/{id}/cancel", post(cancel_run))
        .route("/runs/{id}/rerun", post(rerun))
        .route("/runs/{id}/logs", get(get_logs))
        .route("/runs/{id}/artifacts", get(list_artifacts))
        .route("/artifacts/{id}/download", get(download_artifact))
        .route("/nodes", get(list_nodes))
        .route("/nodes/{id}", get(get_node));
    routes.clone().nest("/api", routes)
}

fn authorize(headers: &HeaderMap, state: &AppState) -> ApiResult<()> {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    if token.is_some_and(|value| bench_common::hash_token(value) == state.admin_token_hash) {
        Ok(())
    } else {
        Err(ApiError {
            status: StatusCode::UNAUTHORIZED,
            message: "missing or invalid Bearer API token".into(),
        })
    }
}

async fn audit(
    state: &AppState,
    action: &str,
    resource_type: &str,
    resource_id: impl ToString,
    metadata: serde_json::Value,
) {
    if let Err(error) = sqlx::query("INSERT INTO audit_events(actor,action,resource_type,resource_id,metadata) VALUES('admin',$1,$2,$3,$4)").bind(action).bind(resource_type).bind(resource_id.to_string()).bind(metadata).execute(&state.pool).await { tracing::error!(%error, "failed to persist audit event"); }
}

#[derive(Serialize, FromRow)]
struct Node {
    id: Uuid,
    asset_key: String,
    name: Option<String>,
    version: Option<String>,
    hostname: Option<String>,
    os: Option<String>,
    kernel: Option<String>,
    architecture: Option<String>,
    cpu_count: Option<i32>,
    memory_bytes: Option<i64>,
    connected: bool,
    state: String,
    last_seen_at: DateTime<Utc>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}
async fn list_nodes(State(s): State<AppState>) -> ApiResult<Json<Vec<Node>>> {
    Ok(Json(
        sqlx::query_as("SELECT * FROM nodes ORDER BY created_at DESC")
            .fetch_all(&s.pool)
            .await?,
    ))
}
async fn get_node(State(s): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<Json<Node>> {
    Ok(Json(
        sqlx::query_as("SELECT * FROM nodes WHERE id=$1")
            .bind(id)
            .fetch_optional(&s.pool)
            .await?
            .ok_or(ApiError::not_found("node"))?,
    ))
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({"status":"ok"}))
}

async fn fleet_overview(State(s): State<AppState>) -> ApiResult<Json<serde_json::Value>> {
    let total_assets: i64 = sqlx::query_scalar("SELECT count(*) FROM assets")
        .fetch_one(&s.pool)
        .await?;
    let online_assets: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM assets WHERE status <> 'OFFLINE' AND last_seen_at > now() - interval '30 seconds'",
    )
    .fetch_one(&s.pool)
    .await?;
    let degraded_assets: i64 =
        sqlx::query_scalar("SELECT count(*) FROM assets WHERE status = 'DEGRADED'")
            .fetch_one(&s.pool)
            .await?;
    let active_incidents: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM incidents WHERE status NOT IN ('RESOLVED','ESCALATED','FAILED','CANCELLED')",
    )
    .fetch_one(&s.pool)
    .await?;
    let recent_observations: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM service_observations WHERE observed_at > now() - interval '5 minutes'",
    )
    .fetch_one(&s.pool)
    .await?;
    Ok(Json(serde_json::json!({
        "total_assets": total_assets,
        "online_assets": online_assets,
        "degraded_assets": degraded_assets,
        "active_incidents": active_incidents,
        "recent_observations": recent_observations,
    })))
}

#[derive(Serialize, FromRow)]
struct TelemetryObservation {
    id: Uuid,
    asset_id: Uuid,
    service_id: String,
    healthy: bool,
    summary: String,
    evidence: serde_json::Value,
    observed_at: DateTime<Utc>,
    correlation_id: Uuid,
}

#[derive(Deserialize)]
struct TelemetryQuery {
    limit: Option<i64>,
}

async fn asset_telemetry(
    State(s): State<AppState>,
    Path(asset): Path<String>,
    Query(query): Query<TelemetryQuery>,
) -> ApiResult<Json<Vec<TelemetryObservation>>> {
    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    Ok(Json(
        sqlx::query_as(
            "SELECT o.id,o.asset_id,o.service_id,o.healthy,o.summary,o.evidence,o.observed_at,o.correlation_id FROM service_observations o JOIN assets a ON a.id=o.asset_id WHERE a.name=$1 ORDER BY o.observed_at DESC LIMIT $2",
        )
        .bind(asset)
        .bind(limit)
        .fetch_all(&s.pool)
        .await?,
    ))
}
async fn install_script() -> ([(header::HeaderName, &'static str); 1], &'static str) {
    (
        [(header::CONTENT_TYPE, "text/x-shellscript")],
        include_str!("../../../scripts/install-node.sh"),
    )
}
async fn systemd_service() -> ([(header::HeaderName, &'static str); 1], &'static str) {
    (
        [(header::CONTENT_TYPE, "text/plain")],
        include_str!("../../../systemd/bench-node.service"),
    )
}

async fn node_health(
    State(s): State<AppState>,
    Path(asset): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    if s.hub.is_connected(&asset).await {
        Ok(Json(
            serde_json::json!({"status":"connected","asset_id":asset}),
        ))
    } else {
        Err(ApiError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            message: format!("node for asset '{asset}' is not connected"),
        })
    }
}

#[derive(Debug, Serialize, FromRow)]
struct Asset {
    id: Uuid,
    name: String,
    node_id: Option<Uuid>,
    status: String,
    hostname: Option<String>,
    os: Option<String>,
    architecture: Option<String>,
    metadata: serde_json::Value,
    description: String,
    hardware_type: String,
    capabilities: serde_json::Value,
    labels: serde_json::Value,
    last_seen_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}
#[derive(Deserialize, Default)]
struct AssetFilter {
    status: Option<String>,
    capability: Option<String>,
    label: Option<String>,
}
async fn list_assets(
    State(s): State<AppState>,
    Query(filter): Query<AssetFilter>,
) -> ApiResult<Json<Vec<Asset>>> {
    let mut assets: Vec<Asset> = sqlx::query_as("SELECT * FROM assets ORDER BY name")
        .fetch_all(&s.pool)
        .await?;
    assets.retain(|asset| {
        filter
            .status
            .as_ref()
            .is_none_or(|status| asset.status.eq_ignore_ascii_case(status))
            && filter
                .capability
                .as_ref()
                .is_none_or(|value| json_filter(&asset.capabilities, value))
            && filter
                .label
                .as_ref()
                .is_none_or(|value| json_filter(&asset.labels, value))
    });
    Ok(Json(assets))
}
fn json_filter(value: &serde_json::Value, expression: &str) -> bool {
    let (key, expected) = expression.split_once('=').unwrap_or((expression, "true"));
    value.get(key).is_some_and(|actual| {
        actual == expected
            || actual.as_str() == Some(expected)
            || (expected == "true" && actual == true)
            || (expected == "false" && actual == false)
    })
}
async fn get_asset(State(s): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<Json<Asset>> {
    Ok(Json(
        sqlx::query_as("SELECT * FROM assets WHERE id=$1")
            .bind(id)
            .fetch_optional(&s.pool)
            .await?
            .ok_or(ApiError::not_found("asset"))?,
    ))
}

#[derive(Deserialize)]
struct ReserveRequest {
    owner: String,
    #[serde(default = "default_reservation_minutes")]
    duration_minutes: i64,
}
fn default_reservation_minutes() -> i64 {
    60
}
#[derive(Debug, Serialize, FromRow)]
struct Reservation {
    id: Uuid,
    asset_id: Uuid,
    owner: String,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    status: String,
    released_at: Option<DateTime<Utc>>,
}
#[derive(Serialize)]
struct ReservationCreated {
    #[serde(flatten)]
    reservation: Reservation,
    token: String,
}
async fn reserve(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(req): Json<ReserveRequest>,
) -> ApiResult<Json<ReservationCreated>> {
    authorize(&headers, &s)?;
    if req.owner.trim().is_empty() || !(1..=1440).contains(&req.duration_minutes) {
        return Err(ApiError::bad_request(
            "owner is required and duration_minutes must be 1..1440",
        ));
    }
    expire_reservations(&s).await?;
    let mut tx = s.pool.begin().await?;
    let asset: Asset = sqlx::query_as("SELECT * FROM assets WHERE id=$1 FOR UPDATE")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ApiError::not_found("asset"))?;
    if asset.status != "AVAILABLE" {
        return Err(ApiError::conflict(format!("asset is {}", asset.status)));
    }
    let token = Uuid::new_v4().to_string();
    let result: Reservation=sqlx::query_as("INSERT INTO reservations(asset_id,owner,token_hash,expires_at) VALUES($1,$2,$3,now()+make_interval(mins=>$4)) RETURNING *").bind(id).bind(req.owner.trim()).bind(bench_common::hash_token(&token)).bind(req.duration_minutes as i32).fetch_one(&mut *tx).await.map_err(ApiError::from_db)?;
    sqlx::query("UPDATE assets SET status='RESERVED',updated_at=now() WHERE id=$1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    let _ = s
        .hub
        .send(
            &asset.name,
            control(
                &asset.name,
                "",
                control_message::Payload::ReservationChanged(ReservationChanged {
                    reserved: true,
                    reservation_id: result.id.to_string(),
                }),
            ),
        )
        .await;
    audit(
        &s,
        "reservation.created",
        "reservation",
        result.id,
        serde_json::json!({"asset_id":id,"owner":result.owner}),
    )
    .await;
    Ok(Json(ReservationCreated {
        reservation: result,
        token,
    }))
}
#[derive(Deserialize)]
struct ScheduleRequest {
    owner: String,
    #[serde(default)]
    requires: serde_json::Value,
    #[serde(default = "default_reservation_minutes")]
    duration_minutes: i64,
}
async fn schedule_reservation(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ScheduleRequest>,
) -> ApiResult<Json<ReservationCreated>> {
    authorize(&headers, &s)?;
    if req.owner.trim().is_empty()
        || !(1..=1440).contains(&req.duration_minutes)
        || !req.requires.is_object()
    {
        return Err(ApiError::bad_request(
            "owner, duration 1..1440, and an object requires field are required",
        ));
    }
    let mut tx = s.pool.begin().await?;
    let asset:Asset=sqlx::query_as("SELECT * FROM assets WHERE status='AVAILABLE' AND capabilities @> $1 ORDER BY last_seen_at DESC NULLS LAST FOR UPDATE SKIP LOCKED LIMIT 1").bind(&req.requires).fetch_optional(&mut *tx).await?.ok_or(ApiError::conflict("no available asset matches the required capabilities"))?;
    let token = Uuid::new_v4().to_string();
    let reservation:Reservation=sqlx::query_as("INSERT INTO reservations(asset_id,owner,token_hash,expires_at) VALUES($1,$2,$3,now()+make_interval(mins=>$4)) RETURNING *").bind(asset.id).bind(req.owner.trim()).bind(bench_common::hash_token(&token)).bind(req.duration_minutes as i32).fetch_one(&mut *tx).await.map_err(ApiError::from_db)?;
    sqlx::query("UPDATE assets SET status='RESERVED',updated_at=now() WHERE id=$1")
        .bind(asset.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    let _ = s
        .hub
        .send(
            &asset.name,
            control(
                &asset.name,
                "",
                control_message::Payload::ReservationChanged(ReservationChanged {
                    reserved: true,
                    reservation_id: reservation.id.to_string(),
                }),
            ),
        )
        .await;
    audit(
        &s,
        "reservation.scheduled",
        "reservation",
        reservation.id,
        serde_json::json!({"asset_id":asset.id,"requires":req.requires}),
    )
    .await;
    Ok(Json(ReservationCreated { reservation, token }))
}
async fn release(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    authorize(&headers, &s)?;
    let asset: Asset = sqlx::query_as("SELECT * FROM assets WHERE id=$1")
        .bind(id)
        .fetch_optional(&s.pool)
        .await?
        .ok_or(ApiError::not_found("asset"))?;
    let changed = sqlx::query(
        "UPDATE reservations SET status='RELEASED',released_at=now() WHERE asset_id=$1 AND status='ACTIVE'",
    )
    .bind(id)
    .execute(&s.pool)
    .await?
    .rows_affected();
    if changed == 0 {
        return Err(ApiError::conflict("asset has no active reservation"));
    }
    sqlx::query("UPDATE assets SET status=CASE WHEN EXISTS(SELECT 1 FROM workflow_runs WHERE asset_id=$1 AND status IN ('QUEUED','PREPARING','RUNNING','CLEANING_UP')) THEN 'RUNNING' ELSE 'AVAILABLE' END,updated_at=now() WHERE id=$1").bind(id).execute(&s.pool).await?;
    let _ = s
        .hub
        .send(
            &asset.name,
            control(
                &asset.name,
                "",
                control_message::Payload::ReservationChanged(ReservationChanged {
                    reserved: false,
                    reservation_id: String::new(),
                }),
            ),
        )
        .await;
    audit(
        &s,
        "reservation.released",
        "asset",
        id,
        serde_json::json!({}),
    )
    .await;
    Ok(Json(serde_json::json!({"released":true})))
}
async fn list_reservations(State(s): State<AppState>) -> ApiResult<Json<Vec<Reservation>>> {
    expire_reservations(&s).await?;
    Ok(Json(
        sqlx::query_as("SELECT * FROM reservations ORDER BY created_at DESC")
            .fetch_all(&s.pool)
            .await?,
    ))
}
async fn get_reservation(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Reservation>> {
    Ok(Json(
        sqlx::query_as("SELECT * FROM reservations WHERE id=$1")
            .bind(id)
            .fetch_optional(&s.pool)
            .await?
            .ok_or(ApiError::not_found("reservation"))?,
    ))
}
async fn release_reservation(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    authorize(&headers, &s)?;
    let asset_id:Uuid=sqlx::query_scalar("UPDATE reservations SET status='RELEASED',released_at=now() WHERE id=$1 AND status='ACTIVE' RETURNING asset_id").bind(id).fetch_optional(&s.pool).await?.ok_or(ApiError::conflict("reservation is not active"))?;
    sqlx::query(
        "UPDATE assets SET status='AVAILABLE',updated_at=now() WHERE id=$1 AND status='RESERVED'",
    )
    .bind(asset_id)
    .execute(&s.pool)
    .await?;
    if let Some(name) = sqlx::query_scalar::<_, String>("SELECT name FROM assets WHERE id=$1")
        .bind(asset_id)
        .fetch_optional(&s.pool)
        .await?
    {
        if let Err(error) = s
            .hub
            .send(
                &name,
                control(
                    &name,
                    "",
                    control_message::Payload::ReservationChanged(ReservationChanged {
                        reserved: false,
                        reservation_id: id.to_string(),
                    }),
                ),
            )
            .await
        {
            tracing::warn!(%asset_id,%error,"reservation released while node unavailable");
        }
    }
    audit(
        &s,
        "reservation.released",
        "reservation",
        id,
        serde_json::json!({"asset_id":asset_id}),
    )
    .await;
    Ok(Json(serde_json::json!({"released":true})))
}

#[derive(Debug, Serialize, FromRow)]
struct Workflow {
    id: Uuid,
    name: String,
    definition_yaml: String,
    created_at: DateTime<Utc>,
    description: String,
    version: i32,
    updated_at: DateTime<Utc>,
}
#[derive(Deserialize)]
struct WorkflowRequest {
    definition_yaml: String,
}
async fn create_workflow(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<WorkflowRequest>,
) -> ApiResult<(StatusCode, Json<Workflow>)> {
    authorize(&headers, &s)?;
    let def = WorkflowDefinition::parse(&req.definition_yaml)
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    let row: Workflow = sqlx::query_as(
        "INSERT INTO workflows(name,description,definition_yaml) VALUES($1,$2,$3) RETURNING *",
    )
    .bind(def.name)
    .bind(def.description)
    .bind(req.definition_yaml)
    .fetch_one(&s.pool)
    .await?;
    audit(
        &s,
        "workflow.created",
        "workflow",
        row.id,
        serde_json::json!({"name":row.name}),
    )
    .await;
    Ok((StatusCode::CREATED, Json(row)))
}
async fn get_workflow(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Workflow>> {
    Ok(Json(
        sqlx::query_as("SELECT * FROM workflows WHERE id=$1")
            .bind(id)
            .fetch_optional(&s.pool)
            .await?
            .ok_or(ApiError::not_found("workflow"))?,
    ))
}
async fn update_workflow(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(req): Json<WorkflowRequest>,
) -> ApiResult<Json<Workflow>> {
    authorize(&headers, &s)?;
    let def = WorkflowDefinition::parse(&req.definition_yaml)
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    let row=sqlx::query_as("UPDATE workflows SET name=$1,description=$2,definition_yaml=$3,version=version+1,updated_at=now() WHERE id=$4 RETURNING *").bind(def.name).bind(def.description).bind(req.definition_yaml).bind(id).fetch_optional(&s.pool).await?.ok_or(ApiError::not_found("workflow"))?;
    audit(
        &s,
        "workflow.updated",
        "workflow",
        id,
        serde_json::json!({}),
    )
    .await;
    Ok(Json(row))
}
async fn delete_workflow(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<StatusCode> {
    authorize(&headers, &s)?;
    let result = sqlx::query("DELETE FROM workflows WHERE id=$1")
        .bind(id)
        .execute(&s.pool)
        .await
        .map_err(ApiError::from_db)?;
    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("workflow"));
    }
    audit(
        &s,
        "workflow.deleted",
        "workflow",
        id,
        serde_json::json!({}),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}
async fn list_workflows(State(s): State<AppState>) -> ApiResult<Json<Vec<Workflow>>> {
    Ok(Json(
        sqlx::query_as("SELECT * FROM workflows ORDER BY created_at DESC")
            .fetch_all(&s.pool)
            .await?,
    ))
}

#[derive(Debug, Serialize, FromRow)]
struct SoftwareBundle {
    id: Uuid,
    name: String,
    filename: String,
    content_type: String,
    size_bytes: i64,
    sha256: String,
    storage_key: String,
    created_at: DateTime<Utc>,
}

async fn list_bundles(State(s): State<AppState>) -> ApiResult<Json<Vec<SoftwareBundle>>> {
    Ok(Json(
        sqlx::query_as("SELECT * FROM software_bundles ORDER BY created_at DESC")
            .fetch_all(&s.pool)
            .await?,
    ))
}

async fn upload_bundle(
    State(s): State<AppState>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> ApiResult<(StatusCode, Json<SoftwareBundle>)> {
    authorize(&headers, &s)?;
    let mut name = None;
    let mut upload: Option<(String, String, Bytes)> = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::bad_request(format!("invalid multipart upload: {e}")))?
    {
        match field.name() {
            Some("name") => {
                name = Some(
                    field
                        .text()
                        .await
                        .map_err(|e| ApiError::bad_request(e.to_string()))?,
                );
            }
            Some("file") => {
                let filename = field.file_name().unwrap_or("bundle.tar.gz").to_string();
                let content_type = field
                    .content_type()
                    .unwrap_or("application/octet-stream")
                    .to_string();
                let bytes = field
                    .bytes()
                    .await
                    .map_err(|e| ApiError::bad_request(e.to_string()))?;
                upload = Some((filename, content_type, bytes));
            }
            _ => {}
        }
    }
    let (filename, content_type, bytes) =
        upload.ok_or_else(|| ApiError::bad_request("multipart field 'file' is required"))?;
    if bytes.is_empty() {
        return Err(ApiError::bad_request("bundle must not be empty"));
    }
    if !(filename.ends_with(".tar.gz") || filename.ends_with(".tgz")) {
        return Err(ApiError::bad_request(
            "bundle must be a .tar.gz or .tgz archive",
        ));
    }
    let display_name = name
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| filename.clone());
    if display_name.len() > 200 || filename.len() > 255 {
        return Err(ApiError::bad_request("bundle name is too long"));
    }
    let id = Uuid::new_v4();
    let storage_key = format!("{id}.tar.gz");
    let digest = format!("{:x}", Sha256::digest(&bytes));
    let path = s.bundle_dir.join(&storage_key);
    tokio::fs::write(&path, &bytes)
        .await
        .map_err(ApiError::storage)?;
    let inserted = sqlx::query_as("INSERT INTO software_bundles(id,name,filename,content_type,size_bytes,sha256,storage_key) VALUES($1,$2,$3,$4,$5,$6,$7) RETURNING *")
        .bind(id).bind(display_name.trim()).bind(filename).bind(content_type).bind(bytes.len() as i64).bind(digest).bind(storage_key)
        .fetch_one(&s.pool).await;
    match inserted {
        Ok(bundle) => Ok((StatusCode::CREATED, Json(bundle))),
        Err(error) => {
            let _ = tokio::fs::remove_file(path).await;
            Err(error.into())
        }
    }
}

async fn download_bundle(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<(HeaderMap, Bytes)> {
    let bundle: SoftwareBundle = sqlx::query_as("SELECT * FROM software_bundles WHERE id=$1")
        .bind(id)
        .fetch_optional(&s.pool)
        .await?
        .ok_or(ApiError::not_found("bundle"))?;
    let bytes = tokio::fs::read(s.bundle_dir.join(&bundle.storage_key))
        .await
        .map_err(ApiError::storage)?;
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        "application/gzip".parse().expect("static content type"),
    );
    headers.insert(
        header::CONTENT_DISPOSITION,
        format!(
            "attachment; filename=\"{}\"",
            bundle.filename.replace('"', "")
        )
        .parse()
        .map_err(|_| ApiError::bad_request("invalid stored filename"))?,
    );
    Ok((headers, Bytes::from(bytes)))
}

#[derive(Deserialize)]
struct RunRequest {
    agent_session_id: Option<Uuid>,
    asset_id: Uuid,
    workflow_id: Uuid,
    reservation_id: Option<Uuid>,
    bundle_id: Option<Uuid>,
}
#[derive(Debug, Serialize, FromRow)]
struct RunSummary {
    id: Uuid,
    workflow_id: Uuid,
    asset_id: Uuid,
    reservation_id: Option<Uuid>,
    bundle_id: Option<Uuid>,
    status: String,
    error: Option<String>,
    created_at: DateTime<Utc>,
    started_at: Option<DateTime<Utc>>,
    completed_at: Option<DateTime<Utc>>,
    updated_at: DateTime<Utc>,
    workflow_name: String,
    asset_name: String,
}
#[derive(Debug, Serialize, FromRow)]
struct Step {
    id: Uuid,
    run_id: Uuid,
    name: String,
    phase: String,
    position: i32,
    status: String,
    exit_code: Option<i32>,
    error: Option<String>,
    started_at: Option<DateTime<Utc>>,
    completed_at: Option<DateTime<Utc>>,
}
#[derive(Serialize)]
struct RunDetail {
    #[serde(flatten)]
    run: RunSummary,
    steps: Vec<Step>,
}
const RUN_SELECT:&str="SELECT wr.*,w.name workflow_name,a.name asset_name FROM workflow_runs wr JOIN workflows w ON w.id=wr.workflow_id JOIN assets a ON a.id=wr.asset_id";
async fn create_run(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<RunRequest>,
) -> ApiResult<(StatusCode, Json<RunDetail>)> {
    authorize(&headers, &s)?;
    let mut tx = s.pool.begin().await?;
    let asset: Asset = sqlx::query_as("SELECT * FROM assets WHERE id=$1 FOR UPDATE")
        .bind(req.asset_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ApiError::not_found("asset"))?;
    let workflow: Workflow = sqlx::query_as("SELECT * FROM workflows WHERE id=$1 FOR SHARE")
        .bind(req.workflow_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ApiError::not_found("workflow"))?;
    let bundle: Option<SoftwareBundle> = match req.bundle_id {
        Some(id) => Some(
            sqlx::query_as("SELECT * FROM software_bundles WHERE id=$1")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?
                .ok_or(ApiError::not_found("bundle"))?,
        ),
        None => None,
    };
    let active: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM reservations WHERE asset_id=$1 AND status='ACTIVE' AND expires_at>now() FOR UPDATE",
    )
    .bind(req.asset_id)
    .fetch_optional(&mut *tx)
    .await?;
    if active.is_none() {
        return Err(ApiError::conflict("asset has no active reservation"));
    }
    if !matches!(asset.status.as_str(), "RESERVED" | "AVAILABLE") {
        return Err(ApiError::conflict(format!("asset is {}", asset.status)));
    }
    if let Some(requested) = req.reservation_id {
        if active != Some(requested) {
            return Err(ApiError::conflict(
                "reservation is not active for this asset",
            ));
        }
    }
    if let Some(session) = req.agent_session_id {
        let owned: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM reservations WHERE id=$1 AND owner=$2)",
        )
        .bind(active)
        .bind(format!("torch-agent:{session}"))
        .fetch_one(&mut *tx)
        .await?;
        if !owned {
            return Err(ApiError::conflict("Agent does not own this reservation"));
        }
        let approved_hash: Option<String> =
            sqlx::query_scalar("SELECT agent_approved_hash FROM workflows WHERE id=$1 FOR SHARE")
                .bind(req.workflow_id)
                .fetch_one(&mut *tx)
                .await?;
        if approved_hash.as_deref() != Some(&bench_common::hash_token(&workflow.definition_yaml)) {
            return Err(ApiError::conflict("Workflow version is not agent-approved"));
        }
        let permitted: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM agent_sessions WHERE id=$1 AND NOT cancel_requested AND deadline>now() AND status IN ('RUNNING','WAITING_FOR_APPROVAL') AND (NOT $6 OR (decision=true AND (pending->'action'->>'bundle_id') IS NOT DISTINCT FROM $2::text AND pending->'action'->>'asset_id'=$3::text AND pending->'action'->>'workflow_id'=$4::text AND pending->>'workflow_hash'=$5)))")
            .bind(session).bind(req.bundle_id).bind(req.asset_id).bind(req.workflow_id).bind(bench_common::hash_token(&workflow.definition_yaml)).bind({let d=WorkflowDefinition::parse(&workflow.definition_yaml).map_err(|e|ApiError::bad_request(e.to_string()))?;req.bundle_id.is_some() || !d.deploy.is_empty() || !d.configure.is_empty()}).fetch_one(&mut *tx).await?;
        if !permitted {
            return Err(ApiError::conflict(
                "Agent deployment requires a matching live human approval",
            ));
        }
    }
    let run_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO workflow_runs(id,workflow_id,asset_id,reservation_id,bundle_id,agent_session_id) VALUES($1,$2,$3,$4,$5,$6)",
    )
    .bind(run_id)
    .bind(req.workflow_id)
    .bind(req.asset_id)
    .bind(active)
    .bind(req.bundle_id)
    .bind(req.agent_session_id)
    .execute(&mut *tx)
    .await?;
    let def = WorkflowDefinition::parse(&workflow.definition_yaml)
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    // Step rows are inserted from StepStarted events so their node-generated IDs also
    // correlate subsequent log and completion messages without another mapping layer.
    let _ = def;
    sqlx::query("UPDATE assets SET status='RUNNING',updated_at=now() WHERE id=$1")
        .bind(req.asset_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    if let Err(error) = s
        .hub
        .send(
            &asset.name,
            control(
                &asset.name,
                &run_id.to_string(),
                control_message::Payload::RunWorkflow(RunWorkflow {
                    workflow_run_id: run_id.to_string(),
                    workflow_yaml: workflow.definition_yaml,
                    bundle_url: bundle
                        .as_ref()
                        .map(|value| format!("{}/bundles/{}/download", s.bundle_base_url, value.id))
                        .unwrap_or_default(),
                    bundle_sha256: bundle
                        .as_ref()
                        .map(|value| value.sha256.clone())
                        .unwrap_or_default(),
                }),
            ),
        )
        .await
    {
        sqlx::query("UPDATE workflow_runs SET status='FAILED',error=$1,completed_at=now(),updated_at=now() WHERE id=$2").bind(error).bind(run_id).execute(&s.pool).await?;
        sqlx::query("UPDATE assets SET status=CASE WHEN $2::uuid IS NULL THEN 'AVAILABLE' ELSE 'RESERVED' END WHERE id=$1").bind(req.asset_id).bind(active).execute(&s.pool).await?;
        return Err(ApiError::conflict(error));
    }
    audit(
        &s,
        "run.queued",
        "run",
        run_id,
        serde_json::json!({"asset_id":req.asset_id,"workflow_id":req.workflow_id}),
    )
    .await;
    Ok((StatusCode::CREATED, Json(load_run(&s.pool, run_id).await?)))
}
async fn list_runs(State(s): State<AppState>) -> ApiResult<Json<Vec<RunSummary>>> {
    Ok(Json(
        sqlx::query_as(&format!("{RUN_SELECT} ORDER BY wr.created_at DESC"))
            .fetch_all(&s.pool)
            .await?,
    ))
}
async fn get_run(State(s): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<Json<RunDetail>> {
    Ok(Json(load_run(&s.pool, id).await?))
}
async fn cancel_run(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    authorize(&headers, &s)?;
    let run: RunSummary = sqlx::query_as(&format!("{RUN_SELECT} WHERE wr.id=$1"))
        .bind(id)
        .fetch_optional(&s.pool)
        .await?
        .ok_or(ApiError::not_found("run"))?;
    if matches!(run.status.as_str(), "PASSED" | "FAILED" | "CANCELLED") {
        return Err(ApiError::conflict("run is already complete"));
    }
    s.hub
        .send(
            &run.asset_name,
            control(
                &run.asset_name,
                &id.to_string(),
                control_message::Payload::CancelWorkflow(CancelWorkflow {
                    workflow_run_id: id.to_string(),
                }),
            ),
        )
        .await
        .map_err(ApiError::conflict)?;
    Ok(Json(serde_json::json!({"cancellation_requested":true})))
}
async fn rerun(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<(StatusCode, Json<RunDetail>)> {
    let original: RunSummary = sqlx::query_as(&format!("{RUN_SELECT} WHERE wr.id=$1"))
        .bind(id)
        .fetch_optional(&s.pool)
        .await?
        .ok_or(ApiError::not_found("run"))?;
    create_run(
        State(s),
        headers,
        Json(RunRequest {
            agent_session_id: None,
            asset_id: original.asset_id,
            workflow_id: original.workflow_id,
            reservation_id: None,
            bundle_id: original.bundle_id,
        }),
    )
    .await
}
#[derive(Serialize, FromRow)]
struct Log {
    id: i64,
    step_id: Option<Uuid>,
    stream: String,
    sequence: i64,
    content: Vec<u8>,
    created_at: DateTime<Utc>,
}
#[derive(Serialize)]
struct LogResponse {
    id: i64,
    step_id: Option<Uuid>,
    stream: String,
    sequence: i64,
    content: String,
    created_at: DateTime<Utc>,
}
async fn get_logs(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Vec<LogResponse>>> {
    let rows:Vec<Log>=sqlx::query_as("SELECT id,step_id,stream,sequence,content,created_at FROM logs WHERE run_id=$1 ORDER BY id").bind(id).fetch_all(&s.pool).await?;
    Ok(Json(
        rows.into_iter()
            .map(|x| LogResponse {
                id: x.id,
                step_id: x.step_id,
                stream: x.stream,
                sequence: x.sequence,
                content: String::from_utf8_lossy(&x.content).into_owned(),
                created_at: x.created_at,
            })
            .collect(),
    ))
}

#[derive(Serialize, FromRow)]
struct Artifact {
    id: Uuid,
    run_id: Uuid,
    step_id: Option<Uuid>,
    name: String,
    path: String,
    content_type: String,
    size_bytes: i64,
    storage_path: String,
    created_at: DateTime<Utc>,
}
async fn list_artifacts(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Vec<Artifact>>> {
    Ok(Json(
        sqlx::query_as("SELECT * FROM artifacts WHERE run_id=$1 ORDER BY created_at")
            .bind(id)
            .fetch_all(&s.pool)
            .await?,
    ))
}
async fn download_artifact(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<(HeaderMap, Bytes)> {
    let artifact: Artifact = sqlx::query_as("SELECT * FROM artifacts WHERE id=$1")
        .bind(id)
        .fetch_optional(&s.pool)
        .await?
        .ok_or(ApiError::not_found("artifact"))?;
    let bytes = tokio::fs::read(s.artifact_dir.join(&artifact.storage_path))
        .await
        .map_err(ApiError::storage)?;
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        artifact
            .content_type
            .parse()
            .map_err(|_| ApiError::bad_request("invalid artifact content type"))?,
    );
    headers.insert(
        header::CONTENT_DISPOSITION,
        format!(
            "attachment; filename=\"{}\"",
            artifact.name.replace('"', "")
        )
        .parse()
        .map_err(|_| ApiError::bad_request("invalid artifact name"))?,
    );
    Ok((headers, Bytes::from(bytes)))
}

async fn load_run(pool: &PgPool, id: Uuid) -> ApiResult<RunDetail> {
    let run = sqlx::query_as(&format!("{RUN_SELECT} WHERE wr.id=$1"))
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or(ApiError::not_found("run"))?;
    let steps = sqlx::query_as("SELECT * FROM workflow_steps WHERE run_id=$1 ORDER BY position")
        .bind(id)
        .fetch_all(pool)
        .await?;
    Ok(RunDetail { run, steps })
}
async fn expire_reservations(s: &AppState) -> ApiResult<()> {
    sqlx::query("WITH expired AS (UPDATE reservations SET status='EXPIRED' WHERE status='ACTIVE' AND expires_at<=now() RETURNING asset_id) UPDATE assets SET status='AVAILABLE',updated_at=now() WHERE id IN (SELECT asset_id FROM expired) AND status='RESERVED'").execute(&s.pool).await?;
    Ok(())
}
fn control(asset: &str, run: &str, payload: control_message::Payload) -> ControlMessage {
    ControlMessage {
        envelope: Some(Envelope {
            asset_id: asset.into(),
            workflow_run_id: run.into(),
            timestamp_ms: Utc::now().timestamp_millis(),
            node_id: String::new(),
        }),
        payload: Some(payload),
    }
}

type ApiResult<T> = Result<T, ApiError>;
struct ApiError {
    status: StatusCode,
    message: String,
}
impl ApiError {
    fn bad_request(m: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: m.into(),
        }
    }
    fn not_found(x: &str) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: format!("{x} not found"),
        }
    }
    fn conflict(m: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message: m.into(),
        }
    }
    fn from_db(e: sqlx::Error) -> Self {
        match &e {
            sqlx::Error::Database(d) if d.is_unique_violation() => {
                Self::conflict("asset already has an active reservation")
            }
            _ => e.into(),
        }
    }
    fn storage(error: std::io::Error) -> Self {
        tracing::error!(%error, "bundle storage error");
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: "bundle storage operation failed".into(),
        }
    }
}
impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        tracing::error!(error=%e,"database error");
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: "database operation failed".into(),
        }
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(serde_json::json!({"error":self.message}))).into_response()
    }
}
