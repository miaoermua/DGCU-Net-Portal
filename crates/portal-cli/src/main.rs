use clap::{Parser, Subcommand};
use portal_core::{controller::Controller, redact, settings::Settings, Credential, PortalClient};
use std::io::{self, Write};
use tokio::io::{AsyncBufReadExt, BufReader};
use zeroize::Zeroizing;

#[derive(Parser)]
#[command(name = "dgcu-portal", version, about = "DGCU Portal / 自助后台客户端")]
struct Cli {
    #[arg(long,default_value=portal_core::DEFAULT_SERVER)]
    server: String,
    /// 密码始终通过隐藏输入，禁止密码出现在进程参数中
    #[arg(long, global = true)]
    username: Option<String>,
    /// 使用进程代理配置（默认直连；不改变 TUN/VPN 路由）
    #[arg(long)]
    use_proxy: bool,
    /// 指定用于 Portal 的本机网卡名称；省略时自动选择活动网卡
    #[arg(long, global = true)]
    interface: Option<String>,
    /// 输出脱敏的共享认证核心日志到 stderr（默认关闭，不写入文件）
    #[arg(long, global = true)]
    log: bool,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// CMCC Portal 上线；省略 URL 时进行 HTTP Portal 探测
    Connect {
        #[arg(long)]
        portal_url: Option<String>,
        /// 等待回车/Ctrl-C 下线，并释放内存凭据与 Cookie
        #[arg(long)]
        one_session: bool,
        /// 必须指定本次目标，或者从后台唯一新增的会话自动绑定
        #[arg(long)]
        session_id: Option<String>,
    },
    /// 只登录自助后台，不等于校园网上线
    Login,
    /// 每次运行先登录后台，再列出在线会话
    Sessions,
    /// 登录后台，按 ID 下线并确认，不自动选择其他设备
    Disconnect { session_id: String },
    /// 仅 HTTP 探测，获取 Portal URL，不读取网卡
    Discover,
    /// 列出本机网卡及 IPv4/MAC（不执行认证）
    Interfaces,
}
fn credentials(username: Option<String>) -> Result<Credential, Box<dyn std::error::Error>> {
    let username = match username {
        Some(v) => v,
        None => {
            print!("账号: ");
            io::stdout().flush()?;
            let mut value = String::new();
            io::stdin().read_line(&mut value)?;
            value.truncate(value.trim_end().len());
            value
        }
    };
    Ok(Credential::new(
        username,
        rpassword::prompt_password("密码（隐藏输入）: ")?,
    ))
}
#[tokio::main]
async fn main() -> std::process::ExitCode {
    let logs = portal_core::logging::LogBuffer::default();
    match run(logs.clone()).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            logs.record(portal_core::logging::Event::ResponseError);
            eprintln!("{e}");
            std::process::ExitCode::FAILURE
        }
    }
}
async fn run(logs: portal_core::logging::LogBuffer) -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    logs.stderr(cli.log);
    logs.set_enabled(cli.log);
    let settings = Settings {
        server: cli.server,
        interface_name: cli.interface.unwrap_or_default(),
        bypass_proxy: !cli.use_proxy,
        log_enabled: cli.log,
        ..Default::default()
    };
    if matches!(cli.command, Command::Discover) {
        let client = PortalClient::with_options(&settings.server, settings.bypass_proxy)?
            .with_logs(logs.clone());
        println!("{}", client.discover(&settings.probe_url).await?.portal_url);
        return Ok(());
    }
    if matches!(cli.command, Command::Interfaces) {
        for interface in portal_core::network::list()? {
            println!(
                "{}\tIPv4={}\tMAC={}{}",
                interface.name,
                interface.ipv4.unwrap_or_else(|| "-".into()),
                interface.mac.unwrap_or_else(|| "-".into()),
                if interface.internal { "\tinternal" } else { "" }
            );
        }
        return Ok(());
    }
    let credential = credentials(cli.username)?;
    match cli.command {
        Command::Connect {
            portal_url,
            one_session,
            session_id,
        } => {
            let mut controller = Controller::with_logs(settings, logs.clone());
            let url = Zeroizing::new(portal_url.unwrap_or_default());
            let snapshot = controller
                .connect(credential, &url, false, |phase| println!("状态: {phase:?}"))
                .await?;
            println!("{}", snapshot.message);
            if one_session {
                let mut selected = session_id.or(snapshot.selected_id);
                if selected.is_none() {
                    let snapshot = controller.refresh().await?;
                    for row in &snapshot.sessions {
                        println!(
                            "ID={} user={} IP={}",
                            row.radacctid,
                            redact(&row.username),
                            row.framedipaddress
                                .map(|v| v.to_string())
                                .unwrap_or_default()
                        );
                    }
                    print!("输入要结束的本次会话 ID（留空取消，账号会立即清除）: ");
                    io::stdout().flush()?;
                    let mut line = String::new();
                    BufReader::new(tokio::io::stdin())
                        .read_line(&mut line)
                        .await?;
                    if !line.trim().is_empty() {
                        selected = Some(line.trim().to_owned());
                    }
                }
                let id = selected.ok_or(portal_core::AppError::AmbiguousSession)?;
                controller.select(&id)?;
                println!("按回车或 Ctrl-C 下线此会话。仅销毁本地凭据不等于服务器下线。");
                let mut input = Zeroizing::new(String::new());
                let mut stdin = BufReader::new(tokio::io::stdin());
                tokio::select! {r=stdin.read_line(&mut input)=>{r?;},r=tokio::signal::ctrl_c()=>{r?;}}
                let result = controller.disconnect(&id).await;
                controller.forget();
                println!("本地凭据与 Cookie 已释放。");
                result?;
                println!("远端会话已确认下线。");
            }
        }
        command => {
            let client = PortalClient::with_options(&settings.server, settings.bypass_proxy)?
                .with_logs(logs.clone());
            client
                .login(&credential.username, &credential.password)
                .await?;
            drop(credential);
            match command {
                Command::Login => println!("后台登录验证成功；未执行 Portal 上线。"),
                Command::Sessions => {
                    for row in client.sessions().await? {
                        println!(
                            "ID={} user={} IP={} 时长={}秒 上传={}B 下载={}B",
                            row.radacctid,
                            redact(&row.username),
                            row.framedipaddress
                                .map(|v| v.to_string())
                                .unwrap_or_default(),
                            row.acctsessiontime,
                            row.acctinputoctets,
                            row.acctoutputoctets
                        );
                    }
                }
                Command::Disconnect { session_id } => {
                    client.disconnect_id(&session_id).await?;
                    println!("目标会话已确认下线。");
                }
                _ => {}
            }
        }
    }
    Ok(())
}
