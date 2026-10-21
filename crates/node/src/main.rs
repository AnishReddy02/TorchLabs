mod executor;
mod recovery;

use anyhow::Context;
use bench_common::{WorkflowDefinition, WorkflowStep};
use bench_protocol::bench::{
    control_message, node_message, node_service_client::NodeServiceClient, ArtifactUploaded,
    Envelope, Heartbeat, Inventory, LogChunk, NodeMessage, NodeState, Register, StateChanged,
    StepCompleted, StepStarted, WorkflowCompleted, WorkflowFailed, WorkflowStarted,
};
use chrono::Utc;
use clap::Parser;
use executor::{ExecutionRequest, Executor, OutputEvent, ShellExecutor};
use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};
use std::{path::PathBuf, sync::Arc, time::Duration};
use sysinfo::{Networks, System};
use tokio::sync::{mpsc, watch, RwLock};
use tokio_stream::wrappers::ReceiverStream;
use tonic::Request;
use tracing::{error, info, warn};
use uuid::Uuid;

struct AbortTask(tokio::task::JoinHandle<()>);
impl Drop for AbortTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[derive(Parser, Debug, Clone)]
struct Config {
    #[arg(long, env = "BENCH_ASSET_ID")]
    asset_id: String,
    #[arg(long, env = "BENCH_NODE_NAME", default_value = "bench-node")]
    node_name: String,
    #[arg(long, env = "BENCH_NODE_TOKEN", default_value = "dev-node-token")]
    token: String,
    #[arg(
        long,
        env = "BENCH_CONTROL_PLANE",
        default_value = "http://localhost:50051"
    )]
    control_plane: String,
    #[arg(
        long,
        env = "BENCH_NODE_IDENTITY_FILE",
        default_value = ".bench-node-id"
    )]
    identity_file: PathBuf,
    #[arg(long, default_value_t = 5)]
    heartbeat_seconds: u64,
    #[arg(
        long,
        env = "BENCH_NODE_WORKSPACE_DIR",
        default_value = ".bench-workspaces"
    )]
    workspace_dir: PathBuf,
    #[arg(long, env = "TORCH_RECOVERY_SERVICE_ID", default_value = "")]
    recovery_service_id: String,
    #[arg(long, env = "TORCH_RECOVERY_CONTAINER_NAME", default_value = "")]
    recovery_container_name: String,
    #[arg(long, env = "TORCH_RECOVERY_HEALTH_URL", default_value = "")]
    recovery_health_url: String,
    #[arg(long, env = "TORCH_RECOVERY_INTERVAL_SECONDS", default_value_t = 2)]
    recovery_interval_seconds: u64,
    #[arg(
        long,
        env = "TORCH_RECOVERY_LEDGER_FILE",
        default_value = ".bench-remediation-ledger.json"
    )]
    recovery_ledger_file: PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env().add_directive("info".parse()?),
        )
        .init();
    let config = Config::parse();
    let node_id = load_identity(&config.identity_file).await?;
    info!(%node_id,asset=%config.asset_id,"bench node starting");
    let mut delay = Duration::from_secs(1);
    loop {
        tokio::select! { _=tokio::signal::ctrl_c()=>{info!("shutting down");return Ok(())}, result=run_connection(&config,node_id)=>if let Err(error)=result{warn!(%error,"connection ended")}}
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(Duration::from_secs(30));
    }
}

async fn run_connection(config: &Config, node_id: Uuid) -> anyhow::Result<()> {
    let mut client = NodeServiceClient::connect(config.control_plane.clone()).await?;
    let (tx, rx) = mpsc::channel(256);
    let state = Arc::new(RwLock::new(NodeState::Available));
    let reserved = Arc::new(RwLock::new(false));
    let recovery = recovery::RecoveryConfig::new(
        &config.recovery_service_id,
        &config.recovery_container_name,
        &config.recovery_health_url,
        config.recovery_interval_seconds,
        config.recovery_ledger_file.clone(),
    )?;
    let remediation_active = Arc::new(std::sync::atomic::AtomicBool::new(false));
    tx.send(message(
        node_id,
        &config.asset_id,
        "",
        node_message::Payload::Register(Register {
            inventory: Some(inventory()),
            node_name: config.node_name.clone(),
            version: env!("CARGO_PKG_VERSION").into(),
            token: config.token.clone(),
            capabilities_json: detected_capabilities().to_string(),
        }),
    ))
    .await?;
    let response = client
        .session(Request::new(ReceiverStream::new(rx)))
        .await?;
    let mut inbound = response.into_inner();
    let heartbeat_tx = tx.clone();
    let asset = config.asset_id.clone();
    let heartbeat_state = state.clone();
    let interval = config.heartbeat_seconds;
    let heartbeat = AbortTask(tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(interval)).await;
            let current = *heartbeat_state.read().await;
            if heartbeat_tx
                .send(message(
                    node_id,
                    &asset,
                    "",
                    node_message::Payload::Heartbeat(Heartbeat {
                        state: current as i32,
                    }),
                ))
                .await
                .is_err()
            {
                break;
            }
        }
    }));
    let recovery_monitor = recovery.as_ref().map(|recovery| {
        AbortTask(recovery::spawn_monitor(
            recovery.clone(),
            tx.clone(),
            node_id,
            config.asset_id.clone(),
        ))
    });
    let mut current_job: Option<(String, watch::Sender<bool>)> = None;
    while let Some(control) = inbound.message().await? {
        if current_job.is_some()
            && matches!(
                *state.read().await,
                NodeState::Available | NodeState::Reserved
            )
        {
            current_job = None;
        }
        match control.payload {
            Some(control_message::Payload::ReservationChanged(change)) => {
                *reserved.write().await = change.reserved;
                if current_job.is_none() {
                    let next = if change.reserved {
                        NodeState::Reserved
                    } else {
                        NodeState::Available
                    };
                    *state.write().await = next;
                    send_state(&tx, node_id, &config.asset_id, "", next, "").await?;
                }
            }
            Some(control_message::Payload::RunWorkflow(run)) => {
                if current_job.is_some()
                    || remediation_active.load(std::sync::atomic::Ordering::SeqCst)
                {
                    warn!(run=%run.workflow_run_id,"rejecting concurrent workflow");
                    continue;
                }
                let (cancel_tx, cancel_rx) = watch::channel(false);
                current_job = Some((run.workflow_run_id.clone(), cancel_tx));
                let job_tx = tx.clone();
                let job_state = state.clone();
                let asset = config.asset_id.clone();
                let job_reserved = reserved.clone();
                let workspace_root = config.workspace_dir.clone();
                tokio::spawn(async move {
                    if let Err(error) = execute_workflow(
                        job_tx.clone(),
                        node_id,
                        &asset,
                        &run.workflow_run_id,
                        &run.workflow_yaml,
                        cancel_rx,
                        job_state.clone(),
                        job_reserved.clone(),
                        &run.bundle_url,
                        &run.bundle_sha256,
                        &workspace_root,
                    )
                    .await
                    {
                        error!(%error,"workflow execution failed");
                        let _ = job_tx
                            .send(message(
                                node_id,
                                &asset,
                                &run.workflow_run_id,
                                node_message::Payload::WorkflowFailed(WorkflowFailed {
                                    error: error.to_string(),
                                    cancelled: false,
                                    timed_out: false,
                                }),
                            ))
                            .await;
                        let final_state = if *job_reserved.read().await {
                            NodeState::Reserved
                        } else {
                            NodeState::Available
                        };
                        *job_state.write().await = final_state;
                        let _ = send_state(
                            &job_tx,
                            node_id,
                            &asset,
                            &run.workflow_run_id,
                            final_state,
                            "workflow preparation failed",
                        )
                        .await;
                    }
                });
            }
            Some(control_message::Payload::CancelWorkflow(cancel)) => {
                if let Some((id, cancel_tx)) = &current_job {
                    if *id == cancel.workflow_run_id {
                        let _ = cancel_tx.send(true);
                    }
                }
            }
            Some(control_message::Payload::ExecuteRemediation(request)) => {
                if !control
                    .envelope
                    .as_ref()
                    .is_some_and(|env| env.asset_id == config.asset_id)
                {
                    warn!("remediation envelope target mismatch");
                    continue;
                }
                recovery::handle_request(
                    recovery.clone(),
                    request,
                    tx.clone(),
                    node_id,
                    config.asset_id.clone(),
                    current_job.is_some() || *reserved.read().await,
                    remediation_active.clone(),
                )
                .await?;
            }
            Some(control_message::Payload::Ping(_)) | None => {}
        }
    }
    drop(heartbeat);
    drop(recovery_monitor);
    anyhow::bail!("control plane closed stream")
}

#[allow(clippy::too_many_arguments)]
async fn execute_workflow(
    tx: mpsc::Sender<NodeMessage>,
    node: Uuid,
    asset: &str,
    run: &str,
    yaml: &str,
    cancel: watch::Receiver<bool>,
    state: Arc<RwLock<NodeState>>,
    reserved: Arc<RwLock<bool>>,
    bundle_url: &str,
    bundle_sha256: &str,
    workspace_root: &PathBuf,
) -> anyhow::Result<()> {
    let workflow = WorkflowDefinition::parse(yaml)?;
    let workspace = prepare_workspace(workspace_root, run, bundle_url, bundle_sha256).await?;
    *state.write().await = NodeState::Running;
    tx.send(message(
        node,
        asset,
        run,
        node_message::Payload::WorkflowStarted(WorkflowStarted {}),
    ))
    .await?;
    send_state(&tx, node, asset, run, NodeState::Running, "").await?;
    let executor = ShellExecutor;
    let mut failed = false;
    let mut cancelled = false;
    let mut timed_out = false;
    let mut error_text = String::new();
    let mut position = 0;
    for (phase, steps) in [
        ("prepare", &workflow.prepare),
        ("deploy", &workflow.deploy),
        ("configure", &workflow.configure),
        ("test", &workflow.test),
        ("collect", &workflow.collect),
    ] {
        for step in steps {
            if failed || *cancel.borrow() {
                cancelled = *cancel.borrow();
                break;
            }
            let outcome = run_step(
                &executor,
                &tx,
                node,
                asset,
                run,
                phase,
                position,
                step,
                cancel.clone(),
                &workspace,
                &workflow.environment.variables,
            )
            .await?;
            position += 1;
            cancelled |= *cancel.borrow() || outcome.error == "cancelled";
            timed_out |= outcome.timed_out;
            if !outcome.success && !step.continue_on_error {
                failed = true;
                error_text = outcome.error;
                if error_text.is_empty() {
                    error_text = format!("step '{}' exited {}", step.name, outcome.exit_code);
                }
            }
        }
        if failed || cancelled {
            break;
        }
    }
    *state.write().await = NodeState::CleaningUp;
    send_state(&tx, node, asset, run, NodeState::CleaningUp, "").await?;
    // Cleanup deliberately gets a fresh cancellation token so cancellation still performs cleanup.
    let (_cleanup_tx, cleanup_cancel) = watch::channel(false);
    for step in &workflow.cleanup {
        let outcome = run_step(
            &executor,
            &tx,
            node,
            asset,
            run,
            "cleanup",
            position,
            step,
            cleanup_cancel.clone(),
            &workspace,
            &workflow.environment.variables,
        )
        .await?;
        position += 1;
        timed_out |= outcome.timed_out;
        if !outcome.success && !step.continue_on_error {
            failed = true;
            if error_text.is_empty() {
                error_text = format!("cleanup step '{}' failed", step.name);
            }
        }
    }
    collect_artifacts(&tx, node, asset, run, &workspace, &workflow.artifacts).await?;
    if failed || cancelled {
        tx.send(message(
            node,
            asset,
            run,
            node_message::Payload::WorkflowFailed(WorkflowFailed {
                error: error_text,
                cancelled,
                timed_out,
            }),
        ))
        .await?;
    } else {
        tx.send(message(
            node,
            asset,
            run,
            node_message::Payload::WorkflowCompleted(WorkflowCompleted {}),
        ))
        .await?;
    }
    let final_state = if *reserved.read().await {
        NodeState::Reserved
    } else {
        NodeState::Available
    };
    *state.write().await = final_state;
    send_state(&tx, node, asset, run, final_state, "").await?;
    if let Err(error) = tokio::fs::remove_dir_all(&workspace).await {
        warn!(%error, path=%workspace.display(), "failed to remove run workspace");
    }
    Ok(())
}

async fn prepare_workspace(
    root: &PathBuf,
    run: &str,
    bundle_url: &str,
    expected_sha256: &str,
) -> anyhow::Result<PathBuf> {
    let run_id = Uuid::parse_str(run).context("invalid workflow run id")?;
    tokio::fs::create_dir_all(root).await?;
    let workspace = root.join(run_id.to_string());
    tokio::fs::create_dir(&workspace)
        .await
        .context("create isolated run workspace")?;
    if bundle_url.is_empty() {
        return Ok(workspace);
    }
    let response = reqwest::get(bundle_url)
        .await
        .context("download software bundle")?
        .error_for_status()
        .context("software bundle download failed")?;
    if response
        .content_length()
        .is_some_and(|size| size > 100 * 1024 * 1024)
    {
        anyhow::bail!("software bundle exceeds 100 MiB limit");
    }
    let bytes = response.bytes().await.context("read software bundle")?;
    if bytes.len() > 100 * 1024 * 1024 {
        anyhow::bail!("software bundle exceeds 100 MiB limit");
    }
    let actual_sha256 = format!("{:x}", Sha256::digest(&bytes));
    if actual_sha256 != expected_sha256 {
        anyhow::bail!("software bundle checksum mismatch");
    }
    let destination = workspace.clone();
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let decoder = GzDecoder::new(bytes.as_ref());
        let mut archive = tar::Archive::new(decoder);
        archive.set_preserve_permissions(true);
        archive
            .unpack(&destination)
            .context("extract software bundle")?;
        Ok(())
    })
    .await
    .context("bundle extraction task failed")??;
    Ok(workspace)
}

#[allow(clippy::too_many_arguments)]
async fn run_step(
    executor: &dyn Executor,
    tx: &mpsc::Sender<NodeMessage>,
    node: Uuid,
    asset: &str,
    run: &str,
    phase: &str,
    position: i32,
    step: &WorkflowStep,
    cancel: watch::Receiver<bool>,
    workspace: &PathBuf,
    workflow_environment: &std::collections::BTreeMap<String, String>,
) -> anyhow::Result<executor::StepOutcome> {
    let step_id = Uuid::new_v4().to_string();
    tx.send(message(
        node,
        asset,
        run,
        node_message::Payload::StepStarted(StepStarted {
            step_id: step_id.clone(),
            phase: phase.into(),
            name: step.name.clone(),
            position,
            command: step.run.clone(),
        }),
    ))
    .await?;
    let (output_tx, mut output_rx) = mpsc::channel(64);
    let result = executor.execute(
        ExecutionRequest {
            step,
            workspace,
            workflow_environment,
        },
        output_tx,
        cancel,
    );
    tokio::pin!(result);
    let mut sequence = 0;
    let outcome = loop {
        tokio::select! {Some(event)=output_rx.recv()=>{sequence+=1;let (payload,data)=match event{OutputEvent::Stdout(d)=>(0,d),OutputEvent::Stderr(d)=>(1,d)};let log=LogChunk{step_id:step_id.clone(),data,sequence};tx.send(message(node,asset,run,if payload==0{node_message::Payload::Stdout(log)}else{node_message::Payload::Stderr(log)})).await?;},value=&mut result=>break value?}
    };
    while let Ok(event) = output_rx.try_recv() {
        sequence += 1;
        let (stdout, data) = match event {
            OutputEvent::Stdout(d) => (true, d),
            OutputEvent::Stderr(d) => (false, d),
        };
        let log = LogChunk {
            step_id: step_id.clone(),
            data,
            sequence,
        };
        tx.send(message(
            node,
            asset,
            run,
            if stdout {
                node_message::Payload::Stdout(log)
            } else {
                node_message::Payload::Stderr(log)
            },
        ))
        .await?;
    }
    tx.send(message(
        node,
        asset,
        run,
        node_message::Payload::StepCompleted(StepCompleted {
            step_id,
            exit_code: outcome.exit_code,
            success: outcome.success,
            error: outcome.error.clone(),
            duration_ms: outcome.duration_ms,
            timed_out: outcome.timed_out,
        }),
    ))
    .await?;
    Ok(outcome)
}

async fn collect_artifacts(
    tx: &mpsc::Sender<NodeMessage>,
    node: Uuid,
    asset: &str,
    run: &str,
    workspace: &PathBuf,
    patterns: &[String],
) -> anyhow::Result<()> {
    let canonical_workspace = tokio::fs::canonicalize(workspace).await?;
    for pattern in patterns {
        let expanded = if pattern.ends_with("/**") {
            format!("{pattern}/*")
        } else {
            pattern.clone()
        };
        let absolute = workspace.join(expanded).to_string_lossy().into_owned();
        for entry in glob::glob(&absolute).context("invalid artifact glob")? {
            let path = entry.context("artifact glob failed")?;
            if !path.is_file() {
                continue;
            }
            let canonical = tokio::fs::canonicalize(&path).await?;
            if !canonical.starts_with(&canonical_workspace) {
                anyhow::bail!("artifact escaped run workspace");
            }
            let metadata = tokio::fs::metadata(&canonical).await?;
            if metadata.len() > 10 * 1024 * 1024 {
                warn!(path=%path.display(), "skipping artifact larger than 10 MiB");
                continue;
            }
            let relative = canonical
                .strip_prefix(&canonical_workspace)?
                .to_string_lossy()
                .into_owned();
            let artifact = ArtifactUploaded {
                artifact_id: Uuid::new_v4().to_string(),
                name: path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
                path: relative,
                content_type: mime_guess::from_path(&path)
                    .first_or_octet_stream()
                    .to_string(),
                data: tokio::fs::read(&canonical).await?,
            };
            tx.send(message(
                node,
                asset,
                run,
                node_message::Payload::ArtifactUploaded(artifact),
            ))
            .await?;
        }
    }
    Ok(())
}

async fn send_state(
    tx: &mpsc::Sender<NodeMessage>,
    node: Uuid,
    asset: &str,
    run: &str,
    state: NodeState,
    detail: &str,
) -> anyhow::Result<()> {
    tx.send(message(
        node,
        asset,
        run,
        node_message::Payload::StateChanged(StateChanged {
            state: state as i32,
            detail: detail.into(),
        }),
    ))
    .await?;
    Ok(())
}
fn message(node: Uuid, asset: &str, run: &str, payload: node_message::Payload) -> NodeMessage {
    NodeMessage {
        envelope: Some(Envelope {
            node_id: node.to_string(),
            asset_id: asset.into(),
            workflow_run_id: run.into(),
            timestamp_ms: Utc::now().timestamp_millis(),
        }),
        payload: Some(payload),
    }
}
fn inventory() -> Inventory {
    let mut system = System::new_all();
    system.refresh_all();
    let networks = Networks::new_with_refreshed_list();
    Inventory {
        hostname: hostname::get()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        os: System::name().unwrap_or_else(|| "unknown".into()),
        kernel: System::kernel_version().unwrap_or_else(|| "unknown".into()),
        architecture: std::env::consts::ARCH.into(),
        cpu_count: system.cpus().len() as u32,
        ram_bytes: system.total_memory(),
        network_interfaces: networks.keys().cloned().collect(),
    }
}
fn detected_capabilities() -> serde_json::Value {
    let command_exists = |name: &str| {
        std::process::Command::new("/usr/bin/env")
            .args(["sh", "-c", &format!("command -v {name} >/dev/null 2>&1")])
            .status()
            .is_ok_and(|status| status.success())
    };
    serde_json::json!({
        "architecture": std::env::consts::ARCH,
        "docker": command_exists("docker"), "nix": command_exists("nix"),
        "systemd": std::path::Path::new("/run/systemd/system").exists(),
        "nvidia": command_exists("nvidia-smi"),
        "jetson": std::path::Path::new("/etc/nv_tegra_release").exists()
    })
}
async fn load_identity(path: &PathBuf) -> anyhow::Result<Uuid> {
    match tokio::fs::read_to_string(path).await {
        Ok(raw) => Ok(Uuid::parse_str(raw.trim()).context("invalid node identity file")?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let id = Uuid::new_v4();
            tokio::fs::write(path, id.to_string())
                .await
                .context("write node identity")?;
            Ok(id)
        }
        Err(e) => Err(e.into()),
    }
}
