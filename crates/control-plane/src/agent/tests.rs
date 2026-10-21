use super::{
    model::Action,
    tools::{policy, Policy},
};
use super::{
    model::{AgentModel, Diagnosis},
    runtime,
    tools::Tools,
};
use crate::{api::AppState, grpc::NodeHub};
use axum::{
    extract::{Path, State},
    http::HeaderMap,
    Json,
};
use serde_json::json;
use serde_json::Value;
use sqlx::PgPool;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

struct TestModel(Mutex<Vec<Action>>);
#[async_trait::async_trait]
impl AgentModel for TestModel {
    async fn respond(&self, _: &str, _: &Value) -> anyhow::Result<Action> {
        self.0
            .lock()
            .unwrap()
            .pop()
            .ok_or_else(|| anyhow::anyhow!("simulated model/API failure"))
    }
}
fn finish() -> Action {
    Action::Finish {
        diagnosis: Diagnosis {
            summary: "Test fixture report".into(),
            likely_cause: "not applicable".into(),
            evidence: vec![],
            recommended_next_action: "none".into(),
            confidence: 0.5,
        },
    }
}
async fn fixture(pool: PgPool) -> (Tools, Uuid, Uuid) {
    let session:Uuid=sqlx::query_scalar("INSERT INTO agent_sessions(prompt,status) VALUES('integration test','RUNNING') RETURNING id").fetch_one(&pool).await.unwrap();
    let asset: Uuid = sqlx::query_scalar(
        "INSERT INTO assets(name,status) VALUES('test-rover','AVAILABLE') RETURNING id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let state = AppState {
        pool,
        hub: Arc::new(NodeHub::default()),
        bundle_dir: Arc::new(std::env::temp_dir()),
        artifact_dir: Arc::new(std::env::temp_dir()),
        bundle_base_url: "http://localhost:8080".into(),
        admin_token_hash: bench_common::hash_token("test-token"),
        recovery_policy: Arc::new(crate::recovery::RecoveryPolicy::from_env()),
    };
    (Tools::new(state, "test-token".into()), session, asset)
}
fn headers() -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert("authorization", "Bearer test-token".parse().unwrap());
    h
}
async fn reserve(tools: &Tools, id: Uuid, asset: Uuid) {
    tools
        .execute(id, &Action::ReserveAsset { asset_id: asset }, false)
        .await
        .unwrap();
}
async fn deployment(tools: &Tools, id: Uuid, asset: Uuid) -> Action {
    reserve(tools, id, asset).await;
    let workflow = tools
        .api(
            "POST",
            "/workflows",
            json!({"definition_yaml":"name: test\ntest:\n- name: test\n  run: echo test\n"}),
        )
        .await
        .unwrap();
    let workflow_id: Uuid = serde_json::from_value(workflow["id"].clone()).unwrap();
    let _ = super::approve_workflow(State(tools.state.clone()), headers(), Path(workflow_id))
        .await
        .unwrap();
    let bundle_id:Uuid=sqlx::query_scalar("INSERT INTO software_bundles(id,name,filename,content_type,size_bytes,sha256,storage_key) VALUES(gen_random_uuid(),'test','test.tgz','application/gzip',1,'test','test.tgz') RETURNING id").fetch_one(&tools.state.pool).await.unwrap();
    Action::StartWorkflow {
        asset_id: asset,
        workflow_id,
        bundle_id: Some(bundle_id),
        reason: "test deployment".into(),
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn successful_agent_releases_and_persists_events(pool: PgPool) {
    let (tools, id, asset) = fixture(pool.clone()).await;
    let model = TestModel(Mutex::new(vec![
        finish(),
        Action::ReserveAsset { asset_id: asset },
        Action::ListAssets {},
    ]));
    for _ in 0..4 {
        runtime::tick(&tools, Some(&model), id).await.unwrap();
    }
    assert_eq!(runtime::load(&pool, id).await.unwrap().status, "COMPLETED");
    assert!(tools.reservation(id).await.unwrap().is_none());
    let Json(detail) = super::detail(State(tools.state.clone()), headers(), Path(id))
        .await
        .unwrap();
    assert!(detail["events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["kind"] == "TOOL_SUCCEEDED"));
    assert!(detail["report"].is_object());
}

#[sqlx::test(migrations = "../../migrations")]
async fn model_error_and_cancellation_release_reservations(pool: PgPool) {
    let (tools, id, asset) = fixture(pool.clone()).await;
    reserve(&tools, id, asset).await;
    let model = TestModel(Mutex::new(vec![]));
    let error = runtime::tick(&tools, Some(&model), id).await.unwrap_err();
    runtime::fail(&tools, id, &error.to_string()).await.unwrap();
    runtime::tick(&tools, None, id).await.unwrap();
    assert_eq!(runtime::load(&pool, id).await.unwrap().status, "FAILED");
    assert!(tools.reservation(id).await.unwrap().is_none());
    reserve(&tools, id, asset).await;
    sqlx::query("UPDATE agent_sessions SET status='RUNNING',cancel_requested=true WHERE id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    runtime::tick(&tools, None, id).await.unwrap();
    assert_eq!(runtime::load(&pool, id).await.unwrap().status, "CANCELLED");
    assert!(tools.reservation(id).await.unwrap().is_none());
}

#[sqlx::test(migrations = "../../migrations")]
async fn approval_pauses_and_denial_is_observed_by_model(pool: PgPool) {
    let (tools, id, asset) = fixture(pool.clone()).await;
    let action = deployment(&tools, id, asset).await;
    assert!(tools.execute(id, &action, false).await.is_err());
    let model = TestModel(Mutex::new(vec![action]));
    runtime::tick(&tools, Some(&model), id).await.unwrap();
    let session = runtime::load(&pool, id).await.unwrap();
    assert_eq!(session.status, "WAITING_FOR_APPROVAL");
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM workflow_runs")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    let request_id =
        serde_json::from_value(session.pending.unwrap()["request_id"].clone()).unwrap();
    let _ = super::decide(
        State(tools.state.clone()),
        headers(),
        Path(id),
        Json(super::Decision {
            approved: false,
            request_id,
        }),
    )
    .await
    .unwrap();
    runtime::tick(&tools, Some(&model), id).await.unwrap();
    let s = runtime::load(&pool, id).await.unwrap();
    assert_eq!(s.status, "RUNNING");
    assert_eq!(s.context[0]["result"]["denied"], true);
    assert!(super::decide(
        State(tools.state.clone()),
        headers(),
        Path(id),
        Json(super::Decision {
            approved: true,
            request_id
        })
    )
    .await
    .is_err());
    runtime::tick(&tools, Some(&TestModel(Mutex::new(vec![finish()]))), id)
        .await
        .unwrap();
    runtime::tick(&tools, None, id).await.unwrap();
    assert!(tools.reservation(id).await.unwrap().is_none());
}

#[sqlx::test(migrations = "../../migrations")]
async fn approval_resumes_exact_action_and_dispatch_failure_cleans_up(pool: PgPool) {
    let (tools, id, asset) = fixture(pool.clone()).await;
    let action = deployment(&tools, id, asset).await;
    runtime::tick(&tools, Some(&TestModel(Mutex::new(vec![action]))), id)
        .await
        .unwrap();
    let pending = runtime::load(&pool, id).await.unwrap().pending.unwrap();
    let request_id = serde_json::from_value(pending["request_id"].clone()).unwrap();
    let _ = super::decide(
        State(tools.state.clone()),
        headers(),
        Path(id),
        Json(super::Decision {
            approved: true,
            request_id,
        }),
    )
    .await
    .unwrap();
    runtime::tick(&tools, None, id).await.unwrap();
    let status: String =
        sqlx::query_scalar("SELECT status FROM workflow_runs WHERE agent_session_id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(status, "FAILED"); // No node attached: real dispatch fails after persistence.
    runtime::fail(&tools, id, "dispatch unavailable")
        .await
        .unwrap();
    runtime::tick(&tools, None, id).await.unwrap();
    assert!(tools.reservation(id).await.unwrap().is_none());
}

#[sqlx::test(migrations = "../../migrations")]
async fn failed_run_evidence_and_foreign_run_protection(pool: PgPool) {
    let (tools, id, asset) = fixture(pool.clone()).await;
    let workflow: Uuid = sqlx::query_scalar(
        "INSERT INTO workflows(name,definition_yaml) VALUES('test','name: test') RETURNING id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let run:Uuid=sqlx::query_scalar("INSERT INTO workflow_runs(workflow_id,asset_id,agent_session_id,status,error) VALUES($1,$2,$3,'FAILED','fixture failure') RETURNING id").bind(workflow).bind(asset).bind(id).fetch_one(&pool).await.unwrap();
    sqlx::query("INSERT INTO logs(run_id,stream,sequence,content) VALUES($1,'stderr',1,$2)")
        .bind(run)
        .bind(b"frame expected map observed odom".as_slice())
        .execute(&pool)
        .await
        .unwrap();
    let model = TestModel(Mutex::new(vec![Action::ReadRunLogs { run_id: run }]));
    runtime::tick(&tools, Some(&model), id).await.unwrap();
    let s = runtime::load(&pool, id).await.unwrap();
    assert!(s
        .context
        .to_string()
        .contains("frame expected map observed odom"));
    let model = TestModel(Mutex::new(vec![
        finish(),
        Action::ListArtifacts { run_id: run },
        Action::InspectRun { run_id: run },
    ]));
    for _ in 0..3 {
        runtime::tick(&tools, Some(&model), id).await.unwrap();
    }
    runtime::tick(&tools, None, id).await.unwrap();
    assert_eq!(runtime::load(&pool, id).await.unwrap().status, "COMPLETED");
    assert!(tools
        .execute(Uuid::new_v4(), &Action::CancelRun { run_id: run }, false)
        .await
        .is_err());
}

#[test]
fn unknown_commands_and_extra_arguments_are_denied() {
    for value in [
        json!({"tool":"shell","run":"rm -rf /"}),
        json!({"tool":"list_assets","command":"echo bypass"}),
        json!({"tool":"start_workflow","asset_id":"../shell"}),
    ] {
        assert!(serde_json::from_value::<Action>(value).is_err());
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn changed_workflow_invalidates_approval_and_timeout_releases(pool: PgPool) {
    let (tools, id, asset) = fixture(pool.clone()).await;
    let action = deployment(&tools, id, asset).await;
    let workflow = match action {
        Action::StartWorkflow { workflow_id, .. } => workflow_id,
        _ => unreachable!(),
    };
    runtime::tick(&tools, Some(&TestModel(Mutex::new(vec![action]))), id)
        .await
        .unwrap();
    let pending = runtime::load(&pool, id).await.unwrap().pending.unwrap();
    let request_id = serde_json::from_value(pending["request_id"].clone()).unwrap();
    let _ = super::decide(
        State(tools.state.clone()),
        headers(),
        Path(id),
        Json(super::Decision {
            approved: true,
            request_id,
        }),
    )
    .await
    .unwrap();
    sqlx::query(
        "UPDATE workflows SET definition_yaml=definition_yaml || E'\\n# changed' WHERE id=$1",
    )
    .bind(workflow)
    .execute(&pool)
    .await
    .unwrap();
    assert!(runtime::tick(&tools, None, id).await.is_err());
    sqlx::query("UPDATE agent_sessions SET deadline=now()-interval '1 second' WHERE id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    runtime::tick(&tools, None, id).await.unwrap();
    assert_eq!(runtime::load(&pool, id).await.unwrap().status, "CANCELLED");
    assert!(tools.reservation(id).await.unwrap().is_none());
}

#[sqlx::test(migrations = "../../migrations")]
async fn restart_recovers_reservation_without_replaying_model(pool: PgPool) {
    let (tools, id, asset) = fixture(pool.clone()).await;
    reserve(&tools, id, asset).await;
    let worker = runtime::start_worker(tools.state.clone(), "test-token".into())
        .await
        .unwrap();
    for _ in 0..20 {
        if runtime::load(&pool, id).await.unwrap().status == "FAILED" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    worker.abort();
    assert_eq!(runtime::load(&pool, id).await.unwrap().status, "FAILED");
    assert!(tools.reservation(id).await.unwrap().is_none());
}

#[sqlx::test(migrations = "../../migrations")]
async fn offline_interruption_releases_without_claiming_physical_cleanup(pool: PgPool) {
    let (tools, id, asset) = fixture(pool.clone()).await;
    reserve(&tools, id, asset).await;
    let workflow: Uuid = sqlx::query_scalar(
        "INSERT INTO workflows(name,definition_yaml) VALUES('test','name: test') RETURNING id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    sqlx::query("UPDATE assets SET last_seen_at=now()-interval '1 minute' WHERE id=$1")
        .bind(asset)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO workflow_runs(workflow_id,asset_id,agent_session_id,status) VALUES($1,$2,$3,'RUNNING')").bind(workflow).bind(asset).bind(id).execute(&pool).await.unwrap();
    runtime::fail(&tools, id, "interrupted").await.unwrap();
    runtime::tick(&tools, None, id).await.unwrap();
    assert!(tools.reservation(id).await.unwrap().is_none());
    let asset = tools
        .api("GET", &format!("/assets/{asset}"), Value::Null)
        .await
        .unwrap();
    assert_eq!(asset["status"], "OFFLINE");
    let error: String =
        sqlx::query_scalar("SELECT error FROM workflow_runs WHERE agent_session_id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(error.contains("unconfirmed"));
}
#[test]
fn deployment_requires_approval_and_reads_are_allowed() {
    assert_eq!(policy(&Action::ListAssets {}), Policy::Allow);
    assert_eq!(
        policy(&Action::StartWorkflow {
            asset_id: Uuid::new_v4(),
            workflow_id: Uuid::new_v4(),
            bundle_id: Some(Uuid::new_v4()),
            reason: "test".into()
        }),
        Policy::Approval
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn operator_can_pause_resume_steer_and_enforce_asset_scope(pool: PgPool) {
    let (tools, id, asset) = fixture(pool.clone()).await;
    let model = TestModel(Mutex::new(vec![Action::ReserveAsset { asset_id: asset }]));

    let _ = super::pause(State(tools.state.clone()), headers(), Path(id))
        .await
        .unwrap();
    runtime::tick(&tools, Some(&model), id).await.unwrap();
    assert!(tools.reservation(id).await.unwrap().is_none());

    let _ = super::instruct(
        State(tools.state.clone()),
        headers(),
        Path(id),
        Json(super::Instruction {
            message: "Use the selected rover only".into(),
        }),
    )
    .await
    .unwrap();
    let _ = super::resume(State(tools.state.clone()), headers(), Path(id))
        .await
        .unwrap();
    runtime::tick(&tools, Some(&model), id).await.unwrap();
    assert!(tools.reservation(id).await.unwrap().is_some());
    let context = runtime::load(&pool, id).await.unwrap().context;
    assert!(context.to_string().contains("Use the selected rover only"));

    tools.cleanup(id).await.unwrap();
    sqlx::query("UPDATE agent_sessions SET constraints=$2 WHERE id=$1")
        .bind(id)
        .bind(json!({"target_asset":"a-different-rover"}))
        .execute(&pool)
        .await
        .unwrap();
    let error = tools
        .execute(id, &Action::ReserveAsset { asset_id: asset }, false)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("restricted this session"));
}
