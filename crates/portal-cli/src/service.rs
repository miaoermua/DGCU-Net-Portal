//! Current-user background startup only. Never installs a root/system-wide service.
use std::{path::Path, process::Command};

// fs / PathBuf 只被 macOS 的 LaunchAgent 与 Linux 的 systemd 分支使用，
// Windows（schtasks）分支不需要，无条件导入会在 Windows 上产生 unused 警告。
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::{fs, path::PathBuf};

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
        "DGCU-Portal".into(),
        "/TR".into(),
        format!("\"{executable}\" run --daemon"),
        "/RL".into(),
        "LIMITED".into(),
    ]
}
/// 用户在 UAC 弹窗点“否”时 Start-Process 抛异常，用 ERROR_CANCELLED 原样回传。
#[cfg(target_os = "windows")]
const USER_CANCELLED: i32 = 1223;
/// 包进 PowerShell 单引号字符串，撇号按 PowerShell 规则写两遍。
#[cfg(target_os = "windows")]
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}
#[cfg(target_os = "windows")]
fn enable_platform(executable: &str) -> Result<(), String> {
    let args = create_args(executable);
    // 已经以管理员身份运行时直接创建成功，不额外弹窗。
    let mut direct = Command::new("schtasks");
    direct.args(&args);
    if run(&mut direct).is_ok() {
        return Ok(());
    }
    let list = args
        .iter()
        .map(|value| quote(value))
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
        Some(USER_CANCELLED) => {
            Err("未获得管理员授权，登录启动任务未创建（请在 UAC 弹窗中选择“是”）".into())
        }
        _ => Err("创建登录启动任务失败，请在 UAC 弹窗中选择“是”后重试".into()),
    }
}
#[cfg(target_os = "windows")]
pub fn disable() -> Result<(), String> {
    run(Command::new("schtasks").args(["/Delete", "/F", "/TN", "DGCU-Portal"]))
}
#[cfg(target_os = "windows")]
fn start_platform() -> Result<(), String> {
    run(Command::new("schtasks").args(["/Run", "/TN", "DGCU-Portal"]))
}
#[cfg(target_os = "windows")]
fn stop_platform() -> Result<(), String> {
    run(Command::new("schtasks").args(["/End", "/TN", "DGCU-Portal"]))
}
