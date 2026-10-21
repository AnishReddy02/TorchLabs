use crate::recovery::{self, RecoveryPolicy};
use bench_protocol::bench::{
    node_message::Payload, node_service_server::NodeService as NodeServiceTrait, ControlMessage,
    NodeMessage, NodeState,
};
use futures::Stream;
use sqlx::PgPool;
use std::{collections::HashMap, path::PathBuf, pin::Pin, sync::Arc};
use tokio::sync::{mpsc, RwLock};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status, Streaming};
use tracing::{info, warn};
use uuid::Uuid;

type NodeSender = mpsc::Sender<Result<ControlMessage, Status>>;

#[derive(Default)]
pub struct NodeHub {
    senders: RwLock<HashMap<String, (Uuid, NodeSender)>>,
}

impl NodeHub {
    pub async fn is_connected(&self, asset: &str) -> bool {
        self.senders.read().await.contains_key(asset)
    }

    pub async fn send(&self, asset: &str, message: ControlMessage) -> Result<(), &'static str> {
        let sender = self
            .senders
            .read()
            .await
            .get(asset)
            .map(|(_, sender)| sender.clone())
            .ok_or("asset node is not connected")?;
        tokio::time::timeout(std::time::Duration::from_secs(2), sender.send(Ok(message)))
            .await
            .map_err(|_| "node dispatch queue timed out")?
            .map_err(|_| "asset node disconnected")
    }
}

#[derive(Clone)]
pub struct NodeService {
    pub pool: PgPool,
    pub hub: Arc<NodeHub>,
    pub artifact_dir: Arc<PathBuf>,
    pub node_token_hash: String,
    pub recovery_policy: Arc<RecoveryPolicy>,
    pub recovery_lock: Arc<tokio::sync::Mutex<()>>,
}

#[tonic::async_trait]
impl NodeServiceTrait for NodeService {
    type SessionStream = Pin<Box<dyn Stream<Item = Result<ControlMessage, Status>> + Send>>;

    async fn session(
        &self,
        request: Request<Streaming<NodeMessage>>,
    ) -> Result<Response<Self::SessionStream>, Status> {
        let mut inbound = request.into_inner();
        let first = inbound
            .message()
            .await?
            .ok_or_else(|| Status::invalid_argument("register message required"))?;
        let envelope = first
            .envelope
            .clone()
            .ok_or_else(|| Status::invalid_argument("envelope required"))?;
        let register = match first.payload {
            Some(Payload::Register(value)) => value,
            _ => return Err(Status::invalid_argument("first message must register")),
        };
        if bench_common::hash_token(&register.token) != self.node_token_hash {
            return Err(Status::unauthenticated("invalid node token"));
        }
        let node_id = Uuid::parse_str(&envelope.node_id)
            .map_err(|_| Status::invalid_argument("invalid node id"))?;
        let asset_key = envelope.asset_id.clone();
        let inventory = register.inventory.unwrap_or_default();
        // Reinstalling a node creates a new persistent identity. Retain the old
        // identity for history, but only advertise the newly connected one.
        sqlx::query("UPDATE nodes SET connected=false,state='OFFLINE',updated_at=now() WHERE asset_key=$1 AND id<>$2")
            .bind(&asset_key).bind(node_id).execute(&self.pool).await.map_err(internal)?;
        sqlx::query("INSERT INTO nodes(id,asset_key,name,version,hostname,os,kernel,architecture,cpu_count,memory_bytes,connected,state,last_seen_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,true,'AVAILABLE',now()) ON CONFLICT(id) DO UPDATE SET asset_key=$2,name=$3,version=$4,hostname=$5,os=$6,kernel=$7,architecture=$8,cpu_count=$9,memory_bytes=$10,connected=true,state='AVAILABLE',last_seen_at=now(),updated_at=now()")
            .bind(node_id).bind(&asset_key).bind(&register.node_name).bind(&register.version).bind(&inventory.hostname).bind(&inventory.os).bind(&inventory.kernel).bind(&inventory.architecture).bind(inventory.cpu_count as i32).bind(inventory.ram_bytes as i64).execute(&self.pool).await.map_err(internal)?;
        let capabilities: serde_json::Value = serde_json::from_str(&register.capabilities_json)
            .unwrap_or_else(|_| serde_json::json!({}));
        sqlx::query("INSERT INTO assets(name,node_id,status,hostname,os,architecture,metadata,capabilities,last_seen_at) VALUES($1,$2,'AVAILABLE',$3,$4,$5,$6,$7,now()) ON CONFLICT(name) DO UPDATE SET node_id=$2,status=CASE WHEN EXISTS(SELECT 1 FROM reservations r WHERE r.asset_id=assets.id AND r.status='ACTIVE' AND r.expires_at>now()) THEN 'RESERVED' ELSE 'AVAILABLE' END,hostname=$3,os=$4,architecture=$5,metadata=$6,capabilities=$7,last_seen_at=now(),updated_at=now()")
            .bind(&asset_key).bind(node_id).bind(&inventory.hostname).bind(&inventory.os).bind(&inventory.architecture)
            .bind(serde_json::json!({"kernel": inventory.kernel, "cpu_count": inventory.cpu_count, "ram_bytes": inventory.ram_bytes, "network_interfaces": inventory.network_interfaces}))
            .bind(capabilities)
            .execute(&self.pool).await.map_err(internal)?;
        let (tx, rx) = mpsc::channel(128);
        let connection_id = Uuid::new_v4();
        self.hub
            .senders
            .write()
            .await
            .insert(asset_key.clone(), (connection_id, tx));
        info!(asset=%asset_key, %node_id, "node connected");
        let service = self.clone();
        tokio::spawn(async move {
            while let Ok(Some(message)) = inbound.message().await {
                if !service
                    .hub
                    .senders
                    .read()
                    .await
                    .get(&asset_key)
                    .is_some_and(|(id, _)| *id == connection_id)
                {
                    break;
                }
                if !message.envelope.as_ref().is_some_and(|env| {
                    env.node_id == node_id.to_string() && env.asset_id == asset_key
                }) {
                    warn!("rejecting message with identity outside registered session");
                    break;
                }
                if let Err(error) = service.handle(message).await {
                    warn!(asset=%asset_key, %error, "node message failed");
                }
            }
            let mut senders = service.hub.senders.write().await;
            if senders
                .get(&asset_key)
                .is_some_and(|(connected_id, _)| *connected_id == connection_id)
            {
                senders.remove(&asset_key);
            } else {
                return;
            }
            drop(senders);
            let _ = sqlx::query(
                "UPDATE nodes SET connected=false,state='OFFLINE',updated_at=now() WHERE id=$1",
            )
            .bind(node_id)
            .execute(&service.pool)
            .await;
            let _ =
                sqlx::query("UPDATE assets SET status='OFFLINE',updated_at=now() WHERE node_id=$1")
                    .bind(node_id)
                    .execute(&service.pool)
                    .await;
            let _ = sqlx::query("UPDATE workflow_runs wr SET status='FAILED',error='Bench Node disconnected during workflow',completed_at=now(),updated_at=now() FROM assets a WHERE a.node_id=$1 AND wr.asset_id=a.id AND wr.status IN ('QUEUED','PREPARING','RUNNING','CLEANING_UP')")
                .bind(node_id).execute(&service.pool).await;
            if let Err(error) = recovery::disconnect(&service.pool, &asset_key).await {
                warn!(%error,"recovery disconnect reconciliation failed");
            }
            info!(asset=%asset_key, "node disconnected");
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }
}

impl NodeService {
    async fn handle(&self, message: NodeMessage) -> anyhow::Result<()> {
        let env = message
            .envelope
            .ok_or_else(|| anyhow::anyhow!("missing envelope"))?;
        let run_id = Uuid::parse_str(&env.workflow_run_id).ok();
        match message.payload {
            Some(Payload::Heartbeat(heartbeat)) => {
                let status = match NodeState::try_from(heartbeat.state).unwrap_or(NodeState::Error)
                {
                    NodeState::Available => "AVAILABLE",
                    NodeState::Reserved => "RESERVED",
                    NodeState::Running | NodeState::CleaningUp => "RUNNING",
                    NodeState::Error => "ERROR",
                    _ => "OFFLINE",
                };
                sqlx::query(
                    "UPDATE assets SET status=CASE WHEN $1='AVAILABLE' AND EXISTS(SELECT 1 FROM reservations r WHERE r.asset_id=assets.id AND r.status='ACTIVE' AND r.expires_at>now()) THEN 'RESERVED' ELSE $1 END,last_seen_at=now(),updated_at=now() WHERE name=$2",
                )
                .bind(status)
                .bind(&env.asset_id)
                .execute(&self.pool)
                .await?;
            }
            Some(Payload::StateChanged(value)) => {
                if let Some(run) = run_id {
                    let status = match NodeState::try_from(value.state).unwrap_or(NodeState::Error)
                    {
                        NodeState::Running => "RUNNING",
                        NodeState::CleaningUp => "CLEANING_UP",
                        _ => return Ok(()),
                    };
                    sqlx::query("UPDATE workflow_runs SET status=$1,updated_at=now() WHERE id=$2 AND status NOT IN ('PASSED','FAILED','CANCELLED')").bind(status).bind(run).execute(&self.pool).await?;
                }
            }
            Some(Payload::WorkflowStarted(_)) => {
                if let Some(id) = run_id {
                    sqlx::query("UPDATE workflow_runs SET status='PREPARING',started_at=COALESCE(started_at,now()),updated_at=now() WHERE id=$1").bind(id).execute(&self.pool).await?;
                }
            }
            Some(Payload::StepStarted(step)) => {
                let id = Uuid::parse_str(&step.step_id)?;
                if let Some(run) = run_id {
                    sqlx::query("INSERT INTO workflow_steps(id,run_id,name,phase,position,command,status,started_at) VALUES($1,$2,$3,$4,$5,$6,'RUNNING',now()) ON CONFLICT(id) DO UPDATE SET status='RUNNING',started_at=now()")
                        .bind(id).bind(run).bind(step.name).bind(step.phase).bind(step.position).bind(step.command).execute(&self.pool).await?;
                }
            }
            Some(Payload::Stdout(log)) => self.log(run_id, log, "stdout").await?,
            Some(Payload::Stderr(log)) => self.log(run_id, log, "stderr").await?,
            Some(Payload::StepCompleted(step)) => {
                let id = Uuid::parse_str(&step.step_id)?;
                let status = if step.success {
                    "PASSED"
                } else if step.timed_out {
                    "TIMED_OUT"
                } else {
                    "FAILED"
                };
                sqlx::query("UPDATE workflow_steps SET status=$1,exit_code=$2,error=NULLIF($3,''),duration_ms=$4,completed_at=now() WHERE id=$5").bind(status).bind(step.exit_code).bind(step.error).bind(step.duration_ms as i64).bind(id).execute(&self.pool).await?;
            }
            Some(Payload::ArtifactUploaded(artifact)) => {
                self.store_artifact(run_id, artifact).await?
            }
            Some(Payload::ServiceObservation(observation)) => {
                if (chrono::Utc::now().timestamp_millis() - env.timestamp_ms).abs() > 10_000 {
                    return Ok(());
                }
                // Coalesce telemetry while diagnosis is busy, keeping heartbeats/results live.
                if let Ok(guard) = self.recovery_lock.clone().try_lock_owned() {
                    let service = self.clone();
                    tokio::spawn(async move {
                        let _guard = guard;
                        if let Err(error) = recovery::handle_observation(
                            &service.pool,
                            &service.hub,
                            &service.recovery_policy,
                            &env.asset_id,
                            observation,
                        )
                        .await
                        {
                            warn!(%error,"recovery observation failed");
                        }
                    });
                }
            }
            Some(Payload::RemediationAcknowledged(acknowledgement)) => {
                recovery::handle_acknowledgement(&self.pool, &env.asset_id, acknowledgement).await?
            }
            Some(Payload::RemediationCompleted(completion)) => {
                recovery::handle_completion(&self.pool, &env.asset_id, completion).await?
            }
            Some(Payload::WorkflowCompleted(_)) => self.finish(run_id, "PASSED", None).await?,
            Some(Payload::WorkflowFailed(value)) => {
                self.finish(
                    run_id,
                    if value.cancelled {
                        "CANCELLED"
                    } else if value.timed_out {
                        "TIMED_OUT"
                    } else {
                        "FAILED"
                    },
                    Some(value.error),
                )
                .await?
            }
            _ => {}
        }
        Ok(())
    }
    async fn log(
        &self,
        run: Option<Uuid>,
        log: bench_protocol::bench::LogChunk,
        stream: &str,
    ) -> anyhow::Result<()> {
        if let Some(run_id) = run {
            sqlx::query(
                "INSERT INTO logs(run_id,step_id,stream,sequence,content) VALUES($1,$2,$3,$4,$5)",
            )
            .bind(run_id)
            .bind(Uuid::parse_str(&log.step_id).ok())
            .bind(stream)
            .bind(log.sequence as i64)
            .bind(log.data)
            .execute(&self.pool)
            .await?;
        }
        Ok(())
    }
    async fn finish(
        &self,
        run: Option<Uuid>,
        status: &str,
        error: Option<String>,
    ) -> anyhow::Result<()> {
        if let Some(id) = run {
            sqlx::query("UPDATE workflow_runs SET status=$1,error=$2,completed_at=now(),updated_at=now() WHERE id=$3").bind(status).bind(error).bind(id).execute(&self.pool).await?;
            sqlx::query("UPDATE assets a SET status=CASE WHEN EXISTS(SELECT 1 FROM reservations r WHERE r.asset_id=a.id AND r.status='ACTIVE' AND r.expires_at>now()) THEN 'RESERVED' ELSE 'AVAILABLE' END,updated_at=now() FROM workflow_runs wr WHERE wr.id=$1 AND a.id=wr.asset_id").bind(id).execute(&self.pool).await?;
        }
        Ok(())
    }
    async fn store_artifact(
        &self,
        run: Option<Uuid>,
        artifact: bench_protocol::bench::ArtifactUploaded,
    ) -> anyhow::Result<()> {
        let run_id = run.ok_or_else(|| anyhow::anyhow!("artifact missing run id"))?;
        let id = Uuid::parse_str(&artifact.artifact_id)?;
        let storage_path = format!("{run_id}/{id}");
        let directory = self.artifact_dir.join(run_id.to_string());
        tokio::fs::create_dir_all(&directory).await?;
        tokio::fs::write(directory.join(id.to_string()), &artifact.data).await?;
        sqlx::query("INSERT INTO artifacts(id,run_id,name,path,content_type,size_bytes,storage_path) VALUES($1,$2,$3,$4,$5,$6,$7)")
            .bind(id).bind(run_id).bind(artifact.name).bind(artifact.path).bind(artifact.content_type).bind(artifact.data.len() as i64).bind(storage_path).execute(&self.pool).await?;
        Ok(())
    }
}

fn internal(error: sqlx::Error) -> Status {
    Status::internal(error.to_string())
}
