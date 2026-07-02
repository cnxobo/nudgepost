use std::{sync::Arc, time::Instant};

use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::warn;

use crate::{
    config::{AppConfig, ChannelConfig, SendCondition},
    error::{AppError, AppResult},
    message_log::append_message_log,
    rate_limit::RateLimiters,
    sender::{route_payload, send_channel},
    system_state::{ConditionEvaluator, ScreenLockState, SystemConditionEvaluator},
};

#[derive(Clone)]
pub struct MessageService {
    config: Arc<AppConfig>,
    rate_limiters: RateLimiters,
}

#[derive(Debug, Clone)]
pub struct SendRouteInput {
    pub route: String,
    pub title: String,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct SendChannelInput {
    pub channel: String,
    pub payload: Value,
}

#[derive(Debug, Clone)]
pub struct SendChannelTextInput {
    pub channel: String,
    pub title: String,
    pub text: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SendOutput {
    pub timestamp: DateTime<Utc>,
    pub message_id: String,
    pub entry_type: String,
    pub target: String,
    pub title: Option<String>,
    pub text: Option<String>,
    pub status: String,
    pub duration_ms: u128,
    pub deliveries: Vec<DeliveryResult>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DeliveryResult {
    pub channel: String,
    pub status: String,
    pub http_status: Option<i64>,
    pub error: Option<String>,
    pub attempts: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RouteInfo {
    pub name: String,
    pub enabled: bool,
    pub send_when: SendCondition,
    pub channel_count: usize,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ChannelInfo {
    pub name: String,
    pub kind: &'static str,
    pub enabled: bool,
    pub send_when: SendCondition,
    pub timeout_seconds: u64,
}

impl MessageService {
    pub fn new(config: Arc<AppConfig>) -> Self {
        let rate_limiters = RateLimiters::new(&config);
        Self {
            config,
            rate_limiters,
        }
    }

    pub async fn send_route(&self, input: SendRouteInput) -> AppResult<SendOutput> {
        self.send_route_with_entry_type("route", input).await
    }

    pub async fn send_hook_route(&self, input: SendRouteInput) -> AppResult<SendOutput> {
        self.send_route_with_entry_type("hook", input).await
    }

    pub async fn send_channel(&self, input: SendChannelInput) -> AppResult<SendOutput> {
        let channel = self.enabled_channel(&input.channel)?;
        let message_id = uuid::Uuid::new_v4().to_string();
        let started = Instant::now();
        let mut output = SendOutput {
            timestamp: Utc::now(),
            message_id,
            entry_type: "channel".into(),
            target: input.channel.clone(),
            title: None,
            text: None,
            status: "pending".into(),
            duration_ms: 0,
            deliveries: Vec::new(),
        };
        let delivery = self
            .send_one(
                &input.channel,
                channel,
                SendCondition::Always,
                &input.payload,
                &SystemConditionEvaluator,
            )
            .await;
        output.deliveries.push(delivery);
        output.status = aggregate_status(&output.deliveries);
        output.duration_ms = started.elapsed().as_millis();
        append_message_log(&self.config.log, &output)?;
        Ok(output)
    }

    pub async fn send_hook_channel(&self, input: SendChannelTextInput) -> AppResult<SendOutput> {
        let channel = self.enabled_channel(&input.channel)?;
        let message_id = uuid::Uuid::new_v4().to_string();
        let payload = route_payload(channel, &message_id, "hook", &input.title, &input.text);
        let started = Instant::now();
        let mut output = SendOutput {
            timestamp: Utc::now(),
            message_id,
            entry_type: "hook".into(),
            target: input.channel.clone(),
            title: Some(input.title),
            text: Some(input.text),
            status: "pending".into(),
            duration_ms: 0,
            deliveries: Vec::new(),
        };
        let delivery = self
            .send_one(
                &input.channel,
                channel,
                SendCondition::Always,
                &payload,
                &SystemConditionEvaluator,
            )
            .await;
        output.deliveries.push(delivery);
        output.status = aggregate_status(&output.deliveries);
        output.duration_ms = started.elapsed().as_millis();
        append_message_log(&self.config.log, &output)?;
        Ok(output)
    }

    pub fn list_routes(&self) -> Vec<RouteInfo> {
        let mut routes = self
            .config
            .routes
            .iter()
            .map(|(name, route)| RouteInfo {
                name: name.clone(),
                enabled: route.enabled,
                send_when: route.send_when,
                channel_count: route.channels.len(),
            })
            .collect::<Vec<_>>();
        routes.sort_by(|left, right| left.name.cmp(&right.name));
        routes
    }

    pub fn list_channels(&self) -> Vec<ChannelInfo> {
        let mut channels = self
            .config
            .channels
            .iter()
            .map(|(name, channel)| ChannelInfo {
                name: name.clone(),
                kind: channel.kind(),
                enabled: channel.enabled(),
                send_when: channel.send_when(),
                timeout_seconds: channel.timeout_seconds(),
            })
            .collect::<Vec<_>>();
        channels.sort_by(|left, right| left.name.cmp(&right.name));
        channels
    }

    async fn send_route_with_entry_type(
        &self,
        entry_type: &str,
        input: SendRouteInput,
    ) -> AppResult<SendOutput> {
        let route_config = self
            .config
            .routes
            .get(&input.route)
            .ok_or_else(|| AppError::NotFound(format!("route not found: {}", input.route)))?;
        if !route_config.enabled {
            return Err(AppError::BadRequest(format!(
                "route is disabled: {}",
                input.route
            )));
        }

        let message_id = uuid::Uuid::new_v4().to_string();
        let started = Instant::now();
        let mut output = SendOutput {
            timestamp: Utc::now(),
            message_id: message_id.clone(),
            entry_type: entry_type.into(),
            target: input.route.clone(),
            title: Some(input.title.clone()),
            text: Some(input.text.clone()),
            status: "pending".into(),
            duration_ms: 0,
            deliveries: Vec::with_capacity(route_config.channels.len()),
        };

        for channel_name in &route_config.channels {
            let channel = self.enabled_channel(channel_name)?;
            let payload = route_payload(
                channel,
                &message_id,
                &input.route,
                &input.title,
                &input.text,
            );
            let delivery = self
                .send_one(
                    channel_name,
                    channel,
                    route_config.send_when,
                    &payload,
                    &SystemConditionEvaluator,
                )
                .await;
            output.deliveries.push(delivery);
        }
        output.status = aggregate_status(&output.deliveries);
        output.duration_ms = started.elapsed().as_millis();
        append_message_log(&self.config.log, &output)?;
        Ok(output)
    }

    fn enabled_channel<'a>(&'a self, name: &str) -> AppResult<&'a ChannelConfig> {
        let channel = self
            .config
            .channels
            .get(name)
            .ok_or_else(|| AppError::NotFound(format!("channel not found: {name}")))?;
        if !channel.enabled() {
            return Err(AppError::BadRequest(format!("channel is disabled: {name}")));
        }
        Ok(channel)
    }

    async fn send_one(
        &self,
        channel_name: &str,
        channel: &ChannelConfig,
        route_condition: SendCondition,
        payload: &Value,
        evaluator: &dyn ConditionEvaluator,
    ) -> DeliveryResult {
        if let Some(reason) = skip_reason(route_condition, channel.send_when(), evaluator) {
            return DeliveryResult {
                channel: channel_name.into(),
                status: "skipped".into(),
                http_status: None,
                error: Some(reason),
                attempts: 0,
            };
        }

        self.rate_limiters.acquire(channel_name).await;
        let outcome = send_channel(&Client::new(), channel, payload).await;
        if outcome.success {
            DeliveryResult {
                channel: channel_name.into(),
                status: "succeeded".into(),
                http_status: outcome.status_code,
                error: None,
                attempts: 1,
            }
        } else {
            DeliveryResult {
                channel: channel_name.into(),
                status: "failed".into(),
                http_status: outcome.status_code,
                error: outcome.error.or_else(|| Some("unknown send error".into())),
                attempts: 1,
            }
        }
    }
}

fn skip_reason(
    route_condition: SendCondition,
    channel_condition: SendCondition,
    evaluator: &dyn ConditionEvaluator,
) -> Option<String> {
    let requires_screen_lock = route_condition == SendCondition::ScreenLocked
        || channel_condition == SendCondition::ScreenLocked;
    if !requires_screen_lock {
        return None;
    }
    match evaluator.screen_locked() {
        ScreenLockState::Locked => None,
        ScreenLockState::Unlocked => Some("condition not met: screen_locked".into()),
        ScreenLockState::Unknown(reason) => {
            warn!("screen lock state unknown, sending message anyway: {reason}");
            None
        }
    }
}

fn aggregate_status(deliveries: &[DeliveryResult]) -> String {
    if deliveries
        .iter()
        .any(|delivery| delivery.status == "failed")
    {
        "failed".into()
    } else if deliveries
        .iter()
        .all(|delivery| delivery.status == "skipped")
    {
        "skipped".into()
    } else {
        "succeeded".into()
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, net::SocketAddr, sync::Arc};

    use serde_json::json;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    use super::*;
    use crate::config::{ChannelConfig, RouteConfig};

    #[derive(Debug)]
    struct FakeEvaluator {
        state: ScreenLockState,
    }

    impl ConditionEvaluator for FakeEvaluator {
        fn screen_locked(&self) -> ScreenLockState {
            self.state.clone()
        }
    }

    #[test]
    fn direct_send_skips_when_screen_unlocked() {
        let evaluator = FakeEvaluator {
            state: ScreenLockState::Unlocked,
        };
        assert_eq!(
            skip_reason(
                SendCondition::ScreenLocked,
                SendCondition::Always,
                &evaluator
            )
            .as_deref(),
            Some("condition not met: screen_locked")
        );
    }

    #[test]
    fn direct_send_allows_when_screen_locked_or_unknown() {
        let locked = FakeEvaluator {
            state: ScreenLockState::Locked,
        };
        assert!(skip_reason(SendCondition::ScreenLocked, SendCondition::Always, &locked).is_none());

        let unknown = FakeEvaluator {
            state: ScreenLockState::Unknown("not available".into()),
        };
        assert!(
            skip_reason(SendCondition::Always, SendCondition::ScreenLocked, &unknown).is_none()
        );
    }

    #[tokio::test]
    async fn route_fanout_sends_to_all_channels() {
        let url1 = ok_server_url(200, r#"{"errcode":0,"errmsg":"ok"}"#).await;
        let url2 = ok_server_url(200, r#"{"code":0,"msg":"ok"}"#).await;
        let service = MessageService::new(Arc::new(test_config(&url1, &url2)));
        let output = service
            .send_route(SendRouteInput {
                route: "alerts".into(),
                title: "test".into(),
                text: "hello".into(),
            })
            .await
            .unwrap();

        assert_eq!(output.status, "succeeded");
        assert_eq!(output.deliveries.len(), 2);
        assert!(
            output
                .deliveries
                .iter()
                .all(|delivery| delivery.attempts == 1)
        );
    }

    #[tokio::test]
    async fn route_failure_sets_failed_status() {
        let url1 = ok_server_url(500, r#"{"errcode":500,"errmsg":"boom"}"#).await;
        let url2 = ok_server_url(200, r#"{"code":0,"msg":"ok"}"#).await;
        let service = MessageService::new(Arc::new(test_config(&url1, &url2)));
        let output = service
            .send_route(SendRouteInput {
                route: "alerts".into(),
                title: "test".into(),
                text: "hello".into(),
            })
            .await
            .unwrap();

        assert_eq!(output.status, "failed");
        assert!(
            output
                .deliveries
                .iter()
                .any(|delivery| delivery.status == "failed")
        );
    }

    async fn ok_server_url(status_code: u16, body: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr: SocketAddr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = [0u8; 4096];
            let _ = socket.read(&mut buffer).await;
            let response = format!(
                "HTTP/1.1 {status_code} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });
        format!("http://{addr}/hook")
    }

    fn test_config(url1: &str, url2: &str) -> AppConfig {
        AppConfig {
            channels: HashMap::from([
                (
                    "ding_main".into(),
                    ChannelConfig::Dingtalk {
                        webhook: url1.into(),
                        secret: None,
                        enabled: true,
                        send_when: SendCondition::Always,
                        timeout_seconds: 1,
                        rate_limit: None,
                    },
                ),
                (
                    "feishu_main".into(),
                    ChannelConfig::Feishu {
                        webhook: url2.into(),
                        secret: None,
                        enabled: true,
                        send_when: SendCondition::Always,
                        timeout_seconds: 1,
                        rate_limit: None,
                    },
                ),
            ]),
            routes: HashMap::from([(
                "alerts".into(),
                RouteConfig {
                    enabled: true,
                    send_when: SendCondition::Always,
                    channels: vec!["ding_main".into(), "feishu_main".into()],
                },
            )]),
            ..AppConfig::default()
        }
    }
}
