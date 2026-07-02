use std::{
    collections::{HashMap, hash_map::DefaultHasher},
    hash::{Hash, Hasher},
    sync::{Mutex, OnceLock},
};

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use reqwest::{Client, Response, StatusCode};
use serde::Deserialize;
use serde_json::Map;
use serde_json::{Value, json};

use crate::{
    config::{ChannelConfig, FeishuReceiveIdType},
    signing::{dingtalk_sign, feishu_sign},
};

#[derive(Debug)]
pub struct SendOutcome {
    pub success: bool,
    pub status_code: Option<i64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
struct CachedToken {
    token: String,
    refresh_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
struct FeishuTokenResponse {
    code: i64,
    msg: Option<String>,
    tenant_access_token: Option<String>,
    expire: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct FeishuApiResponse {
    code: i64,
    msg: Option<String>,
}

static FEISHU_TOKEN_CACHE: OnceLock<Mutex<HashMap<String, CachedToken>>> = OnceLock::new();

pub fn route_payload(
    channel: &ChannelConfig,
    message_id: &str,
    route: &str,
    title: &str,
    text: &str,
) -> Value {
    let content = if title.trim().is_empty() {
        text.to_owned()
    } else {
        format!("{title}\n{text}")
    };
    match channel {
        ChannelConfig::Dingtalk { .. } => {
            json!({"msgtype": "text", "text": {"content": content}})
        }
        ChannelConfig::Feishu { .. } => {
            json!({"msg_type": "text", "content": {"text": content}})
        }
        ChannelConfig::FeishuApp { .. } => {
            json!({"msg_type": "text", "content": json!({"text": content}).to_string()})
        }
        ChannelConfig::Custom { .. } => {
            json!({"message_id": message_id, "route": route, "title": title, "text": text})
        }
    }
}

pub async fn send_channel(
    client: &Client,
    channel: &ChannelConfig,
    payload: &Value,
) -> SendOutcome {
    let timeout = std::time::Duration::from_secs(channel.timeout_seconds());
    let result = match channel {
        ChannelConfig::Dingtalk {
            webhook, secret, ..
        } => {
            let mut url = webhook.clone();
            if let Some(secret) = secret.as_deref().filter(|secret| !secret.trim().is_empty()) {
                let timestamp = Utc::now().timestamp_millis();
                let sign = dingtalk_sign(timestamp, secret);
                let separator = if url.contains('?') { '&' } else { '?' };
                url = format!("{url}{separator}timestamp={timestamp}&sign={sign}");
            }
            let result = client.post(url).timeout(timeout).json(payload).send().await;
            return bot_webhook_outcome(result, "DingTalk").await;
        }
        ChannelConfig::Feishu {
            webhook, secret, ..
        } => {
            let mut signed_payload = payload.clone();
            if let Some(secret) = secret.as_deref().filter(|secret| !secret.trim().is_empty()) {
                let timestamp = Utc::now().timestamp();
                if let Value::Object(object) = &mut signed_payload {
                    object.insert("timestamp".into(), json!(timestamp.to_string()));
                    object.insert("sign".into(), json!(feishu_sign(timestamp, secret)));
                }
            }
            let result = client
                .post(webhook)
                .timeout(timeout)
                .json(&signed_payload)
                .send()
                .await;
            return bot_webhook_outcome(result, "Feishu").await;
        }
        ChannelConfig::FeishuApp {
            app_id,
            app_secret,
            receive_id_type,
            receive_id,
            ..
        } => {
            return send_feishu_app(
                client,
                app_id,
                app_secret,
                *receive_id_type,
                receive_id,
                payload,
                timeout,
            )
            .await;
        }
        ChannelConfig::Custom { url, headers, .. } => {
            let mut request = client.post(url).timeout(timeout).json(payload);
            for (name, value) in headers {
                request = request.header(name, value);
            }
            request.send().await
        }
    };

    match result {
        Ok(response) => {
            let status = response.status();
            if status.is_success() {
                SendOutcome {
                    success: true,
                    status_code: Some(status.as_u16() as i64),
                    error: None,
                }
            } else {
                SendOutcome {
                    success: false,
                    status_code: Some(status.as_u16() as i64),
                    error: Some(format_http_error(status)),
                }
            }
        }
        Err(err) => SendOutcome {
            success: false,
            status_code: None,
            error: Some(format!("request failed: {}", err.without_url())),
        },
    }
}

async fn bot_webhook_outcome(
    result: Result<Response, reqwest::Error>,
    provider: &str,
) -> SendOutcome {
    match result {
        Ok(response) => {
            let status = response.status();
            let status_code = Some(status.as_u16() as i64);
            if !status.is_success() {
                return SendOutcome {
                    success: false,
                    status_code,
                    error: Some(format_http_error(status)),
                };
            }
            let text = match response.text().await {
                Ok(text) => text,
                Err(err) => {
                    return SendOutcome {
                        success: false,
                        status_code,
                        error: Some(format!("failed to read {provider} response: {err}")),
                    };
                }
            };
            match serde_json::from_str::<Value>(&text) {
                Ok(body) => {
                    if let Some(error) = bot_business_error(provider, &body) {
                        SendOutcome {
                            success: false,
                            status_code,
                            error: Some(error),
                        }
                    } else {
                        SendOutcome {
                            success: true,
                            status_code,
                            error: None,
                        }
                    }
                }
                Err(err) => SendOutcome {
                    success: false,
                    status_code,
                    error: Some(format!("failed to parse {provider} response: {err}")),
                },
            }
        }
        Err(err) => SendOutcome {
            success: false,
            status_code: None,
            error: Some(format!("request failed: {}", err.without_url())),
        },
    }
}

fn bot_business_error(provider: &str, body: &Value) -> Option<String> {
    for field in ["errcode", "code", "StatusCode"] {
        if let Some(code) = body.get(field).and_then(Value::as_i64) {
            if code != 0 {
                let message = body
                    .get("errmsg")
                    .or_else(|| body.get("msg"))
                    .or_else(|| body.get("StatusMessage"))
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                return Some(format!(
                    "{provider} webhook failed: code={code} msg={message}"
                ));
            }
            return None;
        }
    }
    None
}

async fn send_feishu_app(
    client: &Client,
    app_id: &str,
    app_secret: &str,
    receive_id_type: FeishuReceiveIdType,
    receive_id: &str,
    payload: &Value,
    timeout: std::time::Duration,
) -> SendOutcome {
    let token = match tenant_access_token(client, app_id, app_secret, timeout).await {
        Ok(token) => token,
        Err(error) => {
            return SendOutcome {
                success: false,
                status_code: None,
                error: Some(error),
            };
        }
    };

    let body = match feishu_app_message_body(receive_id, payload) {
        Ok(body) => body,
        Err(error) => {
            return SendOutcome {
                success: false,
                status_code: None,
                error: Some(error),
            };
        }
    };
    let url = format!(
        "https://open.feishu.cn/open-apis/im/v1/messages?receive_id_type={}",
        receive_id_type
    );

    let result = client
        .post(url)
        .timeout(timeout)
        .bearer_auth(token)
        .json(&body)
        .send()
        .await;

    match result {
        Ok(response) => {
            let status = response.status();
            let status_code = Some(status.as_u16() as i64);
            if !status.is_success() {
                return SendOutcome {
                    success: false,
                    status_code,
                    error: Some(format_http_error(status)),
                };
            }
            match response.json::<FeishuApiResponse>().await {
                Ok(body) if body.code == 0 => SendOutcome {
                    success: true,
                    status_code,
                    error: None,
                },
                Ok(body) => SendOutcome {
                    success: false,
                    status_code,
                    error: Some(format_feishu_error("message send", body.code, body.msg)),
                },
                Err(err) => SendOutcome {
                    success: false,
                    status_code,
                    error: Some(format!("failed to parse Feishu response: {err}")),
                },
            }
        }
        Err(err) => SendOutcome {
            success: false,
            status_code: None,
            error: Some(format!("request failed: {}", err.without_url())),
        },
    }
}

async fn tenant_access_token(
    client: &Client,
    app_id: &str,
    app_secret: &str,
    timeout: std::time::Duration,
) -> Result<String, String> {
    let key = token_cache_key(app_id, app_secret);
    let cache = FEISHU_TOKEN_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(token) = cache
        .lock()
        .map_err(|_| "Feishu token cache is poisoned".to_owned())?
        .get(&key)
        .filter(|cached| cached.refresh_at > Utc::now())
        .map(|cached| cached.token.clone())
    {
        return Ok(token);
    }

    let response = client
        .post("https://open.feishu.cn/open-apis/auth/v3/tenant_access_token/internal")
        .timeout(timeout)
        .json(&json!({"app_id": app_id, "app_secret": app_secret}))
        .send()
        .await
        .map_err(|err| format!("Feishu token request failed: {}", err.without_url()))?;
    if !response.status().is_success() {
        return Err(format_http_error(response.status()));
    }

    let body = response
        .json::<FeishuTokenResponse>()
        .await
        .map_err(|err| format!("failed to parse Feishu token response: {err}"))?;
    if body.code != 0 {
        return Err(format_feishu_error("token", body.code, body.msg));
    }
    let token = body
        .tenant_access_token
        .ok_or_else(|| "Feishu token response did not include tenant_access_token".to_owned())?;
    let expire = body.expire.unwrap_or(7200).max(600);
    let refresh_after = expire.saturating_sub(300).max(60);
    let cached = CachedToken {
        token: token.clone(),
        refresh_at: Utc::now() + ChronoDuration::seconds(refresh_after),
    };
    cache
        .lock()
        .map_err(|_| "Feishu token cache is poisoned".to_owned())?
        .insert(key, cached);
    Ok(token)
}

fn token_cache_key(app_id: &str, app_secret: &str) -> String {
    let mut hasher = DefaultHasher::new();
    app_secret.hash(&mut hasher);
    format!("{app_id}:{}", hasher.finish())
}

fn feishu_app_message_body(receive_id: &str, payload: &Value) -> Result<Value, String> {
    let mut object = match payload {
        Value::String(text) => feishu_text_payload(text),
        Value::Object(object) => {
            if object.contains_key("receive_id") || object.contains_key("receive_id_type") {
                return Err(
                    "invalid feishu_app payload: receive_id is configured on the channel".into(),
                );
            }
            if let Some(text) = object.get("text").and_then(Value::as_str) {
                feishu_text_payload(text)
            } else if object.contains_key("msg_type") && object.contains_key("content") {
                object.clone()
            } else {
                return Err("invalid feishu_app payload: expected text or msg_type/content".into());
            }
        }
        _ => return Err("invalid feishu_app payload: expected object or string".into()),
    };
    object.insert("receive_id".into(), Value::String(receive_id.to_owned()));
    Ok(Value::Object(object))
}

fn feishu_text_payload(text: &str) -> Map<String, Value> {
    let mut object = Map::new();
    object.insert("msg_type".into(), Value::String("text".into()));
    object.insert(
        "content".into(),
        Value::String(json!({"text": text}).to_string()),
    );
    object
}

fn format_http_error(status: StatusCode) -> String {
    format!("remote returned HTTP {}", status.as_u16())
}

fn format_feishu_error(operation: &str, code: i64, msg: Option<String>) -> String {
    format!(
        "Feishu {operation} failed: code={code} msg={}",
        msg.unwrap_or_else(|| "unknown".into())
    )
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn route_payload_for_custom_contains_standard_fields() {
        let payload = route_payload(
            &ChannelConfig::Custom {
                url: "https://example.com/hook".into(),
                headers: HashMap::new(),
                enabled: true,
                send_when: crate::config::SendCondition::Always,
                timeout_seconds: 10,
                rate_limit: None,
            },
            "mid",
            "alerts",
            "title",
            "body",
        );
        assert_eq!(payload["message_id"], "mid");
        assert_eq!(payload["route"], "alerts");
        assert_eq!(payload["title"], "title");
        assert_eq!(payload["text"], "body");
    }

    #[test]
    fn route_payload_for_feishu_uses_text_shape() {
        let payload = route_payload(
            &ChannelConfig::Feishu {
                webhook: "https://example.com".into(),
                secret: None,
                enabled: true,
                send_when: crate::config::SendCondition::Always,
                timeout_seconds: 10,
                rate_limit: None,
            },
            "mid",
            "alerts",
            "title",
            "body",
        );
        assert_eq!(payload["msg_type"], "text");
        assert_eq!(payload["content"]["text"], "title\nbody");
    }

    #[test]
    fn route_payload_for_feishu_app_uses_message_create_shape() {
        let payload = route_payload(
            &ChannelConfig::FeishuApp {
                app_id: "cli_test".into(),
                app_secret: "secret".into(),
                receive_id_type: FeishuReceiveIdType::OpenId,
                receive_id: "ou_test".into(),
                enabled: true,
                send_when: crate::config::SendCondition::Always,
                timeout_seconds: 10,
                rate_limit: None,
            },
            "mid",
            "alerts",
            "title",
            "body",
        );
        assert_eq!(payload["msg_type"], "text");
        assert_eq!(
            payload["content"],
            json!({"text": "title\nbody"}).to_string()
        );
    }

    #[test]
    fn feishu_app_payload_rejects_receive_id_override() {
        let result = feishu_app_message_body(
            "ou_test",
            &json!({"receive_id": "other", "msg_type": "text", "content": "{}"}),
        );
        assert!(result.is_err());
    }

    #[test]
    fn feishu_app_payload_adds_configured_receive_id() {
        let body = feishu_app_message_body("ou_test", &json!({"text": "hello"})).unwrap();
        assert_eq!(body["receive_id"], "ou_test");
        assert_eq!(body["msg_type"], "text");
        assert_eq!(body["content"], json!({"text": "hello"}).to_string());
    }

    #[test]
    fn bot_business_error_detects_dingtalk_error_code() {
        let error = bot_business_error(
            "DingTalk",
            &json!({"errcode": 310000, "errmsg": "invalid keywords"}),
        )
        .unwrap();
        assert!(error.contains("code=310000"));
        assert!(error.contains("invalid keywords"));
    }

    #[test]
    fn bot_business_error_accepts_success_code() {
        assert!(bot_business_error("Feishu", &json!({"code": 0, "msg": "success"})).is_none());
        assert!(
            bot_business_error(
                "Feishu",
                &json!({"StatusCode": 0, "StatusMessage": "success"})
            )
            .is_none()
        );
    }
}
