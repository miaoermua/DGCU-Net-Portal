use clap::{Parser, Subcommand};
use portal_cli::{
    daemon,
    ipc::{self, Request},
    network, service,
    settings::{PollJitter, ReconnectMode, RefreshPolicy, Settings},
    Credential,
};
use std::io::{self, Write};

#[derive(Parser)]
#[command(name = "portal-cli", version, about = "DGCU Net Portal 命令行客户端")]
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
        "1m" | "60s" => RefreshPolicy::OneMinute,
        "off" => RefreshPolicy::Disabled,
        _ => return None,
    })
}
fn set_config(mut settings: Settings, key: &str, value: &str) -> Result<(), String> {
    match key {
        "interface" => settings.interface_name = value.into(),
        "paip" => settings.paip = value.into(),
        "basip" => {
            settings.basip = if value == "auto" {
                String::new()
            } else {
                value.into()
            }
        }
        "refresh" => {
            settings.refresh_policy = parse_refresh(value).ok_or("刷新策略应为 1m 或 off")?
        }
        "jitter" => {
            settings.poll_jitter = match value {
                "low" => PollJitter::Low,
                "medium" => PollJitter::Medium,
                "high" => PollJitter::High,
                "off" => PollJitter::Disabled,
                _ => return Err("jitter 应为 low、medium、high 或 off".into()),
            }
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
            settings.reconnect_mode = match value {
                "off" => ReconnectMode::Disabled,
                "new" => ReconnectMode::NewSession,
                "terminate" => ReconnectMode::TerminateAndReconnect,
                _ => return Err("auto-redial 应为 off、new 或 terminate".into()),
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

/// `Reload` 请求的结果。
///
/// 三种结果对用户的意义完全不同，不能压成一个"失败"：daemon 没在运行时配置已落盘、
/// 下次启动自然生效；daemon 明确拒绝时配置没进运行态，需要看原因；连不上端点则
/// 介于两者之间，只能如实说"未确认"。
///
/// 端点不存在（daemon 没运行）与连接期 IPC 失败在 `interprocess` 的 `connect`
/// 层返回的是同一类错误，无法可靠区分，因此合并成 `Unreachable`——措辞上写成
/// "未运行或无法连接"，不去猜具体是哪一种。
#[derive(Debug, PartialEq, Eq)]
enum ReloadOutcome {
    Reloaded,
    Rejected(String),
    Unreachable(String),
}

async fn reload_daemon() -> ReloadOutcome {
    match ipc::request(Request::Reload).await {
        Ok(response) if response.ok => ReloadOutcome::Reloaded,
        Ok(response) => ReloadOutcome::Rejected(response.message),
        Err(error) => ReloadOutcome::Unreachable(error.to_string()),
    }
}

/// `config set` 用：保存成功后尽力通知 daemon，失败只提示、不改变命令结果。
///
/// 配置已经落盘，运行态没跟上属于"下次启动会追平"的情况，不该让一次成功的写入
/// 变成失败退出；但也不能把"没确认"说成"已加载"。
async fn reload_daemon_if_running() {
    match reload_daemon().await {
        ReloadOutcome::Reloaded => {}
        ReloadOutcome::Rejected(reason) => {
            eprintln!("配置已保存，但后台拒绝重新加载：{reason}")
        }
        ReloadOutcome::Unreachable(reason) => {
            eprintln!("配置已保存；后台未运行或无法连接（{reason}），将在下次启动时生效")
        }
    }
}

/// `config account` 的最终结论。
///
/// 抽成纯函数是为了让"凭据保存失败"与"Reload 失败"的六种组合都能被直接断言，
/// 而不必真的去动系统凭据库或 IPC。
#[derive(Debug, PartialEq, Eq)]
enum AccountOutcome {
    /// 配置与凭据都保存成功，后台也确认已重新加载。
    Saved,
    /// 都已保存，但后台没确认重新加载。命令仍算成功：磁盘状态正确，运行态会在
    /// 下次启动或下次 Reload 时追平。
    SavedWithoutReload(String),
    /// 凭据没保存成功。即使配置与 Reload 都成功，这也是真实失败——磁盘上的账号
    /// 已经改了，但自动登录用的密码还是旧的（或根本没有）。
    PasswordFailed(String),
}

fn account_outcome(password: Result<(), String>, reload: ReloadOutcome) -> AccountOutcome {
    match (password, reload) {
        (Ok(()), ReloadOutcome::Reloaded) => AccountOutcome::Saved,
        (Ok(()), ReloadOutcome::Rejected(reason)) => {
            AccountOutcome::SavedWithoutReload(format!("后台拒绝重新加载：{reason}"))
        }
        (Ok(()), ReloadOutcome::Unreachable(reason)) => AccountOutcome::SavedWithoutReload(format!(
            "后台未运行或无法连接（{reason}），新配置将在下次启动时生效"
        )),
        (Err(error), ReloadOutcome::Reloaded) => {
            AccountOutcome::PasswordFailed(format!("配置已保存并重新加载，但密码保存失败：{error}"))
        }
        (Err(error), ReloadOutcome::Rejected(reason)) => AccountOutcome::PasswordFailed(format!(
            "配置已保存，但密码保存失败：{error}；后台也拒绝重新加载：{reason}"
        )),
        (Err(error), ReloadOutcome::Unreachable(reason)) => AccountOutcome::PasswordFailed(format!(
            "配置已保存，但密码保存失败：{error}；后台未运行或无法连接（{reason}），新配置将在下次启动时生效"
        )),
    }
}

/// 按"配置 → 凭据 → Reload"的顺序执行 `config account`。
///
/// 配置保存失败就直接返回，凭据与 Reload 都不执行（磁盘没变，没有需要同步的东西）；
/// 配置保存成功之后，无论凭据是否保存成功都必须尝试 Reload，否则 daemon 会继续用
/// 旧的运行态配置，而磁盘上已经是新的了。
///
/// 三步用闭包注入，只为了让"配置失败时后两步不执行"这条顺序约束能被测试直接验证。
async fn apply_account<S, P, F, Fut>(
    settings: &Settings,
    save_config: S,
    save_password: P,
    reload: F,
) -> Result<AccountOutcome, String>
where
    S: FnOnce(&Settings) -> Result<(), String>,
    P: FnOnce() -> Result<(), String>,
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = ReloadOutcome>,
{
    save_config(settings)?;
    let password = save_password();
    let reload = reload().await;
    Ok(account_outcome(password, reload))
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
                reload_daemon_if_running().await;
            }
            ConfigCommand::Account => {
                let credential = credentials()?;
                let mut next = Settings::load();
                next.username = credential.username.clone();
                next.credential_store = portal_cli::settings::CredentialStore::System;
                // 配置存盘失败时 `apply_account` 直接返回，凭据与 Reload 都不执行；
                // 存盘成功之后即使密码保存失败，也会尝试让 daemon 看到新配置。
                let outcome = apply_account(
                    &next,
                    Settings::save,
                    || portal_cli::settings::save_password(&credential.password),
                    reload_daemon,
                )
                .await?;
                match outcome {
                    AccountOutcome::Saved => println!("账号凭据已保存到系统凭据库"),
                    AccountOutcome::SavedWithoutReload(note) => {
                        println!("账号凭据已保存到系统凭据库");
                        eprintln!("{note}");
                    }
                    AccountOutcome::PasswordFailed(message) => return Err(message.into()),
                }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    /// 配置保存失败：凭据与 Reload 都不执行，错误原样返回。
    #[tokio::test]
    async fn config_failure_stops_before_password_and_reload() {
        let password_called = Rc::new(Cell::new(false));
        let reload_called = Rc::new(Cell::new(false));
        let (p, r) = (password_called.clone(), reload_called.clone());
        let settings = Settings::default();

        let result = apply_account(
            &settings,
            |_| Err("磁盘只读".to_string()),
            || {
                p.set(true);
                Ok(())
            },
            || {
                r.set(true);
                async { ReloadOutcome::Reloaded }
            },
        )
        .await;

        assert_eq!(result, Err("磁盘只读".to_string()));
        assert!(!password_called.get(), "配置没存成功就不该写凭据");
        assert!(!reload_called.get(), "配置没存成功就不该 Reload");
    }

    /// 三步都成功：结论是 Saved。
    #[tokio::test]
    async fn account_reports_plain_success() {
        let settings = Settings::default();
        let outcome = apply_account(
            &settings,
            |_| Ok(()),
            || Ok(()),
            || async { ReloadOutcome::Reloaded },
        )
        .await
        .expect("配置保存成功");
        assert_eq!(outcome, AccountOutcome::Saved);
    }

    /// 凭据保存失败但 Reload 成功：必须说清"配置已保存并重新加载、密码没存上"，
    /// 而且不能把凭据层的原始错误吞掉。
    #[tokio::test]
    async fn account_reports_password_failure_after_successful_reload() {
        let reloaded = Rc::new(Cell::new(false));
        let r = reloaded.clone();
        let settings = Settings::default();
        let outcome = apply_account(
            &settings,
            |_| Ok(()),
            || Err("无法保存到系统凭据库".to_string()),
            || {
                r.set(true);
                async { ReloadOutcome::Reloaded }
            },
        )
        .await
        .expect("配置保存成功");

        assert!(reloaded.get(), "凭据失败也必须让 daemon 重新加载");
        let AccountOutcome::PasswordFailed(message) = outcome else {
            panic!("必须报告为失败：{outcome:?}");
        };
        assert!(message.contains("配置已保存并重新加载"), "{message}");
        assert!(
            message.contains("无法保存到系统凭据库"),
            "原始错误不能丢：{message}"
        );
    }

    /// daemon 没在运行：配置与凭据都写好了，但不能说成"已重新加载"。
    #[tokio::test]
    async fn account_reports_missing_daemon_without_claiming_reload() {
        let settings = Settings::default();
        let outcome = apply_account(
            &settings,
            |_| Ok(()),
            || Ok(()),
            || async { ReloadOutcome::Unreachable("连接被拒绝".into()) },
        )
        .await
        .expect("配置保存成功");

        let AccountOutcome::SavedWithoutReload(note) = outcome else {
            panic!("不该报成完全成功：{outcome:?}");
        };
        assert!(note.contains("未运行或无法连接"), "{note}");
        assert!(note.contains("下次启动"), "{note}");
        assert!(!note.contains("已重新加载"), "不能谎称已加载：{note}");
    }

    /// daemon 明确拒绝 Reload：原因要保留下来。
    #[tokio::test]
    async fn account_reports_rejected_reload() {
        let settings = Settings::default();
        let outcome = apply_account(
            &settings,
            |_| Ok(()),
            || Ok(()),
            || async { ReloadOutcome::Rejected("配置格式错误".into()) },
        )
        .await
        .expect("配置保存成功");

        let AccountOutcome::SavedWithoutReload(note) = outcome else {
            panic!("不该报成完全成功：{outcome:?}");
        };
        assert!(note.contains("拒绝"), "{note}");
        assert!(note.contains("配置格式错误"), "{note}");
    }

    /// 凭据与 Reload 都失败：两个原因必须分开讲清楚。
    #[tokio::test]
    async fn account_reports_password_and_reload_failures_separately() {
        let settings = Settings::default();
        let outcome = apply_account(
            &settings,
            |_| Ok(()),
            || Err("无法保存到系统凭据库".to_string()),
            || async { ReloadOutcome::Unreachable("连接被拒绝".into()) },
        )
        .await
        .expect("配置保存成功");

        let AccountOutcome::PasswordFailed(message) = outcome else {
            panic!("必须报告为失败：{outcome:?}");
        };
        assert!(message.contains("密码保存失败"), "{message}");
        assert!(message.contains("无法保存到系统凭据库"), "{message}");
        assert!(message.contains("未运行或无法连接"), "{message}");
    }

    /// 凭据失败 + Reload 被拒：两个原因都要出现。
    #[tokio::test]
    async fn account_reports_password_failure_with_rejected_reload() {
        let settings = Settings::default();
        let outcome = apply_account(
            &settings,
            |_| Ok(()),
            || Err("无法保存到系统凭据库".to_string()),
            || async { ReloadOutcome::Rejected("配置格式错误".into()) },
        )
        .await
        .expect("配置保存成功");

        let AccountOutcome::PasswordFailed(message) = outcome else {
            panic!("必须报告为失败：{outcome:?}");
        };
        assert!(message.contains("密码保存失败"), "{message}");
        assert!(message.contains("拒绝重新加载"), "{message}");
        assert!(message.contains("配置格式错误"), "{message}");
    }
}
