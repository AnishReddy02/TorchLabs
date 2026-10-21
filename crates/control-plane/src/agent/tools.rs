use super::{event, model::Action};
use crate::api::{self, AppState};
use anyhow::{bail, Context, Result};
use axum::{
    body::{to_bytes, Body},
    http::Request,
    Router,
};
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

#[derive(Clone)]
pub struct Tools {
    pub state: AppState,
    router: Router,
    token: String,
}
#[derive(Debug, PartialEq)]
pub enum Policy {
    Allow,
    Approval,
}
pub fn policy(action: &Action) -> Policy {
    match action {
        Action::StartWorkflow {
            bundle_id: Some(_), ..
        } => Policy::Approval,
        _ => Policy::Allow,
    }
}
impl Tools {
    pub fn new(state: AppState, token: String) -> Self {
        Self {
            router: api::router().with_state(state.clone()),
            state,
            token,
        }
    }
    // Invoke the same router as human clients, with server-owned credentials. The
    // model can choose a typed action but never a URL, HTTP method or raw body.
    pub async fn api(&self, method: &str, path: &str, body: Value) -> Result<Value> {
        let response = self
            .router
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("authorization", format!("Bearer {}", self.token))
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))?,
            )
            .await?;
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 2 * 1024 * 1024)
            .await
            .context("Tool response exceeds 2 MiB limit")?;
        let mut value: Value = serde_json::from_slice(&bytes)?;
        if !status.is_success() {
            bail!(
                "{}",
                value["error"]
                    .as_str()
                    .unwrap_or("Control plane rejected action")
            );
        }
        if let Some(object) = value.as_object_mut() {
            object.remove("token");
        }
        Ok(value)
    }
    pub async fn reservation(&self, session: Uuid) -> Result<Option<(Uuid, Uuid)>> {
        Ok(sqlx::query_as("SELECT id,asset_id FROM reservations WHERE owner=$1 AND status='ACTIVE' AND expires_at>now() ORDER BY created_at DESC LIMIT 1")
            .bind(format!("torch-agent:{session}")).fetch_optional(&self.state.pool).await?)
    }
    async fn own_run(&self, session: Uuid, run: Uuid) -> Result<()> {
        let owned: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM workflow_runs WHERE id=$1 AND agent_session_id=$2)",
        )
        .bind(run)
        .bind(session)
        .fetch_one(&self.state.pool)
        .await?;
        if !owned {
            bail!("Agent may operate only its own runs");
        }
        Ok(())
    }
    async fn enforce_asset_scope(&self, session: Uuid, asset_id: Uuid) -> Result<()> {
        let target: Option<String> = sqlx::query_scalar(
            "SELECT NULLIF(constraints->>'target_asset','') FROM agent_sessions WHERE id=$1",
        )
        .bind(session)
        .fetch_one(&self.state.pool)
        .await?;
        if let Some(target) = target {
            let actual: String = sqlx::query_scalar("SELECT name FROM assets WHERE id=$1")
                .bind(asset_id)
                .fetch_optional(&self.state.pool)
                .await?
                .context("Asset does not exist")?;
            if !actual.eq_ignore_ascii_case(&target) {
                bail!("Operator restricted this session to asset '{target}'");
            }
        }
        Ok(())
    }
    pub async fn approval_snapshot(&self, session: Uuid, action: &Action) -> Result<Value> {
        if let Action::StartWorkflow {
            asset_id,
            workflow_id,
            bundle_id,
            reason,
        } = action
        {
            self.enforce_asset_scope(session, *asset_id).await?;
            if reason.is_empty() || reason.len() > 500 {
                bail!("Deployment requires a short reason (1–500 bytes)");
            }
            let (_, owned) = self
                .reservation(session)
                .await?
                .context("Reserve an asset first")?;
            if owned != *asset_id {
                bail!("Asset is not reserved by this agent");
            }
            let asset = self
                .api("GET", &format!("/assets/{asset_id}"), Value::Null)
                .await?;
            let workflow = self
                .api("GET", &format!("/workflows/{workflow_id}"), Value::Null)
                .await?;
            let yaml = workflow["definition_yaml"]
                .as_str()
                .context("missing workflow definition")?;
            let hash = bench_common::hash_token(yaml);
            let approved: Option<String> =
                sqlx::query_scalar("SELECT agent_approved_hash FROM workflows WHERE id=$1")
                    .bind(workflow_id)
                    .fetch_one(&self.state.pool)
                    .await?;
            if approved.as_deref() != Some(&hash) {
                bail!("Workflow version is not approved for agent execution");
            }
            let def = bench_common::WorkflowDefinition::parse(yaml)?;
            if !bench_common::capabilities_match(&asset["capabilities"], &def.requires) {
                bail!("Asset does not meet workflow capabilities");
            }
            let bundle: Option<Value> = if let Some(id) = bundle_id {
                Some(
                    sqlx::query_scalar(
                        "SELECT to_jsonb(b)-'storage_key' FROM software_bundles b WHERE id=$1",
                    )
                    .bind(id)
                    .fetch_optional(&self.state.pool)
                    .await?
                    .context("Bundle does not exist")?,
                )
            } else {
                None
            };
            return Ok(
                json!({"request_id":Uuid::new_v4(),"action":action,"asset":asset["name"],"workflow":workflow["name"],"workflow_hash":hash,"bundle":bundle,"software_changes":bundle_id.is_some() || !def.deploy.is_empty() || !def.configure.is_empty(),"reason":reason,"notice":"Executes this exact approved workflow, transferring the bundle when selected. No persistent installation is provided by Torch."}),
            );
        }
        bail!("Action does not support approval")
    }
    pub async fn execute(&self, session: Uuid, action: &Action, approved: bool) -> Result<Value> {
        if policy(action) == Policy::Approval && !approved {
            bail!("Human approval required");
        }
        let null = Value::Null;
        match action {
            Action::ListAssets {} => {
                let rows:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('id',id,'name',name,'status',status,'hardware_type',hardware_type,'capabilities',capabilities,'labels',labels,'hostname',hostname,'os',os,'architecture',architecture,'last_seen_at',last_seen_at) FROM assets WHERE status<>'OFFLINE' ORDER BY last_seen_at DESC LIMIT 20").fetch_all(&self.state.pool).await?;
                Ok(json!(rows))
            }
            Action::InspectAsset { asset_id } => {
                let asset = self
                    .api("GET", &format!("/assets/{asset_id}"), null)
                    .await?;
                let recent: Vec<Value>=sqlx::query_scalar("SELECT to_jsonb(r) FROM workflow_runs r WHERE asset_id=$1 ORDER BY created_at DESC LIMIT 5").bind(asset_id).fetch_all(&self.state.pool).await?;
                Ok(json!({"asset":asset,"recent_runs":recent}))
            }
            Action::ReserveAsset { asset_id } => {
                self.enforce_asset_scope(session, *asset_id).await?;
                if self.reservation(session).await?.is_some() {
                    bail!("Agent already owns a reservation");
                }
                self.api(
                    "POST",
                    &format!("/assets/{asset_id}/reserve"),
                    json!({"owner":format!("torch-agent:{session}"),"duration_minutes":30}),
                )
                .await
            }
            Action::ReleaseAsset {} => {
                if self.reservation(session).await?.is_none() {
                    bail!("This agent has no reservation to release; choose the next action from current observations");
                }
                self.cleanup(session).await?;
                Ok(json!({"released":true}))
            }
            Action::ListSoftware {} => {
                let rows:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('id',id,'name',name,'filename',filename,'size_bytes',size_bytes,'sha256',sha256,'created_at',created_at) FROM (SELECT DISTINCT ON (name) * FROM software_bundles ORDER BY name,created_at DESC) b ORDER BY created_at DESC LIMIT 20").fetch_all(&self.state.pool).await?;
                Ok(json!(rows))
            }
            Action::ListWorkflows {} => {
                let rows: Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('id',id,'name',name,'description',description,'version',version,'created_at',created_at,'phases',jsonb_build_object('prepare',definition_yaml LIKE '%prepare:%','deploy',definition_yaml LIKE '%deploy:%','configure',definition_yaml LIKE '%configure:%','test',definition_yaml LIKE '%test:%','collect',definition_yaml LIKE '%collect:%','cleanup',definition_yaml LIKE '%cleanup:%')) FROM (SELECT DISTINCT ON (name) * FROM workflows WHERE agent_approved_hash=encode(sha256(convert_to(definition_yaml,'UTF8')),'hex') ORDER BY name,created_at DESC) w ORDER BY created_at DESC LIMIT 20").fetch_all(&self.state.pool).await?;
                Ok(json!(rows))
            }
            Action::StartWorkflow {
                asset_id,
                workflow_id,
                bundle_id,
                ..
            } => {
                self.enforce_asset_scope(session, *asset_id).await?;
                let snapshot = self.approval_snapshot(session, action).await?;
                if snapshot["software_changes"] == true && !approved {
                    bail!("Human approval required for software changes");
                }
                let (reservation_id, _) = self
                    .reservation(session)
                    .await?
                    .context("Reservation expired")?;
                self.api("POST","/runs",json!({"asset_id":asset_id,"workflow_id":workflow_id,"bundle_id":bundle_id,"reservation_id":reservation_id,"agent_session_id":session})).await
            }
            Action::InspectRun { run_id } => {
                self.own_run(session, *run_id).await?;
                let result = self.api("GET", &format!("/runs/{run_id}"), null).await?;
                if !terminal(result["status"].as_str().unwrap_or("")) {
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
                Ok(result)
            }
            Action::ReadRunLogs { run_id } => {
                self.own_run(session, *run_id).await?;
                // Bound the evidence sent externally while retaining exact step IDs.
                let chunks: Vec<(i64,Option<Uuid>,String,Vec<u8>)>=sqlx::query_as("SELECT id,step_id,stream,substring(content for 4096) FROM (SELECT * FROM logs WHERE run_id=$1 ORDER BY id DESC LIMIT 40) l ORDER BY id")
                    .bind(run_id).fetch_all(&self.state.pool).await?;
                let rows:Vec<Value>=chunks.into_iter().map(|(id,step_id,stream,bytes)|json!({"id":id,"step_id":step_id,"stream":stream,"content":String::from_utf8_lossy(&bytes)})).collect();
                Ok(
                    json!({"run_id":run_id,"logs":rows,"limit":"last 40 chunks, up to 4096 bytes each"}),
                )
            }
            Action::ListArtifacts { run_id } => {
                self.own_run(session, *run_id).await?;
                self.api("GET", &format!("/runs/{run_id}/artifacts"), null)
                    .await
            }
            Action::CancelRun { run_id } => {
                self.own_run(session, *run_id).await?;
                self.api("POST", &format!("/runs/{run_id}/cancel"), json!({}))
                    .await
            }
            Action::Finish { .. } => bail!("Finish is handled by runtime"),
        }
    }
    pub async fn cleanup(&self, session: Uuid) -> Result<()> {
        let runs: Vec<(Uuid, String)> =
            sqlx::query_as("SELECT id,status FROM workflow_runs WHERE agent_session_id=$1")
                .bind(session)
                .fetch_all(&self.state.pool)
                .await?;
        for (id, status) in runs {
            if !terminal(&status) {
                let disconnected:Option<String>=sqlx::query_scalar("SELECT a.name FROM assets a JOIN workflow_runs r ON r.asset_id=a.id WHERE r.id=$1 AND a.last_seen_at<now()-interval '15 seconds'").bind(id).fetch_optional(&self.state.pool).await?;
                if let Some(name) = disconnected {
                    if !self.state.hub.is_connected(&name).await {
                        sqlx::query("UPDATE workflow_runs SET status='FAILED',error='Node offline; physical cleanup unconfirmed',completed_at=now(),updated_at=now() WHERE id=$1").bind(id).execute(&self.state.pool).await?;
                        sqlx::query("UPDATE assets SET status='OFFLINE' WHERE name=$1")
                            .bind(name)
                            .execute(&self.state.pool)
                            .await?;
                        event(&self.state.pool,session,"STATUS",json!({"run_id":id,"message":"Node offline. Run interrupted; physical cleanup could not be confirmed."})).await?;
                        continue;
                    }
                }
                self.api("POST", &format!("/runs/{id}/cancel"), json!({}))
                    .await?;
                bail!(
                    "Cancellation requested; waiting for node cleanup before releasing reservation"
                );
            }
        }
        if let Some((id, _)) = self.reservation(session).await? {
            self.api("POST", &format!("/reservations/{id}/release"), json!({}))
                .await?;
            event(
                &self.state.pool,
                session,
                "STATUS",
                json!({"message":"Agent reservation released","reservation_id":id}),
            )
            .await?;
        }
        Ok(())
    }
}
pub fn terminal(status: &str) -> bool {
    matches!(status, "PASSED" | "FAILED" | "CANCELLED" | "TIMED_OUT")
}
