use std::{
    collections::HashMap,
    env, fmt, fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use url::Url;

use crate::error::{AppError, AppResult};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AppConfig {
    #[serde(default)]
    pub rate_limit: RateLimitConfig,
    #[serde(default)]
    pub log: LogConfig,
    #[serde(default)]
    pub channels: HashMap<String, ChannelConfig>,
    #[serde(default)]
    pub routes: HashMap<String, RouteConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LogConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_log_path")]
    pub path: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RateLimitConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_rate_limit_per_second")]
    pub default_per_second: f64,
    #[serde(default = "default_rate_limit_burst")]
    pub default_burst: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ChannelRateLimitConfig {
    pub per_second: f64,
    pub burst: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SendCondition {
    Always,
    ScreenLocked,
}

impl Default for SendCondition {
    fn default() -> Self {
        Self::Always
    }
}

impl fmt::Display for SendCondition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SendCondition::Always => formatter.write_str("always"),
            SendCondition::ScreenLocked => formatter.write_str("screen_locked"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FeishuReceiveIdType {
    OpenId,
    UserId,
    UnionId,
    Email,
    ChatId,
}

impl fmt::Display for FeishuReceiveIdType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FeishuReceiveIdType::OpenId => formatter.write_str("open_id"),
            FeishuReceiveIdType::UserId => formatter.write_str("user_id"),
            FeishuReceiveIdType::UnionId => formatter.write_str("union_id"),
            FeishuReceiveIdType::Email => formatter.write_str("email"),
            FeishuReceiveIdType::ChatId => formatter.write_str("chat_id"),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RouteConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub send_when: SendCondition,
    #[serde(default)]
    pub channels: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ChannelConfig {
    Dingtalk {
        webhook: String,
        #[serde(default)]
        secret: Option<String>,
        #[serde(default = "default_true")]
        enabled: bool,
        #[serde(default)]
        send_when: SendCondition,
        #[serde(default = "default_timeout_seconds")]
        timeout_seconds: u64,
        #[serde(default)]
        rate_limit: Option<ChannelRateLimitConfig>,
    },
    Feishu {
        webhook: String,
        #[serde(default)]
        secret: Option<String>,
        #[serde(default = "default_true")]
        enabled: bool,
        #[serde(default)]
        send_when: SendCondition,
        #[serde(default = "default_timeout_seconds")]
        timeout_seconds: u64,
        #[serde(default)]
        rate_limit: Option<ChannelRateLimitConfig>,
    },
    #[serde(rename = "feishu_app")]
    FeishuApp {
        app_id: String,
        app_secret: String,
        receive_id_type: FeishuReceiveIdType,
        receive_id: String,
        #[serde(default = "default_true")]
        enabled: bool,
        #[serde(default)]
        send_when: SendCondition,
        #[serde(default = "default_timeout_seconds")]
        timeout_seconds: u64,
        #[serde(default)]
        rate_limit: Option<ChannelRateLimitConfig>,
    },
    Custom {
        url: String,
        #[serde(default)]
        headers: HashMap<String, String>,
        #[serde(default = "default_true")]
        enabled: bool,
        #[serde(default)]
        send_when: SendCondition,
        #[serde(default = "default_timeout_seconds")]
        timeout_seconds: u64,
        #[serde(default)]
        rate_limit: Option<ChannelRateLimitConfig>,
    },
}

impl AppConfig {
    pub fn load(path: impl AsRef<Path>) -> AppResult<Self> {
        let content = fs::read_to_string(path.as_ref()).map_err(|err| {
            AppError::Config(format!(
                "failed to read config {}: {err}",
                path.as_ref().display()
            ))
        })?;
        let mut config: AppConfig = toml::from_str(&content)
            .map_err(|err| AppError::Config(format!("failed to parse config: {err}")))?;
        config.normalize_paths(path.as_ref());
        config.validate()?;
        Ok(config)
    }

    fn normalize_paths(&mut self, config_path: &Path) {
        self.log.path = normalize_config_path(config_path, &self.log.path)
            .to_string_lossy()
            .replace('\\', "/");
    }

    pub fn validate(&self) -> AppResult<()> {
        if self.rate_limit.default_per_second < 0.0 || self.rate_limit.default_burst == 0 {
            return Err(AppError::Config(
                "rate_limit default values are invalid".into(),
            ));
        }
        if self.log.enabled && self.log.path.trim().is_empty() {
            return Err(AppError::Config(
                "log.path is required when log.enabled is true".into(),
            ));
        }
        for (name, channel) in &self.channels {
            validate_name("channel", name)?;
            channel.validate(name)?;
        }
        for (name, route) in &self.routes {
            validate_name("route", name)?;
            if route.channels.is_empty() {
                return Err(AppError::Config(format!(
                    "route {name} must reference at least one channel"
                )));
            }
            for channel in &route.channels {
                if !self.channels.contains_key(channel) {
                    return Err(AppError::Config(format!(
                        "route {name} references missing channel {channel}"
                    )));
                }
            }
        }
        Ok(())
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            rate_limit: RateLimitConfig::default(),
            log: LogConfig::default(),
            channels: HashMap::new(),
            routes: HashMap::new(),
        }
    }
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            path: default_log_path(),
        }
    }
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            default_per_second: default_rate_limit_per_second(),
            default_burst: default_rate_limit_burst(),
        }
    }
}

impl ChannelConfig {
    pub fn enabled(&self) -> bool {
        match self {
            ChannelConfig::Dingtalk { enabled, .. }
            | ChannelConfig::Feishu { enabled, .. }
            | ChannelConfig::FeishuApp { enabled, .. }
            | ChannelConfig::Custom { enabled, .. } => *enabled,
        }
    }

    pub fn timeout_seconds(&self) -> u64 {
        match self {
            ChannelConfig::Dingtalk {
                timeout_seconds, ..
            }
            | ChannelConfig::Feishu {
                timeout_seconds, ..
            }
            | ChannelConfig::FeishuApp {
                timeout_seconds, ..
            }
            | ChannelConfig::Custom {
                timeout_seconds, ..
            } => *timeout_seconds,
        }
    }

    pub fn rate_limit(&self) -> Option<&ChannelRateLimitConfig> {
        match self {
            ChannelConfig::Dingtalk { rate_limit, .. }
            | ChannelConfig::Feishu { rate_limit, .. }
            | ChannelConfig::FeishuApp { rate_limit, .. }
            | ChannelConfig::Custom { rate_limit, .. } => rate_limit.as_ref(),
        }
    }

    pub fn send_when(&self) -> SendCondition {
        match self {
            ChannelConfig::Dingtalk { send_when, .. }
            | ChannelConfig::Feishu { send_when, .. }
            | ChannelConfig::FeishuApp { send_when, .. }
            | ChannelConfig::Custom { send_when, .. } => *send_when,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            ChannelConfig::Dingtalk { .. } => "dingtalk",
            ChannelConfig::Feishu { .. } => "feishu",
            ChannelConfig::FeishuApp { .. } => "feishu_app",
            ChannelConfig::Custom { .. } => "custom",
        }
    }

    fn validate(&self, name: &str) -> AppResult<()> {
        if self.timeout_seconds() == 0 {
            return Err(AppError::Config(format!(
                "channel {name} timeout_seconds must be greater than 0"
            )));
        }
        if let Some(rate_limit) = self.rate_limit() {
            if rate_limit.per_second < 0.0 || rate_limit.burst == 0 {
                return Err(AppError::Config(format!(
                    "channel {name} rate_limit values are invalid"
                )));
            }
        }
        match self {
            ChannelConfig::Dingtalk { webhook, .. } | ChannelConfig::Feishu { webhook, .. } => {
                validate_http_url(name, webhook)
            }
            ChannelConfig::Custom { url, .. } => validate_http_url(name, url),
            ChannelConfig::FeishuApp {
                app_id,
                app_secret,
                receive_id,
                ..
            } => {
                if app_id.trim().is_empty() {
                    return Err(AppError::Config(format!(
                        "channel {name} app_id is required"
                    )));
                }
                if app_secret.trim().is_empty() {
                    return Err(AppError::Config(format!(
                        "channel {name} app_secret is required"
                    )));
                }
                if receive_id.trim().is_empty() {
                    return Err(AppError::Config(format!(
                        "channel {name} receive_id is required"
                    )));
                }
                Ok(())
            }
        }
    }
}

pub fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn normalize_config_path(config_path: &Path, value: &str) -> PathBuf {
    let expanded = expand_home(value);
    if expanded.is_absolute() {
        return expanded;
    }
    config_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .join(expanded)
}

fn expand_home(value: &str) -> PathBuf {
    if value == "~" {
        return home_dir().unwrap_or_else(|| PathBuf::from(value));
    }
    if let Some(rest) = value
        .strip_prefix("~/")
        .or_else(|| value.strip_prefix("~\\"))
    {
        if let Some(home) = home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(value)
}

fn validate_name(kind: &str, name: &str) -> AppResult<()> {
    if name.is_empty()
        || !name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
    {
        return Err(AppError::Config(format!(
            "{kind} name must use only letters, numbers, underscore, or hyphen: {name}"
        )));
    }
    Ok(())
}

fn validate_http_url(name: &str, value: &str) -> AppResult<()> {
    let url = Url::parse(value)
        .map_err(|_| AppError::Config(format!("channel {name} has invalid URL")))?;
    match url.scheme() {
        "http" | "https" => Ok(()),
        _ => Err(AppError::Config(format!(
            "channel {name} URL must use http or https"
        ))),
    }
}

fn default_true() -> bool {
    true
}
fn default_rate_limit_per_second() -> f64 {
    20.0
}
fn default_rate_limit_burst() -> usize {
    20
}
fn default_timeout_seconds() -> u64 {
    10
}
fn default_log_path() -> String {
    home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".local")
        .join("state")
        .join("nudgepost")
        .join("messages.jsonl")
        .to_string_lossy()
        .replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_is_disabled_by_default() {
        let config = AppConfig::default();
        assert!(!config.log.enabled);
        assert!(config.log.path.ends_with("messages.jsonl"));
    }

    #[test]
    fn rejects_route_with_missing_channel() {
        let mut config = AppConfig::default();
        config.routes.insert(
            "alerts".into(),
            RouteConfig {
                enabled: true,
                send_when: SendCondition::Always,
                channels: vec!["missing".into()],
            },
        );
        assert!(config.validate().is_err());
    }

    #[test]
    fn accepts_valid_channel_and_route() {
        let mut config = AppConfig::default();
        config.channels.insert(
            "ding_main".into(),
            ChannelConfig::Dingtalk {
                webhook: "https://oapi.dingtalk.com/robot/send?access_token=x".into(),
                secret: None,
                enabled: true,
                send_when: SendCondition::Always,
                timeout_seconds: 10,
                rate_limit: None,
            },
        );
        config.routes.insert(
            "alerts".into(),
            RouteConfig {
                enabled: true,
                send_when: SendCondition::Always,
                channels: vec!["ding_main".into()],
            },
        );
        assert!(config.validate().is_ok());
    }

    #[test]
    fn parses_screen_locked_send_condition() {
        let route: RouteConfig = toml::from_str(
            r#"
            enabled = true
            send_when = "screen_locked"
            channels = ["ding_main"]
            "#,
        )
        .unwrap();
        assert_eq!(route.send_when, SendCondition::ScreenLocked);
    }

    #[test]
    fn resolves_relative_log_path_from_config_directory() {
        let dir =
            std::env::temp_dir().join(format!("nudgepost-config-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join("nudgepost.toml");
        std::fs::write(
            &config_path,
            r#"
            [log]
            enabled = true
            path = "logs/messages.jsonl"

            [channels.custom_main]
            kind = "custom"
            url = "https://example.com/hook"

            [routes.alerts]
            channels = ["custom_main"]
            "#,
        )
        .unwrap();

        let config = AppConfig::load(&config_path).unwrap();
        assert_eq!(
            config.log.path,
            dir.join("logs")
                .join("messages.jsonl")
                .to_string_lossy()
                .replace('\\', "/")
        );
    }
}
