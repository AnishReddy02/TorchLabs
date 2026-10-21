use bench_protocol::bench::{
    node_message, Envelope, ExecuteRemediation, NodeMessage, RemediationAcknowledged,
    RemediationCompleted, ServiceObservation,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::BTreeMap,
    path::PathBuf,
    process::Stdio,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{
    io::AsyncReadExt,
    process::Command,
    sync::{mpsc, Mutex},
    task::JoinHandle,
};
use uuid::Uuid;

const ACTION: &str = "RESTART_SERVICE";

#[derive(Clone)]
pub struct RecoveryConfig {
    service_id: String,
    container_name: String,
    health_url: String,
    interval: Duration,
    ledger_path: PathBuf,
    ledger: Arc<Mutex<BTreeMap<String, LedgerEntry>>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct LedgerEntry {
    #[serde(default)]
    started_ms: i64,
    completed: bool,
    success: bool,
    output: String,
    error: String,
}

impl RecoveryConfig {
    pub fn new(
        service_id: &str,
        container_name: &str,
        health_url: &str,
        interval_seconds: u64,
        ledger_path: PathBuf,
    ) -> anyhow::Result<Option<Arc<Self>>> {
        if service_id.is_empty() && container_name.is_empty() && health_url.is_empty() {
            return Ok(None);
        }
        if service_id.is_empty() || service_id.len() > 200 {
            anyhow::bail!("TORCH_RECOVERY_SERVICE_ID must contain 1-200 characters");
        }
        if !valid_container_name(container_name) {
            anyhow::bail!("TORCH_RECOVERY_CONTAINER_NAME contains unsupported characters");
        }
        let url = reqwest::Url::parse(health_url)?;
        if !matches!(url.scheme(), "http" | "https") {
            anyhow::bail!("TORCH_RECOVERY_HEALTH_URL must use HTTP or HTTPS");
        }
        let ledger = match std::fs::read(&ledger_path) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(error) => return Err(error.into()),
        };
        Ok(Some(Arc::new(Self {
            service_id: service_id.into(),
            container_name: container_name.into(),
            health_url: health_url.into(),
            interval: Duration::from_secs(interval_seconds.clamp(1, 60)),
            ledger_path,
            ledger: Arc::new(Mutex::new(ledger)),
        })))
    }
}

pub fn spawn_monitor(
    config: Arc<RecoveryConfig>,
    tx: mpsc::Sender<NodeMessage>,
    node_id: Uuid,
    asset: String,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let client = match reqwest::Client::builder()
            .timeout(Duration::from_secs(3))
            .build()
        {
            Ok(client) => client,
            Err(error) => {
                tracing::error!(%error, "failed to build recovery health client");
                return;
            }
        };
        let mut interval = tokio::time::interval(config.interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            let (healthy, summary) = match client.get(&config.health_url).send().await {
                Ok(response) => (
                    response.status().is_success(),
                    format!(
                        "health endpoint returned HTTP {}",
                        response.status().as_u16()
                    ),
                ),
                Err(error) => (false, format!("health request failed: {error}")),
            };
            let process_state = docker_output(&[
                "inspect",
                "--format",
                "{{.State.Running}}",
                &config.container_name,
            ])
            .await;
            let healthy = healthy && process_state.trim() == "true";
            let evidence = if healthy {
                json!({"health_url":config.health_url,"process_running":true,"observed_at":Utc::now()})
            } else {
                collect_evidence(&config).await
            };
            let observation = ServiceObservation {
                service_id: config.service_id.clone(),
                healthy,
                summary,
                evidence_json: bounded(&evidence.to_string(), 8 * 1024),
            };
            if tx
                .send(message(
                    node_id,
                    &asset,
                    node_message::Payload::ServiceObservation(observation),
                ))
                .await
                .is_err()
            {
                break;
            }
        }
    })
}

pub async fn handle_request(
    config: Option<Arc<RecoveryConfig>>,
    request: ExecuteRemediation,
    tx: mpsc::Sender<NodeMessage>,
    node_id: Uuid,
    asset: String,
    workflow_active: bool,
    remediation_active: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    let rejection = validate_request(config.as_deref(), &request, workflow_active);
    if let Some(reason) = rejection {
        send_ack(&tx, node_id, &asset, &request.operation_id, false, &reason).await?;
        return Ok(());
    }
    let config = config.expect("validated recovery configuration");
    if remediation_active.swap(true, Ordering::SeqCst) {
        send_ack(
            &tx,
            node_id,
            &asset,
            &request.operation_id,
            false,
            "another remediation is already running",
        )
        .await?;
        return Ok(());
    }

    {
        let ledger = config.ledger.lock().await;
        if let Some(previous) = ledger.get(&request.idempotency_key) {
            send_ack(
                &tx,
                node_id,
                &asset,
                &request.operation_id,
                previous.completed,
                "duplicate idempotency key",
            )
            .await?;
            if previous.completed {
                send_completion(
                    &tx,
                    node_id,
                    &asset,
                    &request.operation_id,
                    previous.success,
                    &previous.output,
                    &previous.error,
                )
                .await?;
            }
            remediation_active.store(false, Ordering::SeqCst);
            return Ok(());
        }
        if ledger.values().any(|entry| {
            !entry.completed || Utc::now().timestamp_millis() - entry.started_ms < 60_000
        }) {
            remediation_active.store(false, Ordering::SeqCst);
            send_ack(
                &tx,
                node_id,
                &asset,
                &request.operation_id,
                false,
                "node cooldown or unresolved execution prevents restart",
            )
            .await?;
            return Ok(());
        }
    }
    {
        let mut ledger = config.ledger.lock().await;
        ledger.insert(
            request.idempotency_key.clone(),
            LedgerEntry {
                started_ms: Utc::now().timestamp_millis(),
                completed: false,
                success: false,
                output: String::new(),
                error: String::new(),
            },
        );
        persist_ledger(&config.ledger_path, &ledger).await?;
    }
    send_ack(
        &tx,
        node_id,
        &asset,
        &request.operation_id,
        true,
        "validated against node allowlist",
    )
    .await?;
    tokio::spawn(async move {
        let result = async {
            if tx.is_closed() || std::path::Path::new(".torch-recovery-disabled").exists() {
                anyhow::bail!("connection closed or recovery disabled before execution");
            }
            let health_client = reqwest::Client::builder()
                .timeout(Duration::from_secs(3))
                .build()?;
            if health_client
                .get(&config.health_url)
                .send()
                .await
                .is_ok_and(|response| response.status().is_success())
            {
                anyhow::bail!("restart precondition no longer holds: service is already healthy");
            }
            if tx.is_closed() || std::path::Path::new(".torch-recovery-disabled").exists() {
                anyhow::bail!("connection closed or recovery disabled during precondition check");
            }
            let remaining = request.deadline_ms - Utc::now().timestamp_millis();
            if remaining <= 0 {
                anyhow::bail!("execution deadline expired");
            }
            tokio::time::timeout(
                Duration::from_millis(remaining as u64),
                restart_container(&config.container_name),
            )
            .await?
        }
        .await;
        let (success, output, error) = match result {
            Ok(output) => (true, output, String::new()),
            Err(error) => (false, String::new(), error.to_string()),
        };
        {
            let mut ledger = config.ledger.lock().await;
            ledger.insert(
                request.idempotency_key.clone(),
                LedgerEntry {
                    started_ms: Utc::now().timestamp_millis(),
                    completed: true,
                    success,
                    output: output.clone(),
                    error: error.clone(),
                },
            );
            if let Err(persist_error) = persist_ledger(&config.ledger_path, &ledger).await {
                tracing::error!(%persist_error,"failed to persist remediation ledger");
            }
        }
        if let Err(send_error) = send_completion(
            &tx,
            node_id,
            &asset,
            &request.operation_id,
            success,
            &output,
            &error,
        )
        .await
        {
            tracing::warn!(%send_error,"failed to report remediation completion");
        }
        remediation_active.store(false, Ordering::SeqCst);
    });
    Ok(())
}

fn validate_request(
    config: Option<&RecoveryConfig>,
    request: &ExecuteRemediation,
    workflow_active: bool,
) -> Option<String> {
    let Some(config) = config else {
        return Some("node remediation is not configured".into());
    };
    if Uuid::parse_str(&request.operation_id).is_err()
        || Uuid::parse_str(&request.incident_id).is_err()
    {
        return Some("invalid durable operation identifiers".into());
    }
    if request.action_id != ACTION {
        return Some("action is not allowlisted by the node".into());
    }
    if std::path::Path::new(".torch-recovery-disabled").exists() {
        return Some("node emergency disable file is present".into());
    }
    if request.service_id != config.service_id {
        return Some("service is not allowlisted by the node".into());
    }
    if request.deadline_ms <= Utc::now().timestamp_millis() {
        return Some("remediation request is stale".into());
    }
    if request.deadline_ms > (Utc::now() + chrono::Duration::minutes(5)).timestamp_millis() {
        return Some("remediation deadline exceeds node maximum".into());
    }
    if request.idempotency_key != format!("{}:RESTART_SERVICE:1", request.incident_id) {
        return Some("invalid idempotency key".into());
    }
    if workflow_active {
        return Some("asset is executing a workflow".into());
    }
    None
}

async fn restart_container(container: &str) -> anyhow::Result<String> {
    let mut command = Command::new("docker");
    command
        .args(["restart", "--timeout", "2", container])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(20), command.output()).await??;
    let stdout = bounded(&String::from_utf8_lossy(&output.stdout), 4096);
    let stderr = bounded(&String::from_utf8_lossy(&output.stderr), 4096);
    if !output.status.success() {
        anyhow::bail!("docker restart failed: {stderr}");
    }
    Ok(stdout)
}

async fn collect_evidence(config: &RecoveryConfig) -> serde_json::Value {
    let inspect = docker_output(&[
        "inspect",
        "--format",
        "{{.State.Status}} exit={{.State.ExitCode}} restarts={{.RestartCount}}",
        &config.container_name,
    ])
    .await;
    let logs = docker_output(&["logs", "--tail", "40", &config.container_name]).await;
    json!({"health_url":config.health_url,"container":config.container_name,"container_state":inspect,"recent_logs_untrusted":logs,"observed_at":Utc::now()})
}
async fn docker_output(args: &[&str]) -> String {
    let operation = async {
        let mut child = Command::new("docker")
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let mut bytes = Vec::new();
        child
            .stdout
            .take()
            .unwrap()
            .take(4096)
            .read_to_end(&mut bytes)
            .await?;
        let _ = child.kill().await;
        Ok::<_, std::io::Error>(String::from_utf8_lossy(&bytes).into_owned())
    };
    match tokio::time::timeout(Duration::from_secs(3), operation).await {
        Ok(Ok(output)) => output
            .lines()
            .map(|line| {
                let lower = line.to_ascii_lowercase();
                if [
                    "password",
                    "secret",
                    "token",
                    "api_key",
                    "authorization",
                    "credential",
                ]
                .iter()
                .any(|key| lower.contains(key))
                {
                    "[REDACTED]"
                } else {
                    line
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Ok(Err(error)) => format!("diagnostic command failed: {error}"),
        Err(_) => "diagnostic command timed out".into(),
    }
}

async fn persist_ledger(
    path: &PathBuf,
    ledger: &BTreeMap<String, LedgerEntry>,
) -> anyhow::Result<()> {
    let temporary = path.with_extension("tmp");
    tokio::fs::write(&temporary, serde_json::to_vec(ledger)?).await?;
    tokio::fs::rename(temporary, path).await?;
    Ok(())
}
async fn send_ack(
    tx: &mpsc::Sender<NodeMessage>,
    node: Uuid,
    asset: &str,
    operation: &str,
    accepted: bool,
    reason: &str,
) -> anyhow::Result<()> {
    tx.send(message(
        node,
        asset,
        node_message::Payload::RemediationAcknowledged(RemediationAcknowledged {
            operation_id: operation.into(),
            accepted,
            reason: bounded(reason, 2048),
        }),
    ))
    .await?;
    Ok(())
}
async fn send_completion(
    tx: &mpsc::Sender<NodeMessage>,
    node: Uuid,
    asset: &str,
    operation: &str,
    success: bool,
    output: &str,
    error: &str,
) -> anyhow::Result<()> {
    tx.send(message(
        node,
        asset,
        node_message::Payload::RemediationCompleted(RemediationCompleted {
            operation_id: operation.into(),
            success,
            output: bounded(output, 4096),
            error: bounded(error, 2048),
        }),
    ))
    .await?;
    Ok(())
}
fn message(node: Uuid, asset: &str, payload: node_message::Payload) -> NodeMessage {
    NodeMessage {
        envelope: Some(Envelope {
            node_id: node.to_string(),
            asset_id: asset.into(),
            workflow_run_id: String::new(),
            timestamp_ms: Utc::now().timestamp_millis(),
        }),
        payload: Some(payload),
    }
}
fn bounded(value: &str, limit: usize) -> String {
    value.chars().take(limit).collect()
}
fn valid_container_name(value: &str) -> bool {
    !value.is_empty()
        && value.starts_with(|c: char| c.is_ascii_alphanumeric())
        && value.len() <= 128
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn completed_duplicate_returns_cached_result_without_execution() {
        let path = std::env::temp_dir().join(format!("torch-ledger-{}.json", Uuid::new_v4()));
        let incident = Uuid::new_v4();
        let key = format!("{incident}:RESTART_SERVICE:1");
        let mut ledger = BTreeMap::new();
        ledger.insert(
            key.clone(),
            LedgerEntry {
                started_ms: 0,
                completed: true,
                success: true,
                output: "cached result".into(),
                error: String::new(),
            },
        );
        persist_ledger(&path, &ledger).await.unwrap();
        let config = RecoveryConfig::new(
            "svc",
            "does-not-exist",
            "http://127.0.0.1/health",
            2,
            path.clone(),
        )
        .unwrap();
        let request = ExecuteRemediation {
            operation_id: Uuid::new_v4().to_string(),
            incident_id: incident.to_string(),
            action_id: ACTION.into(),
            service_id: "svc".into(),
            deadline_ms: Utc::now().timestamp_millis() + 30_000,
            idempotency_key: key,
        };
        let (tx, mut rx) = mpsc::channel(4);
        handle_request(
            config,
            request,
            tx,
            Uuid::new_v4(),
            "asset".into(),
            false,
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
        assert!(matches!(
            rx.recv().await.unwrap().payload,
            Some(node_message::Payload::RemediationAcknowledged(_))
        ));
        let result = rx.recv().await.unwrap();
        assert!(
            matches!(result.payload,Some(node_message::Payload::RemediationCompleted(value)) if value.output=="cached result")
        );
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn container_target_is_narrowly_validated() {
        assert!(valid_container_name("torch-recovery-demo"));
        assert!(!valid_container_name("demo; shutdown -h now"));
        assert!(!valid_container_name(""));
    }
    #[test]
    fn unknown_action_and_stale_request_are_rejected() {
        let config = RecoveryConfig::new(
            "svc",
            "container",
            "http://127.0.0.1/health",
            2,
            PathBuf::from("ledger.json"),
        )
        .unwrap()
        .unwrap();
        let mut request = ExecuteRemediation {
            operation_id: Uuid::new_v4().to_string(),
            incident_id: Uuid::new_v4().to_string(),
            action_id: "RUN_COMMAND".into(),
            service_id: "svc".into(),
            deadline_ms: (Utc::now() + chrono::Duration::seconds(30)).timestamp_millis(),
            idempotency_key: "key".into(),
        };
        assert!(validate_request(Some(&config), &request, false).is_some());
        request.action_id = ACTION.into();
        request.deadline_ms = Utc::now().timestamp_millis() - 1;
        assert!(validate_request(Some(&config), &request, false).is_some());
    }
}
