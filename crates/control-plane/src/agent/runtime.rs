use super::{
    event,
    model::{Action, AgentModel, OpenAiModel},
    tools::{policy, Policy, Tools},
};
use crate::api::AppState;
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use sqlx::{FromRow, PgPool};
use std::{sync::Arc, time::Duration};
use uuid::Uuid;

#[derive(FromRow)]
pub struct Session {
    pub id: Uuid,
    pub prompt: String,
    pub status: String,
    pub context: Value,
    pub pending: Option<Value>,
    pub decision: Option<bool>,
    pub cancel_requested: bool,
    pub paused: bool,
    pub control_version: i64,
    pub expired: bool,
    pub outcome: String,
}
pub async fn load(pool: &PgPool, id: Uuid) -> Result<Session> {
    Ok(
        sqlx::query_as("SELECT *,deadline<=now() AS expired FROM agent_sessions WHERE id=$1")
            .bind(id)
            .fetch_one(pool)
            .await?,
    )
}

// One worker per control-plane process. Durable ownership is encoded in reservation
// owner and workflow_runs.agent_session_id, including the crash window after dispatch.
pub async fn start_worker(state: AppState, token: String) -> Result<tokio::task::JoinHandle<()>> {
    sqlx::query("UPDATE agent_sessions SET status='CLEANING_UP',outcome='FAILED',error='Control plane restarted; interrupted agent will clean up' WHERE status IN ('RUNNING','WAITING_FOR_APPROVAL')").execute(&state.pool).await?;
    let tools = Tools::new(state, token);
    let model = OpenAiModel::from_env()
        .ok()
        .map(|m| Arc::new(m) as Arc<dyn AgentModel>);
    Ok(tokio::spawn(async move {
        loop {
            let ids=sqlx::query_scalar::<_,Uuid>("SELECT id FROM agent_sessions WHERE status IN ('CREATED','RUNNING','WAITING_FOR_APPROVAL','CLEANING_UP') ORDER BY created_at").fetch_all(&tools.state.pool).await;
            match ids {
                Ok(ids) => {
                    for id in ids {
                        if let Err(error) = tick(&tools, model.as_deref(), id).await {
                            tracing::error!(session_id=%id,%error,"agent turn failed");
                            if let Err(e) = fail(&tools, id, &error.to_string()).await {
                                tracing::error!(%e,"persisting agent failure failed; will retry");
                            }
                        }
                    }
                }
                Err(error) => tracing::error!(%error,"agent worker query failed"),
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }))
}
pub async fn fail(tools: &Tools, id: Uuid, error: &str) -> Result<()> {
    sqlx::query("UPDATE agent_sessions SET status='CLEANING_UP',outcome='FAILED',error=$2,updated_at=now() WHERE id=$1").bind(id).bind(error).execute(&tools.state.pool).await?;
    event(
        &tools.state.pool,
        id,
        "FAILED",
        json!({"message":error,"cleanup":"pending"}),
    )
    .await
}
pub async fn tick(tools: &Tools, model: Option<&dyn AgentModel>, id: Uuid) -> Result<()> {
    let pool = &tools.state.pool;
    let mut s = load(pool, id).await?;
    if s.cancel_requested || s.expired {
        sqlx::query("UPDATE agent_sessions SET status='CLEANING_UP',outcome='CANCELLED',error=$2 WHERE id=$1").bind(id).bind(if s.expired {"Agent deadline exceeded"}else{"Cancelled by user"}).execute(pool).await?;
        s.status = "CLEANING_UP".into();
        s.outcome = "CANCELLED".into();
    }
    if s.status == "CLEANING_UP" {
        match tools.cleanup(id).await {
            Ok(()) => {
                sqlx::query("UPDATE agent_sessions SET status=outcome,pending=NULL,updated_at=now() WHERE id=$1").bind(id).execute(pool).await?;
                event(pool,id,&s.outcome,json!({"message":"Session finished; reservation cleanup complete. Check workflow cleanup step results for physical cleanup confirmation."})).await?;
            }
            Err(error) => {
                tracing::warn!(session_id=%id,%error,"agent cleanup pending; retrying");
            }
        }
        return Ok(());
    }
    if s.paused {
        return Ok(());
    }
    if s.status == "WAITING_FOR_APPROVAL" && s.decision.is_none() {
        return Ok(());
    }
    if s.status == "CREATED" {
        sqlx::query("UPDATE agent_sessions SET status='RUNNING' WHERE id=$1")
            .bind(id)
            .execute(pool)
            .await?;
        for action in [
            Action::ListAssets {},
            Action::ListSoftware {},
            Action::ListWorkflows {},
        ] {
            let result = tools.execute(id, &action, false).await?;
            observe(pool, &mut s, &action, result).await?;
        }
        return Ok(());
    }
    if s.context.as_array().map_or(0, Vec::len) > 24 {
        bail!("Agent action limit reached");
    }
    // Waiting for a known run is orchestration, not reasoning. Keeping polling and
    // evidence collection here avoids spending model requests on deterministic work.
    if s.status == "RUNNING" && advance_run(tools, &mut s).await? {
        return Ok(());
    }
    let (action, approved) = if s.status == "WAITING_FOR_APPROVAL" {
        let pending = s.pending.as_ref().context("Missing pending action")?;
        let action: Action = serde_json::from_value(pending["action"].clone())?;
        if s.decision == Some(false) {
            observe(
                pool,
                &mut s,
                &action,
                json!({"denied":true,"message":"Human denied deployment. Do not retry it."}),
            )
            .await?;
            clear_pending(pool, id).await?;
            return Ok(());
        }
        let fresh = tools.approval_snapshot(id, &action).await?;
        if fresh["workflow_hash"] != pending["workflow_hash"]
            || fresh["bundle"] != pending["bundle"]
        {
            bail!("Deployment inputs changed after approval; start a new session");
        }
        (action, true)
    } else {
        let model = model.context("OPENAI_API_KEY missing; agent unavailable")?;
        let action = model.respond(&s.prompt, &s.context).await?;
        // Cancellation may arrive while the model is in flight. No new tool then runs.
        let current = load(pool, id).await?;
        if current.cancel_requested
            || current.expired
            || current.paused
            || current.control_version != s.control_version
        {
            return Ok(());
        }
        event(pool, id, "TOOL_REQUESTED", json!({"action":action})).await?;
        let repeats = s.context.as_array().map_or(0, |history| {
            history
                .iter()
                .rev()
                .take(6)
                .filter(|item| item["action"] == json!(action))
                .count()
        });
        if repeats >= 2 && !matches!(action, Action::InspectRun { .. }) {
            bail!("Model repeated an action with identical arguments; stopped to prevent an unsafe loop");
        }
        (action, false)
    };
    if let Action::Finish { diagnosis } = &action {
        let failed:Vec<Uuid>=sqlx::query_scalar("SELECT id FROM workflow_runs WHERE agent_session_id=$1 AND status IN ('FAILED','TIMED_OUT')").bind(id).fetch_all(pool).await?;
        for run_id in failed {
            for tool in ["inspect_run", "read_run_logs", "list_artifacts"] {
                let observed = s.context.as_array().is_some_and(|history| {
                    history.iter().any(|item| {
                        item["action"]["tool"] == tool
                            && item["action"]["run_id"] == run_id.to_string()
                            && match tool {
                                "inspect_run" => item["result"]["id"] == run_id.to_string(),
                                "read_run_logs" => item["result"]["logs"].is_array(),
                                _ => item["result"].is_array(),
                            }
                    })
                });
                if !observed {
                    observe(pool,&mut s,&action,json!({"error":"Read failed run evidence before reporting","required_tool":tool,"run_id":run_id})).await?;
                    return Ok(());
                }
            }
        }
        if !(0.0..=1.0).contains(&diagnosis.confidence) || diagnosis.summary.len() > 4000 {
            bail!("Invalid diagnosis");
        }
        sqlx::query("UPDATE agent_sessions SET status='CLEANING_UP',report=$2 WHERE id=$1")
            .bind(id)
            .bind(json!(diagnosis))
            .execute(pool)
            .await?;
        event(pool, id, "AGENT_MESSAGE", json!(diagnosis)).await?;
        return Ok(());
    }
    let snapshot = if matches!(action, Action::StartWorkflow { .. }) {
        Some(tools.approval_snapshot(id, &action).await?)
    } else {
        None
    };
    if (policy(&action) == Policy::Approval
        || snapshot
            .as_ref()
            .is_some_and(|s| s["software_changes"] == true))
        && !approved
    {
        let pending = snapshot.context("Missing deployment snapshot")?;
        sqlx::query("UPDATE agent_sessions SET status='WAITING_FOR_APPROVAL',pending=$2,decision=NULL,updated_at=now() WHERE id=$1").bind(id).bind(&pending).execute(pool).await?;
        event(pool, id, "APPROVAL_REQUIRED", pending).await?;
        return Ok(());
    }
    event(pool, id, "TOOL_STARTED", json!({"action":action})).await?;
    let result = tools.execute(id, &action, approved).await;
    let observation = match result {
        Ok(value) => {
            event(
                pool,
                id,
                "TOOL_SUCCEEDED",
                json!({"action":action,"result":value}),
            )
            .await?;
            value
        }
        Err(error) => {
            event(
                pool,
                id,
                "TOOL_FAILED",
                json!({"action":action,"error":error.to_string()}),
            )
            .await?;
            json!({"error":error.to_string()})
        }
    };
    observe(pool, &mut s, &action, observation).await?;
    if approved {
        clear_pending(pool, id).await?;
    }
    Ok(())
}

async fn advance_run(tools: &Tools, s: &mut Session) -> Result<bool> {
    let history = s.context.as_array().context("Invalid agent context")?;
    let run_id = history.iter().rev().find_map(|item| {
        if item["action"]["tool"] != "start_workflow" {
            return None;
        }
        item["result"]["id"].as_str()?.parse::<Uuid>().ok()
    });
    let Some(run_id) = run_id else {
        return Ok(false);
    };
    let status = history.iter().rev().find_map(|item| {
        (item["action"]["tool"] == "inspect_run" && item["action"]["run_id"] == run_id.to_string())
            .then(|| item["result"]["status"].as_str())
            .flatten()
    });
    let action = match status {
        None | Some("QUEUED" | "PREPARING" | "RUNNING" | "CLEANING_UP") => {
            Some(Action::InspectRun { run_id })
        }
        Some("FAILED" | "TIMED_OUT") => {
            let observed = |tool: &str| {
                history.iter().any(|item| {
                    item["action"]["tool"] == tool
                        && item["action"]["run_id"] == run_id.to_string()
                        && item["result"].get("error").is_none()
                })
            };
            if !observed("read_run_logs") {
                Some(Action::ReadRunLogs { run_id })
            } else if !observed("list_artifacts") {
                Some(Action::ListArtifacts { run_id })
            } else {
                None
            }
        }
        _ => None,
    };
    let Some(action) = action else {
        return Ok(false);
    };
    event(
        &tools.state.pool,
        s.id,
        "TOOL_STARTED",
        json!({"action":action,"automated":true}),
    )
    .await?;
    let result = tools.execute(s.id, &action, false).await;
    let observation = match result {
        Ok(value) => {
            event(
                &tools.state.pool,
                s.id,
                "TOOL_SUCCEEDED",
                json!({"action":action,"result":value,"automated":true}),
            )
            .await?;
            value
        }
        Err(error) => {
            event(
                &tools.state.pool,
                s.id,
                "TOOL_FAILED",
                json!({"action":action,"error":error.to_string(),"automated":true}),
            )
            .await?;
            json!({"error":error.to_string()})
        }
    };
    observe(&tools.state.pool, s, &action, observation).await?;
    Ok(true)
}

async fn clear_pending(pool: &PgPool, id: Uuid) -> Result<()> {
    sqlx::query(
        "UPDATE agent_sessions SET status='RUNNING',pending=NULL,decision=NULL WHERE id=$1",
    )
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}
async fn observe(pool: &PgPool, s: &mut Session, action: &Action, result: Value) -> Result<()> {
    s.context
        .as_array_mut()
        .context("Invalid agent context")?
        .push(json!({"action":action,"result":result}));
    if s.context.to_string().len() > 350_000 {
        bail!("Agent context limit reached");
    }
    sqlx::query("UPDATE agent_sessions SET context=$2,updated_at=now() WHERE id=$1")
        .bind(s.id)
        .bind(&s.context)
        .execute(pool)
        .await?;
    Ok(())
}
