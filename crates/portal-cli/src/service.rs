//! Current-user background startup only. Never installs a root/system-wide service.
use std::{path::Path, process::Command};

// fs / PathBuf 只被 macOS 的 LaunchAgent 与 Linux 的 systemd 分支使用，
// Windows（schtasks）分支不需要，无条件导入会在 Windows 上产生 unused 警告。
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::{fs, path::PathBuf};

// Windows 分支用 schtasks() 拿工具原话做诊断，这条通用文案只在别处用得上。
#[cfg(not(target_os = "windows"))]
fn run(command: &mut Command) -> Result<(), String> {
    let result = command.output().map_err(|_| "无法运行服务管理工具")?;
    if result.status.success() {
        Ok(())
    } else {
        Err("系统拒绝服务操作；请确认已打包安装并具有用户服务权限".into())
    }
}
pub fn enable(executable: &Path) -> Result<(), String> {
    if Path::new("/etc/openwrt_release").is_file() {
        return Err(
            "OpenWrt 使用 /etc/init.d/portal-cli 管理 procd，不调用 portal-cli service".into(),
        );
    }
    if !executable.is_file() {
        return Err("程序路径无效".into());
    }
    let path = executable.to_str().ok_or("程序路径编码不受支持")?;
    if path.contains(['\n', '\r', '"']) {
        return Err("程序路径包含不支持的字符".into());
    }
    enable_platform(path)
}
pub fn start() -> Result<(), String> {
    start_platform()
}
pub fn stop() -> Result<(), String> {
    stop_platform()
}
#[cfg(target_os = "macos")]
fn plist_path() -> Result<PathBuf, String> {
    Ok(directories::BaseDirs::new()
        .ok_or("找不到用户目录")?
        .home_dir()
        .join("Library/LaunchAgents/net.dgcu.portal.plist"))
}
#[cfg(target_os = "macos")]
fn xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
#[cfg(target_os = "macos")]
fn enable_platform(executable: &str) -> Result<(), String> {
    let path = plist_path()?;
    fs::create_dir_all(path.parent().unwrap()).map_err(|_| "无法创建 LaunchAgents")?;
    let body=format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?><!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\"><plist version=\"1.0\"><dict><key>Label</key><string>net.dgcu.portal</string><key>ProgramArguments</key><array><string>{}</string><string>run</string><string>--daemon</string></array><key>RunAtLoad</key><true/></dict></plist>",xml(executable));
    fs::write(&path, body).map_err(|_| "无法写入用户 LaunchAgent")?;
    // -w makes the user logon job available; RunAtLoad will launch the single instance.
    run(Command::new("launchctl").arg("load").arg("-w").arg(path))
}
#[cfg(target_os = "macos")]
pub fn disable() -> Result<(), String> {
    let path = plist_path()?;
    if !path.exists() {
        return Ok(());
    }
    // RunAtLoad only (not KeepAlive): removing the file disables future login starts
    // without terminating this application's in-flight logout request.
    fs::remove_file(path).map_err(|_| "无法移除用户 LaunchAgent".into())
}
#[cfg(target_os = "macos")]
fn start_platform() -> Result<(), String> {
    run(Command::new("launchctl")
        .arg("load")
        .arg("-w")
        .arg(plist_path()?))
}
#[cfg(target_os = "macos")]
fn stop_platform() -> Result<(), String> {
    run(Command::new("launchctl").arg("unload").arg(plist_path()?))
}
#[cfg(target_os = "linux")]
fn unit_path() -> Result<PathBuf, String> {
    Ok(directories::BaseDirs::new()
        .ok_or("找不到用户目录")?
        .config_dir()
        .join("systemd/user/dgcu-portal.service"))
}
#[cfg(target_os = "linux")]
fn enable_platform(executable: &str) -> Result<(), String> {
    let path = unit_path()?;
    fs::create_dir_all(path.parent().unwrap()).map_err(|_| "无法创建用户服务目录")?;
    let escaped = executable.replace('\\', "\\\\").replace('%', "%%");
    fs::write(&path,format!("[Unit]\nDescription=DGCU Portal authentication daemon\nAfter=graphical-session.target\n[Service]\nExecStart=\"{escaped}\" run --daemon\n[Install]\nWantedBy=graphical-session.target\n")).map_err(|_|"无法写入用户服务")?;
    run(Command::new("systemctl").args(["--user", "daemon-reload"]))?;
    run(Command::new("systemctl").args(["--user", "enable", "dgcu-portal.service"]))
}
#[cfg(target_os = "linux")]
pub fn disable() -> Result<(), String> {
    let path = unit_path()?;
    if !path.exists() {
        return Ok(());
    }
    run(Command::new("systemctl").args(["--user", "disable", "dgcu-portal.service"]))?;
    fs::remove_file(path).map_err(|_| "无法删除服务文件")?;
    run(Command::new("systemctl").args(["--user", "daemon-reload"]))
}
#[cfg(target_os = "linux")]
fn start_platform() -> Result<(), String> {
    if Path::new("/etc/openwrt_release").is_file() {
        return Err("OpenWrt 使用 /etc/init.d/portal-cli start".into());
    }
    run(Command::new("systemctl").args(["--user", "start", "dgcu-portal.service"]))
}
#[cfg(target_os = "linux")]
fn stop_platform() -> Result<(), String> {
    if Path::new("/etc/openwrt_release").is_file() {
        return Err("OpenWrt 使用 /etc/init.d/portal-cli stop".into());
    }
    run(Command::new("systemctl").args(["--user", "stop", "dgcu-portal.service"]))
}
/// ONLOGON 触发器写在系统任务库里，普通权限调用 schtasks 一定被拒，
/// 因此只有创建任务这一步需要 UAC 提权，start/stop/delete 不弹窗。
#[cfg(target_os = "windows")]
fn create_args(executable: &str) -> Vec<String> {
    vec![
        "/Create".into(),
        "/F".into(),
        "/SC".into(),
        "ONLOGON".into(),
        "/TN".into(),
        TASK.into(),
        "/TR".into(),
        format!("\"{executable}\" run --daemon"),
        "/RL".into(),
        "LIMITED".into(),
    ]
}
/// 系统任务库里的任务名，创建、删除、启动、停止都用它。
#[cfg(target_os = "windows")]
const TASK: &str = "DGCU-Portal";
/// 用户在 UAC 弹窗点“否”时 Start-Process 抛异常，用 ERROR_CANCELLED 原样回传。
#[cfg(target_os = "windows")]
const USER_CANCELLED: i32 = 1223;
/// 包进 PowerShell 单引号字符串，撇号按 PowerShell 规则写两遍。
#[cfg(target_os = "windows")]
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}
/// Start-Process 只是把 -ArgumentList 的元素用空格拼成一条命令行，不会补引号，
/// 于是 /TR 里的引号会被 schtasks 自己的参数解析吃掉，
/// `"C:\...\portal-cli.exe" run --daemon` 会裂成三个参数，提权后照样创建失败。
/// 这里照 Windows 命令行规则先转义，让整串原样落到 schtasks 手里。
#[cfg(target_os = "windows")]
fn escape(value: &str) -> String {
    if !value.is_empty() && !value.contains([' ', '\t', '"']) {
        return value.to_string();
    }
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('"');
    let mut backslashes = 0;
    for ch in value.chars() {
        match ch {
            '\\' => {
                backslashes += 1;
                escaped.push('\\');
            }
            // 引号前的反斜杠要翻倍再多一根，多出来的那根用来转义引号本身。
            '"' => {
                escaped.push_str(&"\\".repeat(backslashes + 1));
                escaped.push('"');
                backslashes = 0;
            }
            _ => {
                backslashes = 0;
                escaped.push(ch);
            }
        }
    }
    // 收尾反斜杠要翻倍，否则会把右引号一起转义掉。
    escaped.push_str(&"\\".repeat(backslashes));
    escaped.push('"');
    escaped
}
/// 跑一次 schtasks，失败时把它的原话带出来，供上层判断与展示。
#[cfg(target_os = "windows")]
fn schtasks(args: &[String]) -> Result<(), String> {
    let output = Command::new("schtasks")
        .args(args)
        .output()
        .map_err(|_| "无法运行 schtasks".to_string())?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(if stderr.is_empty() {
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    } else {
        stderr
    })
}
/// ONLOGON 触发器写在系统任务库里，普通权限改不动；提权创建的任务所有者是
/// Administrators，而普通令牌里该组只用于拒绝，所以删除、启动同样要提权。
/// 先按当前权限试一次，被拒再弹 UAC，这样以管理员跑 GUI 时不会白弹窗。
#[cfg(target_os = "windows")]
fn manage(action: &str, args: &[String]) -> Result<(), String> {
    let reason = match schtasks(args) {
        Ok(()) => return Ok(()),
        Err(reason) => reason,
    };
    let list = args
        .iter()
        .map(|value| quote(&escape(value)))
        .collect::<Vec<_>>()
        .join(",");
    let script = format!(
        "$ErrorActionPreference='Stop';try{{$p=Start-Process -FilePath 'schtasks.exe' -ArgumentList @({list}) -Verb RunAs -Wait -PassThru -WindowStyle Hidden;exit $p.ExitCode}}catch{{exit {USER_CANCELLED}}}"
    );
    let mut elevate = Command::new("powershell");
    elevate.args(["-NoProfile", "-Command", &script]);
    {
        use std::os::windows::process::CommandExt;
        // 隐藏 powershell 自己的控制台，提权确认框由系统弹出。
        elevate.creation_flags(0x0800_0000);
    }
    let status = elevate.status().map_err(|_| "无法发起管理员提权")?;
    match status.code() {
        Some(0) => Ok(()),
        Some(USER_CANCELLED) => Err(format!(
            "未获得管理员授权，{action}未执行（请在 UAC 弹窗中选择“是”）"
        )),
        // 附带未提权那次的原话，否则提权后仍失败就只剩一句“失败”。
        _ if reason.is_empty() => Err(format!("{action}失败，请在 UAC 弹窗中选择“是”后重试")),
        _ => Err(format!("{action}失败；未提权时 schtasks 报错：{reason}")),
    }
}
#[cfg(target_os = "windows")]
fn enable_platform(executable: &str) -> Result<(), String> {
    manage("创建登录启动任务", &create_args(executable))
}
#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::*;

    #[test]
    fn task_command_stays_a_single_argument() {
        let args = create_args(r"C:\Program Files\DGCU-Net-Portal\portal-cli.exe");
        let escaped = args.iter().map(|value| escape(value)).collect::<Vec<_>>();
        // 未转义时这串引号会被 schtasks 的解析器吃掉，`run --daemon` 变成两个多余参数。
        assert!(!args[7].contains("\\\""));
        assert_eq!(
            escaped[7],
            r#""\"C:\Program Files\DGCU-Net-Portal\portal-cli.exe\" run --daemon""#
        );
        assert_eq!(
            escaped
                .iter()
                .map(|value| quote(value))
                .collect::<Vec<_>>()
                .join(","),
            format!(
                "'/Create','/F','/SC','ONLOGON','/TN','DGCU-Portal','/TR',{},'/RL','LIMITED'",
                quote(&escaped[7])
            )
        );
    }
    #[test]
    fn plain_arguments_are_left_alone() {
        assert_eq!(escape("/Create"), "/Create");
        assert_eq!(escape("DGCU-Portal"), "DGCU-Portal");
    }
}
#[cfg(target_os = "windows")]
pub fn disable() -> Result<(), String> {
    // 任务不存在就当已经关掉：关开关时不该因为“没有这个任务”而报错。
    let query = vec!["/Query".into(), "/TN".into(), TASK.into()];
    if schtasks(&query).is_err() {
        return Ok(());
    }
    manage(
        "删除登录启动任务",
        &["/Delete".into(), "/F".into(), "/TN".into(), TASK.into()],
    )
}
#[cfg(target_os = "windows")]
fn start_platform() -> Result<(), String> {
    manage(
        "启动登录启动任务",
        &["/Run".into(), "/TN".into(), TASK.into()],
    )
}
#[cfg(target_os = "windows")]
fn stop_platform() -> Result<(), String> {
    // /End 只结束任务实例，失败也不影响什么；退出 GUI 会走这里，别弹 UAC。
    schtasks(&["/End".into(), "/TN".into(), TASK.into()])
}
