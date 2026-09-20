use clap::{Parser, Subcommand};
use portal_cli::{
    daemon,
    ipc::{self, Request},
    network, service,
    settings::{RefreshPolicy, Settings},
    Credential,
};
use std::io::{self, Write};

#[derive(Parser)]
#[command(name = "portal-cli", version, about = "DGCU Portal 命令行客户端")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Run {
        #[arg(long)]
        daemon: bool,
    },
    Up,
    Down {
        session_id: String,
    },
    Status,
    Sessions,
    Logs {
        #[arg(long)]
        follow: bool,
        #[arg(long)]
        clear: bool,
    },
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    Service {
        #[command(subcommand)]
        command: ServiceCommand,
    },
    Diagnose {
        #[command(subcommand)]
        command: DiagnoseCommand,
    },
}
#[derive(Subcommand)]
enum ConfigCommand {
    Show,
    Get { key: String },
    Set { key: String, value: String },
    Account,
    Reset,
}
#[derive(Subcommand)]
enum ServiceCommand {
    Enable,
    Disable,
    Start,
    Stop,
    Restart,
    Status,
}
#[derive(Subcommand)]
enum DiagnoseCommand {
    Interfaces,
    Backend,
    Portal,
    Probe,
    Routes,
    Config,
    Request,
}

fn credentials() -> Result<Credential, Box<dyn std::error::Error + Send + Sync>> {
    print!("账号: ");
    io::stdout().flush()?;
    let mut username = String::new();
    io::stdin().read_line(&mut username)?;
    username.truncate(username.trim_end().len());
    Ok(Credential::new(
        username,
        rpassword::prompt_password("密码（隐藏输入）: ")?,
    ))
}
fn parse_refresh(value: &str) -> Option<RefreshPolicy> {
    Some(match value {
        "1s" => RefreshPolicy::OneSecond,
        "2s" => RefreshPolicy::TwoSeconds,
        "5s" => RefreshPolicy::FiveSeconds,
        "random" => RefreshPolicy::Random,
        "1m" | "60s" => RefreshPolicy::OneMinute,
        "off" => RefreshPolicy::Disabled,
        _ => return None,
    })
}
fn set_config(mut settings: Settings, key: &str, value: &str) -> Result<(), String> {
    match key {
        "interface" => settings.interface_name = value.into(),
        "refresh" => {
            settings.refresh_policy =
                parse_refresh(value).ok_or("刷新策略应为 1s、2s、5s、random、1m 或 off")?
        }
        "portal-probe" => {
            settings.probe_enabled = match value {
                "on" => true,
                "off" => false,
                _ => return Err("portal-probe 应为 on 或 off".into()),
            }
        }
        "proxy" => {
            settings.bypass_proxy = match value {
                "bypass" => true,
                "system" => false,
                _ => return Err("proxy 应为 bypass 或 system".into()),
            }
        }
        "auto-redial" => {
            settings.auto_redial = match value {
                "on" => true,
                "off" => false,
                _ => return Err("auto-redial 应为 on 或 off".into()),
            }
        }
        "traffic" => {
            settings.traffic_enabled = match value {
                "on" => true,
                "off" => false,
                _ => return Err("traffic 应为 on 或 off".into()),
            }
        }
        _ => return Err(format!("未知配置项：{key}")),
    }
    settings.save()
}
#[tokio::main]
async fn main() -> std::process::ExitCode {
    match run(Cli::parse()).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            std::process::ExitCode::FAILURE
        }
    }
}
async fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    match cli.command {
        Command::Run { daemon: true } => daemon::serve().await?,
        Command::Run { daemon: false } => return Err("请使用 portal-cli run --daemon".into()),
        Command::Up => {
            let credential = credentials()?;
            let response = ipc::request(Request::Up {
                username: credential.username.clone(),
                password: credential.password.clone(),
                portal_url: String::new(),
                backend_only: false,
            })
            .await?;
            if !response.ok {
                return Err(response.message.into());
            }
            println!("{}", response.message);
        }
        Command::Down { session_id } => {
            let response = ipc::request(Request::Down { session_id }).await?;
            if !response.ok {
                return Err(response.message.into());
            }
            println!("{}", response.message);
        }
        Command::Status => {
            let response = ipc::request(Request::Status).await?;
            println!("{}", response.message);
            if let Some(snapshot) = response.snapshot {
                println!(
                    "状态={} 会话数={}",
                    snapshot.status,
                    snapshot.sessions.len()
                );
            }
        }
        Command::Sessions => {
            let response = ipc::request(Request::Sessions).await?;
            if !response.ok {
                return Err(response.message.into());
            }
            if let Some(snapshot) = response.snapshot {
                for row in snapshot.sessions {
                    println!(
                        "ID={} IP={}",
                        row.radacctid,
                        row.framedipaddress
                            .map(|v| v.to_string())
                            .unwrap_or_default()
                    );
                }
            }
        }
        Command::Logs { follow: _, clear } => {
            if clear {
                return Err("日志清理由 GUI 配置完成后执行".into());
            }
            let response = ipc::request(Request::Logs).await?;
            if let Some(logs) = response.logs {
                for entry in logs {
                    println!("{} {} {}", entry.level, entry.code, entry.message);
                }
            }
        }
        Command::Config { command } => match command {
            ConfigCommand::Show | ConfigCommand::Get { .. } => {
                println!("{}", serde_json::to_string_pretty(&Settings::load())?)
            }
            ConfigCommand::Set { key, value } => {
                set_config(Settings::load(), &key, &value)?;
                println!("已保存配置：{key}");
            }
            ConfigCommand::Account => {
                let credential = credentials()?;
                let mut settings = Settings::load();
                settings.username = credential.username.clone();
                settings.credential_store = portal_cli::settings::CredentialStore::System;
                settings.save()?;
                portal_cli::settings::save_password(&credential.password)?;
                println!("账号凭据已保存到系统凭据库");
            }
            ConfigCommand::Reset => return Err("配置重置需要明确指定配置文件路径".into()),
        },
        Command::Service { command } => {
            let executable = std::env::current_exe().map_err(|_| "无法获取 portal-cli 路径")?;
            match command {
                ServiceCommand::Enable => {
                    service::enable(&executable)?;
                    println!("后台服务已启用");
                }
                ServiceCommand::Disable => {
                    service::disable()?;
                    println!("后台服务已禁用");
                }
                ServiceCommand::Start => {
                    if ipc::request(Request::Status).await.is_err() {
                        service::start()?;
                    }
                    println!("daemon 已启动");
                }
                ServiceCommand::Status => {
                    let response = ipc::request(Request::Status).await?;
                    println!("{}", response.message);
                }
                ServiceCommand::Stop => {
                    service::stop()?;
                    println!("后台服务已停止");
                }
                ServiceCommand::Restart => {
                    service::stop().ok();
                    service::start()?;
                    println!("后台服务已重启");
                }
            }
        }
        Command::Diagnose { command } => match command {
            DiagnoseCommand::Interfaces => {
                for interface in network::list()? {
                    println!(
                        "{}\tIPv4={}\tMAC={}",
                        interface.name,
                        interface.ipv4.unwrap_or_else(|| "-".into()),
                        interface.mac.unwrap_or_else(|| "-".into())
                    );
                }
            }
            DiagnoseCommand::Config => {
                println!("{}", serde_json::to_string_pretty(&Settings::load())?)
            }
            _ => return Err("该诊断命令尚未接入 daemon 请求链".into()),
        },
    }
    Ok(())
}
