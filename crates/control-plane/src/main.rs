mod agent;
mod api;
mod grpc;
mod recovery;
mod recovery_model;

use anyhow::Context;
use axum::{
    http::{header, HeaderValue, Method},
    Router,
};
use bench_protocol::bench::node_service_server::NodeServiceServer;
use bench_protocol::bench::{control_message, ControlMessage, Envelope, ReservationChanged};
use clap::Parser;
use grpc::{NodeHub, NodeService};
use sqlx::postgres::PgPoolOptions;
use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};
use tokio::net::TcpListener;
use tonic::transport::Server;
use tower_http::{cors::CorsLayer, trace::TraceLayer};
use tracing::{info, warn};

#[derive(Parser, Debug)]
struct Config {
    #[arg(long, env = "BENCH_ENV", default_value = "development")]
    environment: String,
    #[arg(
        long,
        env = "DATABASE_URL",
        default_value = "postgres://bench:bench@localhost:5432/bench"
    )]
    database_url: String,
    #[arg(long, env = "BENCH_HTTP_ADDR", default_value = "127.0.0.1:8080")]
    http_addr: SocketAddr,
    #[arg(long, env = "BENCH_GRPC_ADDR", default_value = "127.0.0.1:50051")]
    grpc_addr: SocketAddr,
    #[arg(long, env = "BENCH_OFFLINE_AFTER_SECONDS", default_value_t = 15)]
    offline_after_seconds: u64,
    #[arg(long, env = "BENCH_BUNDLE_DIR", default_value = ".bench-data/bundles")]
    bundle_dir: PathBuf,
    #[arg(
        long,
        env = "BENCH_BUNDLE_BASE_URL",
        default_value = "http://localhost:8080"
    )]
    bundle_base_url: String,
    #[arg(
        long,
        env = "BENCH_ARTIFACT_DIR",
        default_value = ".bench-data/artifacts"
    )]
    artifact_dir: PathBuf,
    #[arg(long, env = "BENCH_NODE_TOKEN", default_value = "dev-node-token")]
    node_token: String,
    #[arg(long, env = "BENCH_ADMIN_TOKEN", default_value = "dev-admin-token")]
    admin_token: String,
    #[arg(
        long,
        env = "BENCH_CORS_ORIGINS",
        default_value = "http://localhost:5173,http://localhost:5174"
    )]
    cors_origins: String,
    #[arg(long, default_value_t = false)]
    migrate_only: bool,
}

impl Config {
    fn validate(&self) -> anyhow::Result<()> {
        if !matches!(
            self.environment.as_str(),
            "development" | "test" | "production"
        ) {
            anyhow::bail!("BENCH_ENV must be development, test, or production");
        }
        if self.environment == "production" {
            if self.admin_token == "dev-admin-token" || self.node_token == "dev-node-token" {
                anyhow::bail!("production refuses development admin/node credentials");
            }
            if self.cors_origins.trim().is_empty() || self.cors_origins.contains("localhost") {
                anyhow::bail!("production requires explicit non-local BENCH_CORS_ORIGINS");
            }
        }
        if self.environment == "production"
            && (self.admin_token.len() < 24 || self.node_token.len() < 24)
        {
            anyhow::bail!("admin and node credentials must be at least 24 characters");
        }
        Ok(())
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env().add_directive("info".parse()?),
        )
        .init();
    let config = Config::parse();
    config.validate()?;
    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect(&config.database_url)
        .await
        .context("connect to PostgreSQL")?;
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .context("run migrations")?;
    if config.migrate_only {
        info!("migrations complete");
        return Ok(());
    }
    tokio::fs::create_dir_all(&config.bundle_dir)
        .await
        .context("create bundle storage directory")?;
    tokio::fs::create_dir_all(&config.artifact_dir)
        .await
        .context("create artifact storage directory")?;
    let hub = Arc::new(NodeHub::default());
    let recovery_policy = Arc::new(recovery::RecoveryPolicy::from_env());
    recovery::start_reconciler(pool.clone()).await?;
    let state = api::AppState {
        pool: pool.clone(),
        hub: hub.clone(),
        bundle_dir: Arc::new(config.bundle_dir.clone()),
        bundle_base_url: config.bundle_base_url.trim_end_matches('/').to_string(),
        artifact_dir: Arc::new(config.artifact_dir.clone()),
        admin_token_hash: bench_common::hash_token(&config.admin_token),
        recovery_policy: recovery_policy.clone(),
    };

    let cors = config
        .cors_origins
        .split(',')
        .filter_map(|origin| origin.trim().parse::<HeaderValue>().ok())
        .fold(CorsLayer::new(), |layer, origin| layer.allow_origin(origin))
        .allow_methods([Method::GET, Method::POST, Method::PUT, Method::DELETE])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE]);
    let app = Router::new()
        .merge(api::router())
        .merge(agent::router())
        .merge(recovery::router())
        .with_state(state.clone())
        .layer(cors)
        .layer(TraceLayer::new_for_http());
    let http = TcpListener::bind(config.http_addr).await?;
    let _agent_worker = agent::start_worker(state, config.admin_token.clone()).await?;
    let grpc_service = NodeService {
        pool: pool.clone(),
        hub: hub.clone(),
        artifact_dir: Arc::new(config.artifact_dir),
        node_token_hash: bench_common::hash_token(&config.node_token),
        recovery_policy,
        recovery_lock: Arc::new(tokio::sync::Mutex::new(())),
    };
    let grpc_addr = config.grpc_addr;
    let offline_pool = pool.clone();
    let expiry_hub = hub.clone();
    let threshold = config.offline_after_seconds;
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        loop {
            interval.tick().await;
            if let Err(error) = sqlx::query("UPDATE assets SET status='OFFLINE', updated_at=now() WHERE last_seen_at < now() - make_interval(secs => $1) AND status <> 'OFFLINE'")
                .bind(threshold as i32).execute(&offline_pool).await { warn!(%error, "offline sweep failed"); }
            match sqlx::query_scalar::<_, String>("WITH expired AS (UPDATE reservations SET status='EXPIRED' WHERE status='ACTIVE' AND expires_at<=now() RETURNING asset_id), updated AS (UPDATE assets SET status='AVAILABLE',updated_at=now() WHERE id IN (SELECT asset_id FROM expired) AND status='RESERVED' RETURNING name) SELECT name FROM updated")
                .fetch_all(&offline_pool).await {
                Ok(assets) => for asset in assets {
                    let message = ControlMessage { envelope: Some(Envelope { node_id: String::new(), asset_id: asset.clone(), workflow_run_id: String::new(), timestamp_ms: chrono::Utc::now().timestamp_millis() }), payload: Some(control_message::Payload::ReservationChanged(ReservationChanged { reserved: false, reservation_id: String::new() })) };
                    let _ = expiry_hub.send(&asset, message).await;
                },
                Err(error) => warn!(%error, "reservation expiry sweep failed"),
            }
        }
    });

    info!(%config.http_addr, %grpc_addr, "control plane started");
    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
        info!("shutting down; allowing agent cleanup while node connections remain live");
        if let Err(error)=sqlx::query("UPDATE agent_sessions SET cancel_requested=true WHERE status IN ('CREATED','RUNNING','WAITING_FOR_APPROVAL')").execute(&pool).await {warn!(%error,"agent shutdown request failed");}
        for _ in 0..30 {
            let active=sqlx::query_scalar::<_,i64>("SELECT count(*) FROM agent_sessions WHERE status IN ('CREATED','RUNNING','WAITING_FOR_APPROVAL','CLEANING_UP')").fetch_one(&pool).await;
            if matches!(active, Ok(0)) {
                break;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    };
    tokio::select! {
        result = axum::serve(http, app) => result?,
        result = Server::builder().add_service(NodeServiceServer::new(grpc_service)).serve(grpc_addr) => result?,
        _ = shutdown => {},
    }
    Ok(())
}
