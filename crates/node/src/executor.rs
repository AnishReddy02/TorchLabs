use async_trait::async_trait;
use bench_common::WorkflowStep;
use std::{
    collections::BTreeMap,
    path::PathBuf,
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
    sync::{mpsc, watch},
};

pub enum OutputEvent {
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
}
pub struct ExecutionRequest<'a> {
    pub step: &'a WorkflowStep,
    pub workspace: &'a PathBuf,
    pub workflow_environment: &'a BTreeMap<String, String>,
}
pub struct StepOutcome {
    pub exit_code: i32,
    pub success: bool,
    pub error: String,
    pub duration_ms: u64,
    pub timed_out: bool,
}

#[async_trait]
pub trait Executor: Send + Sync {
    async fn execute(
        &self,
        request: ExecutionRequest<'_>,
        output: mpsc::Sender<OutputEvent>,
        cancel: watch::Receiver<bool>,
    ) -> anyhow::Result<StepOutcome>;
}

pub struct ShellExecutor;

#[async_trait]
impl Executor for ShellExecutor {
    async fn execute(
        &self,
        request: ExecutionRequest<'_>,
        output: mpsc::Sender<OutputEvent>,
        mut cancel: watch::Receiver<bool>,
    ) -> anyhow::Result<StepOutcome> {
        let step = request.step;
        let working_dir = match &step.working_directory {
            Some(relative) => request.workspace.join(relative),
            None => request.workspace.clone(),
        };
        let started = Instant::now();
        // V1 workflows are trusted authenticated input. /bin/sh remains an explicit
        // replaceable boundary; production deployments still require sandboxing.
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg(&step.run)
            .current_dir(working_dir)
            .envs(request.workflow_environment)
            .envs(&step.environment)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command.spawn()?;
        let stdout_pipe = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("stdout pipe unavailable"))?;
        let stderr_pipe = child
            .stderr
            .take()
            .ok_or_else(|| anyhow::anyhow!("stderr pipe unavailable"))?;
        let stdout = tokio::spawn(copy_output(stdout_pipe, output.clone(), true));
        let stderr = tokio::spawn(copy_output(stderr_pipe, output, false));
        let (exit_code, success, error, timed_out) = tokio::select! {
            status=child.wait()=>{let status=status?;(status.code().unwrap_or(-1),status.success(),String::new(),false)},
            _=tokio::time::sleep(Duration::from_secs(step.timeout_seconds))=>{terminate_tree(&mut child).await?;(-1,false,format!("timed out after {} seconds",step.timeout_seconds),true)},
            changed=cancel.changed()=>{changed?;terminate_tree(&mut child).await?;(-1,false,"cancelled".into(),false)},
        };
        let _ = stdout.await;
        let _ = stderr.await;
        Ok(StepOutcome {
            exit_code,
            success,
            error,
            timed_out,
            duration_ms: started.elapsed().as_millis() as u64,
        })
    }
}

async fn terminate_tree(child: &mut tokio::process::Child) -> anyhow::Result<()> {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        // The command is its own process-group leader, so a negative PID targets
        // the shell and every descendant it spawned.
        let result = unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
        if result != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    #[cfg(not(unix))]
    child.kill().await?;
    let _ = child.wait().await;
    Ok(())
}

async fn copy_output(
    mut reader: impl AsyncRead + Unpin,
    tx: mpsc::Sender<OutputEvent>,
    stdout: bool,
) {
    let mut buf = vec![0; 4096];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let event = if stdout {
                    OutputEvent::Stdout(buf[..n].to_vec())
                } else {
                    OutputEvent::Stderr(buf[..n].to_vec())
                };
                if tx.send(event).await.is_err() {
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn step(command: &str, timeout: u64) -> WorkflowStep {
        WorkflowStep {
            name: "test".into(),
            run: command.into(),
            timeout_seconds: timeout,
            continue_on_error: false,
            working_directory: None,
            environment: BTreeMap::new(),
        }
    }
    async fn execute(command: &str, timeout: u64) -> (StepOutcome, Vec<OutputEvent>) {
        let dir = std::env::temp_dir();
        let command = step(command, timeout);
        let env = BTreeMap::new();
        let (tx, mut rx) = mpsc::channel(16);
        let (_cancel_tx, cancel) = watch::channel(false);
        let result = ShellExecutor
            .execute(
                ExecutionRequest {
                    step: &command,
                    workspace: &dir,
                    workflow_environment: &env,
                },
                tx,
                cancel,
            )
            .await
            .unwrap();
        let mut output = Vec::new();
        while let Some(event) = rx.recv().await {
            output.push(event)
        }
        (result, output)
    }
    #[tokio::test]
    async fn captures_stdout_stderr_and_nonzero_exit() {
        let (result, events) = execute("echo out; echo err >&2; exit 7", 5).await;
        assert_eq!(result.exit_code, 7);
        assert!(!result.success);
        assert_eq!(events.len(), 2);
    }
    #[tokio::test]
    async fn kills_timed_out_process() {
        let (result, _) = execute("sleep 2", 1).await;
        assert!(result.timed_out);
        assert!(!result.success);
    }
    #[tokio::test]
    async fn cancellation_kills_process() {
        let dir = std::env::temp_dir();
        let command = step("sleep 10", 30);
        let env = BTreeMap::new();
        let (tx, _) = mpsc::channel(4);
        let (cancel_tx, cancel) = watch::channel(false);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let _ = cancel_tx.send(true);
        });
        let result = ShellExecutor
            .execute(
                ExecutionRequest {
                    step: &command,
                    workspace: &dir,
                    workflow_environment: &env,
                },
                tx,
                cancel,
            )
            .await
            .unwrap();
        assert_eq!(result.error, "cancelled");
    }
}
