use crate::{validate_url, DEFAULT_SERVER};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use zeroize::Zeroizing;

#[derive(Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UiPreferences {
    pub show_sessions: bool,
    pub log_enabled: bool,
    pub theme_mode: ThemeMode,
    pub refresh_policy: RefreshPolicy,
    pub traffic_enabled: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefreshPolicy {
    OneSecond,
    TwoSeconds,
    #[default]
    FiveSeconds,
    Random,
    OneMinute,
    Disabled,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialStore {
    #[default]
    System,
    File,
    Memory,
}
impl RefreshPolicy {
    pub fn next_delay(self) -> Option<Duration> {
        let seconds = match self {
            Self::OneSecond => 1,
            Self::TwoSeconds => 2,
            Self::FiveSeconds => 5,
            Self::Random => {
                let nanos = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .subsec_nanos();
                u64::from(nanos % 10 + 1)
            }
            Self::OneMinute => 60,
            Self::Disabled => return None,
        };
        Some(Duration::from_secs(seconds))
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub server: String,
    pub auth_url: String,
    pub probe_url: String,
    /// Try public captive-check discovery only after the DGCU template fails.
    pub probe_enabled: bool,
    pub refresh_policy: RefreshPolicy,
    pub traffic_enabled: bool,
    /// Name of the interface whose IPv4/MAC are sent to the Portal gateway.
    /// Empty means automatic selection of the first active non-loopback one.
    pub interface_name: String,
    pub bypass_proxy: bool,
    pub credential_store: CredentialStore,
    pub auto_redial: bool,
    pub tray_startup: bool,
    pub service_enabled: bool,
    pub username: String,
    pub show_sessions: bool,
    pub log_enabled: bool,
    pub theme_mode: ThemeMode,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            server: DEFAULT_SERVER.into(),
            auth_url: format!("{DEFAULT_SERVER}web/admin/login"),
            probe_url: "http://captive.apple.com/hotspot-detect.html".into(),
            probe_enabled: true,
            refresh_policy: RefreshPolicy::FiveSeconds,
            traffic_enabled: false,
            interface_name: String::new(),
            bypass_proxy: true,
            credential_store: CredentialStore::System,
            auto_redial: false,
            tray_startup: false,
            service_enabled: false,
            username: String::new(),
            show_sessions: false,
            log_enabled: false,
            theme_mode: ThemeMode::System,
        }
    }
}
pub fn config_path() -> Result<PathBuf, String> {
    ProjectDirs::from("net", "dgcu", "portal")
        .map(|d| d.config_dir().join("settings.json"))
        .ok_or("无法定位用户配置目录".into())
}
impl Settings {
    pub fn ui_preferences(&self) -> UiPreferences {
        UiPreferences {
            show_sessions: self.show_sessions,
            log_enabled: self.log_enabled,
            theme_mode: self.theme_mode,
            refresh_policy: self.refresh_policy,
            traffic_enabled: self.traffic_enabled,
        }
    }
    pub fn set_ui_preferences(&mut self, value: &UiPreferences) {
        self.show_sessions = value.show_sessions;
        self.log_enabled = value.log_enabled;
        self.theme_mode = value.theme_mode;
        self.refresh_policy = value.refresh_policy;
        self.traffic_enabled = value.traffic_enabled;
    }
    pub fn normalize(&mut self) -> Result<(), String> {
        for value in [&self.server, &self.auth_url, &self.probe_url] {
            validate_url(value).map_err(|e| e.to_string())?;
        }
        if self.credential_store == CredentialStore::Memory {
            self.auto_redial = false;
            self.service_enabled = false;
            self.username.clear();
        }
        Ok(())
    }
    pub fn load() -> Self {
        config_path()
            .ok()
            .and_then(|p| fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }
    pub fn save(&self) -> Result<(), String> {
        let mut value = self.clone();
        value.normalize()?;
        let path = config_path()?;
        fs::create_dir_all(path.parent().unwrap()).map_err(|_| "无法创建配置目录")?;
        let temp = path.with_extension("tmp");
        let bytes = serde_json::to_vec_pretty(&value).map_err(|_| "无法序列化设置")?;
        use std::io::Write;
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp).map_err(|_| "无法写入设置")?;
        file.write_all(&bytes).map_err(|_| "无法写入设置")?;
        file.sync_all().map_err(|_| "无法同步设置")?;
        fs::rename(&temp, &path).map_err(|_| "无法替换设置")?;
        Ok(())
    }
}
fn entry() -> Result<keyring::Entry, String> {
    keyring::Entry::new("net.dgcu.portal", "saved-account").map_err(|_| "无法访问系统凭据库".into())
}
fn password_path() -> Result<PathBuf, String> {
    config_path().map(|path| path.with_file_name("credentials"))
}
pub fn save_password(password: &str) -> Result<(), String> {
    entry()?
        .set_password(password)
        .map_err(|_| "无法保存到系统凭据库".into())
}
pub fn password() -> Result<Zeroizing<String>, String> {
    entry()?
        .get_password()
        .map(Zeroizing::new)
        .map_err(|_| "没有保存的密码或凭据库不可用".into())
}
pub fn save_file_password(password: &str) -> Result<(), String> {
    let path = password_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|_| "无法创建凭据目录")?;
    }
    use std::io::Write;
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|_| "无法写入明文凭据文件")?;
    file.write_all(password.as_bytes())
        .map_err(|_| "无法写入明文凭据文件")?;
    file.sync_all().map_err(|_| "无法同步明文凭据文件".into())
}
pub fn file_password() -> Result<Zeroizing<String>, String> {
    let bytes = fs::read(password_path()?).map_err(|_| "没有保存的明文凭据")?;
    String::from_utf8(bytes)
        .map(Zeroizing::new)
        .map_err(|_| "明文凭据文件编码无效".into())
}
pub fn forget_password() -> Result<(), String> {
    let _ = fs::remove_file(password_path()?);
    match entry()?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(_) => Err("无法删除系统凭据；请在系统凭据库中手动移除".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_settings_default_to_hidden_management_and_system_theme() {
        let settings: Settings =
            serde_json::from_str(r#"{"server":"http://example.test/lfradius/"}"#).unwrap();
        assert!(!settings.show_sessions);
        assert!(!settings.log_enabled);
        assert!(matches!(settings.theme_mode, ThemeMode::System));
    }
    #[test]
    fn changing_display_preferences_does_not_change_authentication_options() {
        let mut settings = Settings {
            username: "test-user".into(),
            credential_store: CredentialStore::System,
            ..Default::default()
        };
        settings.set_ui_preferences(&UiPreferences {
            show_sessions: true,
            log_enabled: true,
            theme_mode: ThemeMode::Dark,
            refresh_policy: RefreshPolicy::Random,
            traffic_enabled: true,
        });
        assert_eq!(settings.username, "test-user");
        assert_eq!(settings.credential_store, CredentialStore::System);
        assert!(settings.show_sessions && settings.log_enabled);
        assert_eq!(settings.refresh_policy, RefreshPolicy::Random);
        assert!(settings.traffic_enabled);
        let decoded: Settings =
            serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert!(matches!(decoded.theme_mode, ThemeMode::Dark));
    }

    #[test]
    fn refresh_policy_has_six_expected_modes() {
        assert_eq!(
            RefreshPolicy::OneSecond.next_delay(),
            Some(Duration::from_secs(1))
        );
        assert_eq!(
            RefreshPolicy::TwoSeconds.next_delay(),
            Some(Duration::from_secs(2))
        );
        assert_eq!(
            RefreshPolicy::FiveSeconds.next_delay(),
            Some(Duration::from_secs(5))
        );
        assert_eq!(
            RefreshPolicy::OneMinute.next_delay(),
            Some(Duration::from_secs(60))
        );
        assert_eq!(RefreshPolicy::Disabled.next_delay(), None);
        let random = RefreshPolicy::Random.next_delay().unwrap().as_secs();
        assert!((1..=10).contains(&random));
    }
}
