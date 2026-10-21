use anyhow::{bail, Context};
use clap::{Parser, Subcommand};
use reqwest::{Client, Method};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{path::PathBuf, time::Duration};

#[derive(Parser)]
#[command(
    name = "bench",
    version,
    about = "Reserve and test physical assets from anywhere"
)]
struct Cli {
    #[arg(
        long,
        env = "BENCH_API_URL",
        default_value = "http://localhost:8080/api"
    )]
    api_url: String,
    #[arg(long, env = "BENCH_ADMIN_TOKEN")]
    token: Option<String>,
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Login {
        #[arg(long)]
        token: String,
        #[arg(long)]
        api_url: Option<String>,
    },
    Assets {
        #[command(subcommand)]
        command: Option<AssetCommand>,
    },
    Reserve {
        asset: String,
        #[arg(long, default_value = "cli")]
        owner: String,
        #[arg(long, default_value_t = 60)]
        minutes: i64,
    },
    Release {
        reservation: String,
    },
    Workflows {
        #[command(subcommand)]
        command: WorkflowCommand,
    },
    Run {
        workflow: PathBuf,
        #[arg(long)]
        asset: String,
    },
    Runs {
        #[command(subcommand)]
        command: Option<RunCommand>,
    },
    Logs {
        run_id: String,
        #[arg(long)]
        follow: bool,
    },
    Cancel {
        run_id: String,
    },
}
#[derive(Subcommand)]
enum AssetCommand {
    Describe { asset: String },
}
#[derive(Subcommand)]
enum WorkflowCommand {
    List,
    Create { file: PathBuf },
}
#[derive(Subcommand)]
enum RunCommand {
    Show { run_id: String },
}
#[derive(Serialize, Deserialize)]
struct StoredConfig {
    api_url: String,
    token: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    if let Command::Login { token, api_url } = cli.command {
        let config = StoredConfig {
            api_url: api_url.unwrap_or(cli.api_url),
            token,
        };
        save_config(&config).await?;
        println!("Saved Bench credentials.");
        return Ok(());
    }
    let stored = load_config().await.ok();
    let api_url = stored
        .as_ref()
        .map(|c| c.api_url.clone())
        .unwrap_or(cli.api_url);
    let token = cli
        .token
        .or_else(|| stored.map(|c| c.token))
        .unwrap_or_else(|| "dev-admin-token".into());
    let api = Api {
        base: api_url.trim_end_matches('/').into(),
        token,
        client: Client::new(),
    };
    match cli.command {
        Command::Assets { command: None } => print(
            cli.json,
            api.request(Method::GET, "/assets", None).await?,
            Some(&[
                "name",
                "status",
                "hardware_type",
                "architecture",
                "last_seen_at",
            ]),
        ),
        Command::Assets {
            command: Some(AssetCommand::Describe { asset }),
        } => {
            let value = find_asset(&api, &asset).await?;
            print(cli.json, value, None)
        }
        Command::Reserve {
            asset,
            owner,
            minutes,
        } => {
            let asset = find_asset(&api, &asset).await?;
            let id = asset["id"].as_str().context("asset missing id")?;
            print(
                cli.json,
                api.request(
                    Method::POST,
                    &format!("/assets/{id}/reserve"),
                    Some(json!({"owner":owner,"duration_minutes":minutes})),
                )
                .await?,
                None,
            )
        }
        Command::Release { reservation } => print(
            cli.json,
            api.request(
                Method::POST,
                &format!("/reservations/{reservation}/release"),
                Some(json!({})),
            )
            .await?,
            None,
        ),
        Command::Workflows {
            command: WorkflowCommand::List,
        } => print(
            cli.json,
            api.request(Method::GET, "/workflows", None).await?,
            Some(&["id", "name", "version", "updated_at"]),
        ),
        Command::Workflows {
            command: WorkflowCommand::Create { file },
        } => {
            let yaml = tokio::fs::read_to_string(file).await?;
            print(
                cli.json,
                api.request(
                    Method::POST,
                    "/workflows",
                    Some(json!({"definition_yaml":yaml})),
                )
                .await?,
                None,
            )
        }
        Command::Run { workflow, asset } => {
            let yaml = tokio::fs::read_to_string(workflow).await?;
            let workflow = api
                .request(
                    Method::POST,
                    "/workflows",
                    Some(json!({"definition_yaml":yaml})),
                )
                .await?;
            let asset = find_asset(&api, &asset).await?;
            let asset_id = asset["id"].as_str().context("asset missing id")?;
            if asset["status"] == "AVAILABLE" {
                let _ = api
                    .request(
                        Method::POST,
                        &format!("/assets/{asset_id}/reserve"),
                        Some(json!({"owner":"cli","duration_minutes":60})),
                    )
                    .await?;
            }
            let run = api
                .request(
                    Method::POST,
                    "/runs",
                    Some(json!({"asset_id":asset_id,"workflow_id":workflow["id"]})),
                )
                .await?;
            print(cli.json, run, None)
        }
        Command::Runs { command: None } => print(
            cli.json,
            api.request(Method::GET, "/runs", None).await?,
            Some(&["id", "workflow_name", "asset_name", "status", "created_at"]),
        ),
        Command::Runs {
            command: Some(RunCommand::Show { run_id }),
        } => print(
            cli.json,
            api.request(Method::GET, &format!("/runs/{run_id}"), None)
                .await?,
            None,
        ),
        Command::Logs { run_id, follow } => follow_logs(&api, &run_id, follow).await?,
        Command::Cancel { run_id } => print(
            cli.json,
            api.request(
                Method::POST,
                &format!("/runs/{run_id}/cancel"),
                Some(json!({})),
            )
            .await?,
            None,
        ),
        Command::Login { .. } => unreachable!(),
    }
    Ok(())
}

struct Api {
    base: String,
    token: String,
    client: Client,
}
impl Api {
    async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> anyhow::Result<Value> {
        let mut request = self
            .client
            .request(method, format!("{}{}", self.base, path))
            .bearer_auth(&self.token);
        if let Some(body) = body {
            request = request.json(&body)
        }
        let response = request
            .send()
            .await
            .context("connect to Bench control plane")?;
        let status = response.status();
        let value: Value = response.json().await.context("decode Bench response")?;
        if !status.is_success() {
            bail!(
                "Bench API {}: {}",
                status,
                value["error"].as_str().unwrap_or("request failed")
            )
        }
        Ok(value)
    }
}
async fn find_asset(api: &Api, name: &str) -> anyhow::Result<Value> {
    api.request(Method::GET, "/assets", None)
        .await?
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|item| item["name"] == name || item["id"] == name)
        })
        .cloned()
        .with_context(|| format!("asset '{name}' not found"))
}
async fn follow_logs(api: &Api, run: &str, follow: bool) -> anyhow::Result<()> {
    let mut last = 0;
    loop {
        let logs = api
            .request(Method::GET, &format!("/runs/{run}/logs"), None)
            .await?;
        for log in logs.as_array().into_iter().flatten() {
            let id = log["id"].as_i64().unwrap_or(0);
            if id > last {
                last = id;
                eprint!("{}", log["content"].as_str().unwrap_or(""));
            }
        }
        if !follow {
            break;
        }
        let run_value = api
            .request(Method::GET, &format!("/runs/{run}"), None)
            .await?;
        if matches!(
            run_value["status"].as_str(),
            Some("PASSED" | "FAILED" | "CANCELLED" | "TIMED_OUT")
        ) {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    Ok(())
}
fn print(as_json: bool, value: Value, columns: Option<&[&str]>) {
    if as_json || columns.is_none() {
        println!(
            "{}",
            serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".into())
        );
        return;
    }
    for item in value.as_array().into_iter().flatten() {
        println!(
            "{}",
            columns
                .unwrap()
                .iter()
                .map(|key| item[*key]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| item[*key].to_string()))
                .collect::<Vec<_>>()
                .join("\t")
        );
    }
}
fn config_path() -> anyhow::Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".config/bench/config.json"))
}
async fn save_config(config: &StoredConfig) -> anyhow::Result<()> {
    let path = config_path()?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(path, serde_json::to_vec_pretty(config)?).await?;
    Ok(())
}
async fn load_config() -> anyhow::Result<StoredConfig> {
    Ok(serde_json::from_slice(
        &tokio::fs::read(config_path()?).await?,
    )?)
}
