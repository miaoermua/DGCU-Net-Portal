use clap::{Parser, Subcommand};
use portal_core::{OnlineSession, PortalClient};
use std::io::{self, Write};
use zeroize::Zeroize;

#[derive(Parser, Debug)]
#[command(name = "dgcu-portal", version, about = "DGCU LFRadius 校园网认证 CLI")]
struct Cli {
    #[arg(
        long,
        env = "PORTAL_BASE_URL",
        default_value = "http://127.0.0.1/lfradius/"
    )]
    server: String,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    Login {
        #[arg(long)]
        username: Option<String>,
        #[arg(long)]
        password: Option<String>,
        /// 登录后等待一次下线；完成后清零账号和密码，不写入任何配置
        #[arg(long)]
        one_session: bool,
    },
    Sessions,
    Disconnect {
        session_id: String,
    },
    DialStatus,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let client = PortalClient::new(&cli.server)?;
    match cli.command {
        Command::Login {
            username,
            password,
            one_session,
        } => {
            let mut username = username.unwrap_or_else(|| prompt("用户名: "));
            let mut password = password.unwrap_or_else(|| prompt("密码: "));
            let before = if one_session {
                client.sessions().await.unwrap_or_default()
            } else {
                Vec::new()
            };
            let status = client.login(&username, &password).await?;
            println!("登录成功，服务端等待时间: {} 秒", status.wait_time);
            if one_session {
                let after = client.sessions().await?;
                let session = after
                    .iter()
                    .find(|s| {
                        s.username == username && !before.iter().any(|b| b.radacctid == s.radacctid)
                    })
                    .cloned()
                    .or_else(|| {
                        if after.iter().filter(|s| s.username == username).count() == 1 {
                            after.iter().find(|s| s.username == username).cloned()
                        } else {
                            None
                        }
                    })
                    .ok_or("登录成功但无法安全确定本次新会话；请先处理已有会话")?;
                println!("仅一次会话模式：按回车下线并清除本次账号信息");
                let mut input = String::new();
                io::stdin().read_line(&mut input)?;
                client.disconnect_and_confirm(&session).await?;
                input.zeroize();
                username.zeroize();
                password.zeroize();
                println!("会话已下线，本次账号和密码已从客户端内存清除");
            }
        }
        Command::Sessions => print_sessions(&client.sessions().await?),
        Command::Disconnect { session_id } => {
            let session = client
                .sessions()
                .await?
                .into_iter()
                .find(|s| s.radacctid == session_id)
                .ok_or_else(|| format!("未找到会话 {session_id}"))?;
            client.disconnect_and_confirm(&session).await?;
            println!("已请求踢下线并确认会话消失: {}", session_id);
        }
        Command::DialStatus => println!("代拨状态：尚未采集真实代拨流程（DIAL_FLOW_NOT_CAPTURED）"),
    }
    Ok(())
}

fn prompt(label: &str) -> String {
    print!("{label}");
    let _ = io::stdout().flush();
    let mut s = String::new();
    io::stdin().read_line(&mut s).expect("stdin");
    s.trim().to_owned()
}
fn print_sessions(sessions: &[OnlineSession]) {
    if sessions.is_empty() {
        println!("没有在线会话");
        return;
    }
    for s in sessions {
        println!(
            "{} user={} ip={} duration={}s in={} out={}",
            s.radacctid,
            s.username,
            s.framedipaddress
                .map(|v| v.to_string())
                .unwrap_or_else(|| "-".into()),
            s.acctsessiontime,
            s.acctinputoctets,
            s.acctoutputoctets
        );
    }
}
