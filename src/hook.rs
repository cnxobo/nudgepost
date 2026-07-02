use std::{
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, bail};
use clap::ValueEnum;
use serde_json::{Map, Value, json};

use crate::config::AppConfig;

const STATUS_MESSAGE: &str = "Sending AI agent update";
const COMMAND_TIMEOUT_SECONDS: u64 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum HookAgent {
    Codex,
    Claude,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookTarget {
    Route(String),
    Channel(String),
}

#[derive(Debug, Clone)]
pub struct InstallOptions {
    pub agent: HookAgent,
    pub target: HookTarget,
    pub config: PathBuf,
    pub force: bool,
    pub dry_run: bool,
}

impl HookAgent {
    pub fn as_str(self) -> &'static str {
        match self {
            HookAgent::Codex => "codex",
            HookAgent::Claude => "claude",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            HookAgent::Codex => "Codex",
            HookAgent::Claude => "Claude",
        }
    }

    fn config_path(self) -> anyhow::Result<PathBuf> {
        let home = env::var_os("HOME")
            .or_else(|| env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .context("HOME or USERPROFILE is not set")?;
        Ok(match self {
            HookAgent::Codex => home.join(".codex").join("hooks.json"),
            HookAgent::Claude => home.join(".claude").join("settings.json"),
        })
    }

    fn events(self) -> &'static [&'static str] {
        match self {
            HookAgent::Codex => &["Stop", "SubagentStop"],
            HookAgent::Claude => &["Notification", "Stop", "SubagentStop"],
        }
    }
}

impl HookTarget {
    pub fn from_route_channel(
        route: Option<String>,
        channel: Option<String>,
    ) -> anyhow::Result<Self> {
        match (route, channel) {
            (Some(route), None) if !route.trim().is_empty() => Ok(HookTarget::Route(route)),
            (None, Some(channel)) if !channel.trim().is_empty() => Ok(HookTarget::Channel(channel)),
            (Some(_), Some(_)) => bail!("use exactly one of --route or --channel"),
            _ => bail!("one of --route or --channel is required"),
        }
    }

    pub fn command_args(&self) -> Vec<String> {
        match self {
            HookTarget::Route(route) => vec!["--route".into(), route.clone()],
            HookTarget::Channel(channel) => vec!["--channel".into(), channel.clone()],
        }
    }

    fn validate(&self, config: &AppConfig) -> anyhow::Result<()> {
        match self {
            HookTarget::Route(route) => {
                let route_config = config
                    .routes
                    .get(route)
                    .with_context(|| format!("route not found: {route}"))?;
                if !route_config.enabled {
                    bail!("route is disabled: {route}");
                }
            }
            HookTarget::Channel(channel) => {
                let channel_config = config
                    .channels
                    .get(channel)
                    .with_context(|| format!("channel not found: {channel}"))?;
                if !channel_config.enabled() {
                    bail!("channel is disabled: {channel}");
                }
            }
        }
        Ok(())
    }
}

pub fn install(options: InstallOptions) -> anyhow::Result<()> {
    let config_path =
        friendly_windows_path(options.config.canonicalize().with_context(|| {
            format!("failed to resolve config path {}", options.config.display())
        })?);
    let app_config = AppConfig::load(&config_path)?;
    options.target.validate(&app_config)?;

    let hook_path = options.agent.config_path()?;
    let command = hook_command(
        &env::current_exe().context("failed to locate current executable")?,
        options.agent,
        &options.target,
        &config_path,
    );
    let mut root = read_json_config(&hook_path)?;
    let changed = upsert_hook(&mut root, options.agent, &command, options.force)?;

    if options.dry_run {
        println!("hook config: {}", hook_path.display());
        println!("agent: {}", options.agent.as_str());
        println!("command: {command}");
        println!("events: {}", options.agent.events().join(", "));
        println!("snippet:");
        println!(
            "{}",
            serde_json::to_string_pretty(&hook_snippet(options.agent, &command))?
        );
        return Ok(());
    }

    if changed {
        if let Some(parent) = hook_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        fs::write(&hook_path, serde_json::to_string_pretty(&root)? + "\n")
            .with_context(|| format!("failed to write {}", hook_path.display()))?;
    }

    println!("hook installed: {}", hook_path.display());
    println!("agent: {}", options.agent.as_str());
    println!("events: {}", options.agent.events().join(", "));
    println!("status: {}", if changed { "updated" } else { "unchanged" });
    println!(
        "next: open /hooks in {} to review/trust the hook if prompted",
        options.agent.label()
    );
    Ok(())
}

pub fn uninstall(agent: HookAgent, dry_run: bool) -> anyhow::Result<()> {
    let hook_path = agent.config_path()?;
    if !hook_path.exists() {
        println!("hook not installed: {}", hook_path.display());
        return Ok(());
    }

    let mut root = read_json_config(&hook_path)?;
    let changed = remove_agent_hooks(&mut root, agent);
    if dry_run {
        println!("hook config: {}", hook_path.display());
        println!("agent: {}", agent.as_str());
        println!(
            "would_remove: {}",
            if changed {
                "yes"
            } else {
                "no matching nudgepost hook"
            }
        );
        return Ok(());
    }
    if changed {
        fs::write(&hook_path, serde_json::to_string_pretty(&root)? + "\n")
            .with_context(|| format!("failed to write {}", hook_path.display()))?;
    }
    println!("hook uninstalled: {}", hook_path.display());
    println!("status: {}", if changed { "updated" } else { "unchanged" });
    Ok(())
}

pub fn status_message(
    agent: HookAgent,
    event_arg: Option<&str>,
    message_arg: Option<&str>,
    stdin: &str,
) -> (String, String) {
    let input = serde_json::from_str::<Value>(stdin).ok();
    let event = event_arg
        .filter(|event| !event.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| string_field(input.as_ref(), "hook_event_name"))
        .unwrap_or_else(|| "hook".into());

    let title = title_for_event(agent, &event);
    let fallback = fallback_summary(
        message_arg,
        input.as_ref(),
        if stdin.trim().is_empty() || input.is_some() {
            None
        } else {
            Some(stdin)
        },
    );
    let summary = if let Some(mut summary) = transcript_summary(input.as_ref()) {
        summary.fill_missing(fallback);
        summary
    } else {
        fallback
    };

    (title, summary.text())
}

fn read_json_config(path: &Path) -> anyhow::Result<Value> {
    if !path.exists() {
        return Ok(json!({}));
    }
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    if content.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(&content)
        .with_context(|| format!("failed to parse JSON config {}", path.display()))
}

fn hook_command(exe: &Path, agent: HookAgent, target: &HookTarget, config: &Path) -> String {
    let mut args = vec![
        exe.to_string_lossy().to_string(),
        "hook".into(),
        "run".into(),
        "--agent".into(),
        agent.as_str().into(),
    ];
    args.extend(target.command_args());
    args.push("--config".into());
    args.push(config.to_string_lossy().to_string());
    args.into_iter()
        .map(|arg| quote_shell_arg(&arg))
        .collect::<Vec<_>>()
        .join(" ")
}

fn hook_snippet(agent: HookAgent, command: &str) -> Value {
    let mut root = json!({});
    let hooks = root
        .as_object_mut()
        .expect("root is object")
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .expect("hooks is object");
    for event in agent.events() {
        hooks.insert(
            (*event).to_string(),
            json!([{
                "matcher": "",
                "hooks": [{
                    "type": "command",
                    "command": command,
                    "timeout": COMMAND_TIMEOUT_SECONDS,
                    "statusMessage": STATUS_MESSAGE
                }]
            }]),
        );
    }
    root
}

fn upsert_hook(
    root: &mut Value,
    agent: HookAgent,
    command: &str,
    force: bool,
) -> anyhow::Result<bool> {
    ensure_object(root)?;
    if has_agent_hook(root, agent) {
        if !force && !has_exact_command(root, command) {
            bail!(
                "{} hook is already installed; rerun with --force to replace it",
                agent.label()
            );
        }
        remove_agent_hooks(root, agent);
    }

    let hooks = ensure_child_object(root, "hooks")?;
    for event in agent.events() {
        let groups = hooks
            .entry((*event).to_string())
            .or_insert_with(|| Value::Array(Vec::new()));
        if !groups.is_array() {
            *groups = Value::Array(Vec::new());
        }
        groups.as_array_mut().expect("groups is array").push(json!({
            "matcher": "",
            "hooks": [{
                "type": "command",
                "command": command,
                "timeout": COMMAND_TIMEOUT_SECONDS,
                "statusMessage": STATUS_MESSAGE
            }]
        }));
    }
    Ok(true)
}

fn remove_agent_hooks(root: &mut Value, agent: HookAgent) -> bool {
    let Some(hooks) = root.get_mut("hooks").and_then(Value::as_object_mut) else {
        return false;
    };
    let mut changed = false;
    let events = hooks.keys().cloned().collect::<Vec<_>>();
    for event in events {
        let Some(groups) = hooks.get_mut(&event).and_then(Value::as_array_mut) else {
            continue;
        };
        let before = groups.len();
        for group in groups.iter_mut() {
            if let Some(handlers) = group.get_mut("hooks").and_then(Value::as_array_mut) {
                let handler_before = handlers.len();
                handlers.retain(|handler| !is_agent_handler(handler, agent));
                changed |= handlers.len() != handler_before;
            }
        }
        groups.retain(|group| {
            group
                .get("hooks")
                .and_then(Value::as_array)
                .map(|handlers| !handlers.is_empty())
                .unwrap_or(true)
        });
        changed |= groups.len() != before;
    }
    hooks.retain(|_, groups| {
        groups
            .as_array()
            .map(|items| !items.is_empty())
            .unwrap_or(true)
    });
    changed
}

fn has_agent_hook(root: &Value, agent: HookAgent) -> bool {
    root.get("hooks")
        .and_then(Value::as_object)
        .map(|hooks| {
            hooks.values().any(|groups| {
                groups.as_array().map_or(false, |groups| {
                    groups.iter().any(|group| {
                        group
                            .get("hooks")
                            .and_then(Value::as_array)
                            .map_or(false, |handlers| {
                                handlers
                                    .iter()
                                    .any(|handler| is_agent_handler(handler, agent))
                            })
                    })
                })
            })
        })
        .unwrap_or(false)
}

fn has_exact_command(root: &Value, command: &str) -> bool {
    root.get("hooks")
        .and_then(Value::as_object)
        .map(|hooks| {
            hooks.values().any(|groups| {
                groups.as_array().map_or(false, |groups| {
                    groups.iter().any(|group| {
                        group
                            .get("hooks")
                            .and_then(Value::as_array)
                            .map_or(false, |handlers| {
                                handlers.iter().any(|handler| {
                                    handler
                                        .get("command")
                                        .and_then(Value::as_str)
                                        .map(|value| value == command)
                                        .unwrap_or(false)
                                })
                            })
                    })
                })
            })
        })
        .unwrap_or(false)
}

fn is_agent_handler(handler: &Value, agent: HookAgent) -> bool {
    handler
        .get("command")
        .and_then(Value::as_str)
        .map(|command| {
            (command.contains("nudgepost") || command.contains("waypost"))
                && command.contains("hook")
                && command.contains("run")
                && command.contains("--agent")
                && command.contains(agent.as_str())
        })
        .unwrap_or(false)
}

fn ensure_object(value: &mut Value) -> anyhow::Result<&mut Map<String, Value>> {
    if !value.is_object() {
        bail!("hook config root must be a JSON object");
    }
    Ok(value.as_object_mut().expect("root is object"))
}

fn ensure_child_object<'a>(
    value: &'a mut Value,
    key: &str,
) -> anyhow::Result<&'a mut Map<String, Value>> {
    let object = ensure_object(value)?;
    object
        .entry(key.to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    if !object.get(key).unwrap().is_object() {
        bail!("{key} must be a JSON object");
    }
    Ok(object
        .get_mut(key)
        .and_then(Value::as_object_mut)
        .expect("child is object"))
}

fn quote_shell_arg(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || "-_./:\\=".contains(ch))
    {
        return value.to_owned();
    }
    format!("\"{}\"", value.replace('"', "\\\""))
}

fn friendly_windows_path(path: PathBuf) -> PathBuf {
    if cfg!(windows) {
        let text = path.to_string_lossy();
        if let Some(rest) = text.strip_prefix(r"\\?\") {
            return PathBuf::from(rest);
        }
    }
    path
}

fn string_field(value: Option<&Value>, field: &str) -> Option<String> {
    value?
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

#[derive(Debug, Clone, Default)]
struct HookSummary {
    request: Option<String>,
    result: Option<String>,
}

impl HookSummary {
    fn fill_missing(&mut self, fallback: HookSummary) {
        if self.request.is_none() {
            self.request = fallback.request;
        }
        if self.result.is_none() {
            self.result = fallback.result;
        }
    }

    fn text(&self) -> String {
        let request = self.request.as_deref().unwrap_or("unknown");
        let result = self
            .result
            .as_deref()
            .unwrap_or("no final response captured");
        format!("请求：{request}\n结果：{result}")
    }
}

fn title_for_event(agent: HookAgent, event: &str) -> String {
    let agent = agent.label();
    match event {
        "Notification" => format!("{agent} needs attention"),
        "Stop" => format!("{agent} finished"),
        "SubagentStop" => format!("{agent} subagent finished"),
        "SessionEnd" => format!("{agent} session ended"),
        "StopFailure" => format!("{agent} stop hook failed"),
        "TaskCompleted" => format!("{agent} task completed"),
        _ => format!("{agent} hook event"),
    }
}

fn transcript_summary(input: Option<&Value>) -> Option<HookSummary> {
    let input = input?;
    for transcript_path in transcript_paths(input) {
        if let Some(summary) = transcript_summary_from_path(input, &transcript_path) {
            return Some(summary);
        }
    }
    None
}

fn transcript_paths(input: &Value) -> Vec<String> {
    let mut paths = Vec::new();
    if string_field(Some(input), "hook_event_name").as_deref() == Some("SubagentStop") {
        if let Some(path) = string_field(Some(input), "agent_transcript_path") {
            paths.push(path);
        }
    }
    if let Some(path) = string_field(Some(input), "transcript_path") {
        if !paths.iter().any(|existing| existing == &path) {
            paths.push(path);
        }
    }
    paths
}

fn transcript_summary_from_path(input: &Value, transcript_path: &str) -> Option<HookSummary> {
    let content = fs::read_to_string(transcript_path).ok()?;
    let records = content
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect::<Vec<_>>();
    if records.is_empty() {
        return None;
    }

    let turn_id = string_field(Some(input), "turn_id").or_else(|| latest_turn_id(&records));
    let mut summary = HookSummary::default();

    for record in &records {
        if !record_matches_turn(record, turn_id.as_deref()) {
            continue;
        }
        apply_codex_transcript_record(record, &mut summary);
        apply_claude_transcript_record(record, &mut summary);
    }

    if summary.request.is_some() || summary.result.is_some() {
        Some(summary)
    } else {
        None
    }
}

fn apply_codex_transcript_record(record: &Value, summary: &mut HookSummary) {
    if record.get("type").and_then(Value::as_str) == Some("event_msg") {
        let payload = record.get("payload");
        match payload
            .and_then(|payload| payload.get("type"))
            .and_then(Value::as_str)
        {
            Some("user_message") => {
                if let Some(message) = string_field(payload, "message") {
                    summary.request = Some(clean_summary_text(&message));
                }
            }
            Some("agent_message") => {
                if let Some(message) = string_field(payload, "message") {
                    summary.result = Some(clean_summary_text(&message));
                }
            }
            Some("task_complete") => {
                if let Some(message) = string_field(payload, "last_agent_message") {
                    summary.result = Some(clean_summary_text(&message));
                }
            }
            Some("turn_aborted") if summary.result.is_none() => {
                summary.result = Some("Turn was interrupted.".into());
            }
            Some("error") if summary.result.is_none() => {
                summary.result = Some("Turn failed.".into());
            }
            _ => {}
        }
    } else if record.get("type").and_then(Value::as_str) == Some("response_item") {
        let payload = record.get("payload");
        if payload
            .and_then(|payload| payload.get("type"))
            .and_then(Value::as_str)
            != Some("message")
        {
            return;
        }
        let Some(text) = message_content_text(payload) else {
            return;
        };
        match payload
            .and_then(|payload| payload.get("role"))
            .and_then(Value::as_str)
        {
            Some("user") => summary.request = Some(clean_summary_text(&text)),
            Some("assistant") => summary.result = Some(clean_summary_text(&text)),
            _ => {}
        }
    }
}

fn apply_claude_transcript_record(record: &Value, summary: &mut HookSummary) {
    let role = record
        .get("message")
        .and_then(|message| message.get("role"))
        .and_then(Value::as_str)
        .or_else(|| record.get("type").and_then(Value::as_str));
    let Some(role) = role else {
        return;
    };
    let Some(text) = claude_record_text(record) else {
        return;
    };
    match role {
        "user" => summary.request = Some(clean_summary_text(&text)),
        "assistant" => summary.result = Some(clean_summary_text(&text)),
        _ => {}
    }
}

fn latest_turn_id(records: &[Value]) -> Option<String> {
    records.iter().rev().find_map(|record| {
        if record.get("type").and_then(Value::as_str) != Some("event_msg") {
            return None;
        }
        let payload = record.get("payload")?;
        let event = payload.get("type").and_then(Value::as_str)?;
        if matches!(event, "task_complete" | "turn_aborted" | "error") {
            return string_field(Some(payload), "turn_id");
        }
        None
    })
}

fn record_matches_turn(record: &Value, turn_id: Option<&str>) -> bool {
    let Some(turn_id) = turn_id else {
        return true;
    };
    let payload = record.get("payload");
    string_field(Some(record), "turn_id").as_deref() == Some(turn_id)
        || string_field(payload, "turn_id").as_deref() == Some(turn_id)
        || payload
            .and_then(|payload| payload.get("internal_chat_message_metadata_passthrough"))
            .and_then(|metadata| string_field(Some(metadata), "turn_id"))
            .as_deref()
            == Some(turn_id)
        || record
            .get("message")
            .and_then(|message| string_field(Some(message), "turn_id"))
            .as_deref()
            == Some(turn_id)
}

fn message_content_text(payload: Option<&Value>) -> Option<String> {
    message_text_from_value(payload?)
}

fn claude_record_text(record: &Value) -> Option<String> {
    record
        .get("message")
        .and_then(message_text_from_value)
        .or_else(|| message_text_from_value(record))
}

fn message_text_from_value(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => non_empty_string(text),
        Value::Array(items) => {
            let text = items
                .iter()
                .filter_map(message_text_from_value)
                .collect::<Vec<_>>()
                .join(" ");
            non_empty_string(&text)
        }
        Value::Object(object) => {
            if let Some(text) = object.get("text").and_then(Value::as_str) {
                return non_empty_string(text);
            }
            let object_type = object.get("type").and_then(Value::as_str);
            let is_message_object = object.contains_key("role")
                || object_type == Some("message")
                || object_type.is_none();
            if is_message_object && let Some(content) = object.get("content") {
                return message_text_from_value(content);
            }
            None
        }
        _ => None,
    }
}

fn non_empty_string(value: &str) -> Option<String> {
    let text = value.trim();
    if text.trim().is_empty() {
        None
    } else {
        Some(text.to_owned())
    }
}

fn fallback_summary(
    message_arg: Option<&str>,
    input: Option<&Value>,
    raw_stdin: Option<&str>,
) -> HookSummary {
    let request = message_arg
        .filter(|message| !message.trim().is_empty())
        .map(clean_summary_text)
        .or_else(|| string_field(input, "message").map(|message| clean_summary_text(&message)))
        .or_else(|| {
            string_field(input, "notification").map(|message| clean_summary_text(&message))
        });
    let result =
        string_field(input, "last_assistant_message").map(|message| clean_summary_text(&message));
    let request = request.or_else(|| raw_stdin.map(clean_summary_text));
    HookSummary { request, result }
}

fn clean_summary_text(value: &str) -> String {
    truncate(&redact_sensitive_text(&collapse_whitespace(value)), 120)
}

fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn redact_sensitive_text(value: &str) -> String {
    let mut redacted = Vec::new();
    let mut skip_next = 0usize;
    for token in value.split_whitespace() {
        if skip_next > 0 {
            skip_next -= 1;
            continue;
        }
        let lower = token.to_ascii_lowercase();
        if lower == "bearer" {
            redacted.push("Bearer [REDACTED]".to_string());
            skip_next = 1;
        } else if lower.starts_with("sk-") {
            redacted.push("[REDACTED]".to_string());
        } else if is_sensitive_assignment(&lower) {
            redacted.push(redacted_token_label(token));
            skip_next = 1;
        } else if let Some(redacted_token) = redact_chinese_sensitive_token(token, "账号密码") {
            redacted.push(redacted_token);
            skip_next = 2;
        } else if let Some(redacted_token) = redact_chinese_sensitive_token(token, "密码") {
            redacted.push(redacted_token);
            skip_next = 1;
        } else {
            redacted.push(token.to_string());
        }
    }
    redacted.join(" ")
}

fn redact_chinese_sensitive_token(token: &str, keyword: &str) -> Option<String> {
    let index = token.find(keyword)?;
    let prefix = &token[..index];
    Some(format!("{prefix}{keyword} [REDACTED]"))
}

fn is_sensitive_assignment(token: &str) -> bool {
    [
        "authorization",
        "cookie",
        "password",
        "passwd",
        "secret",
        "token",
        "access_token",
        "private_key",
    ]
    .iter()
    .any(|key| {
        token == *key
            || token.starts_with(&format!("{key}:"))
            || token.starts_with(&format!("{key}="))
            || token.starts_with(&format!("{key}："))
    })
}

fn redacted_token_label(token: &str) -> String {
    let label = token
        .split([':', '=', '：'])
        .next()
        .filter(|label| !label.is_empty())
        .unwrap_or("secret");
    format!("{label} [REDACTED]")
}

fn truncate(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let truncated = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated}...")
    } else {
        truncated
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_temp_transcript(content: &str) -> PathBuf {
        let path = env::temp_dir().join(format!(
            "nudgepost-hook-test-{}-{}.jsonl",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        fs::write(&path, content).expect("write temp transcript");
        path
    }

    #[test]
    fn target_requires_exactly_one_destination() {
        assert!(HookTarget::from_route_channel(Some("alerts".into()), None).is_ok());
        assert!(HookTarget::from_route_channel(None, Some("ding".into())).is_ok());
        assert!(HookTarget::from_route_channel(Some("a".into()), Some("b".into())).is_err());
        assert!(HookTarget::from_route_channel(None, None).is_err());
    }

    #[test]
    fn status_message_extracts_safe_fields() {
        let (title, text) = status_message(
            HookAgent::Claude,
            None,
            None,
            r#"{"hook_event_name":"Notification","session_id":"s1","cwd":"C:\\repo","tool_input":{"secret":"nope"},"message":"waiting"}"#,
        );
        assert_eq!(title, "Claude needs attention");
        assert_eq!(text, "请求：waiting\n结果：no final response captured");
        assert!(!text.contains("secret"));
    }

    #[test]
    fn transcript_summary_filters_codex_records_by_turn_id() {
        let transcript = write_temp_transcript(
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"old request"}],"internal_chat_message_metadata_passthrough":{"turn_id":"old"}}}
{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"old result"}],"internal_chat_message_metadata_passthrough":{"turn_id":"old"}}}
{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"new request"}],"internal_chat_message_metadata_passthrough":{"turn_id":"new"}}}
{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"new result"}],"internal_chat_message_metadata_passthrough":{"turn_id":"new"}}}"#,
        );
        let input = json!({
            "transcript_path": transcript.to_string_lossy(),
            "turn_id": "new"
        });

        let summary = transcript_summary(Some(&input)).expect("transcript summary");

        assert_eq!(summary.text(), "请求：new request\n结果：new result");
        fs::remove_file(transcript).ok();
    }

    #[test]
    fn transcript_summary_reads_claude_jsonl_messages() {
        let transcript = write_temp_transcript(
            r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"please check build"}]}}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"build passed"}]}}"#,
        );
        let input = json!({
            "transcript_path": transcript.to_string_lossy()
        });

        let summary = transcript_summary(Some(&input)).expect("transcript summary");

        assert_eq!(
            summary.text(),
            "请求：please check build\n结果：build passed"
        );
        fs::remove_file(transcript).ok();
    }

    #[test]
    fn transcript_summary_prefers_claude_subagent_transcript() {
        let main_transcript = write_temp_transcript(
            r#"{"type":"user","message":{"role":"user","content":"main request"}}
{"type":"assistant","message":{"role":"assistant","content":"main result"}}"#,
        );
        let agent_transcript = write_temp_transcript(
            r#"{"type":"user","message":{"role":"user","content":"agent request"}}
{"type":"assistant","message":{"role":"assistant","content":"agent result"}}"#,
        );
        let input = json!({
            "hook_event_name": "SubagentStop",
            "transcript_path": main_transcript.to_string_lossy(),
            "agent_transcript_path": agent_transcript.to_string_lossy()
        });

        let summary = transcript_summary(Some(&input)).expect("transcript summary");

        assert_eq!(summary.text(), "请求：agent request\n结果：agent result");
        fs::remove_file(main_transcript).ok();
        fs::remove_file(agent_transcript).ok();
    }

    #[test]
    fn status_message_uses_last_assistant_message_when_transcript_has_request_only() {
        let transcript = write_temp_transcript(
            r#"{"type":"user","message":{"role":"user","content":"deploy now"}}"#,
        );
        let stdin = json!({
            "hook_event_name": "Stop",
            "transcript_path": transcript.to_string_lossy(),
            "last_assistant_message": "deployment finished"
        })
        .to_string();

        let (_title, text) = status_message(HookAgent::Claude, None, None, &stdin);

        assert_eq!(text, "请求：deploy now\n结果：deployment finished");
        fs::remove_file(transcript).ok();
    }

    #[test]
    fn status_message_falls_back_when_transcript_is_unreadable() {
        let stdin = json!({
            "hook_event_name": "Stop",
            "transcript_path": "Z:\\missing\\transcript.jsonl",
            "message": "waiting for permission",
            "last_assistant_message": "approval requested"
        })
        .to_string();

        let (_title, text) = status_message(HookAgent::Claude, None, None, &stdin);

        assert_eq!(
            text,
            "请求：waiting for permission\n结果：approval requested"
        );
    }

    #[test]
    fn hook_command_quotes_paths_once() {
        let command = hook_command(
            Path::new(r"C:\Program Files\nudgepost\nudgepost.exe"),
            HookAgent::Codex,
            &HookTarget::Route("alerts".into()),
            Path::new(r"C:\Users\me\nudgepost config.toml"),
        );
        assert!(command.starts_with(r#""C:\Program Files\nudgepost\nudgepost.exe" hook run"#));
        assert!(command.contains(r#"--config "C:\Users\me\nudgepost config.toml""#));
        assert!(!command.contains(r#""\"C:"#));
    }

    #[test]
    fn remove_agent_hooks_removes_only_matching_agent() {
        let mut root = json!({
            "hooks": {
                "Stop": [
                    {"hooks": [
                        {"type": "command", "command": "nudgepost hook run --agent codex --route alerts"},
                        {"type": "command", "command": "other"}
                    ]}
                ]
            }
        });
        assert!(remove_agent_hooks(&mut root, HookAgent::Codex));
        let handlers = root["hooks"]["Stop"][0]["hooks"].as_array().unwrap();
        assert_eq!(handlers.len(), 1);
        assert_eq!(handlers[0]["command"], "other");
    }

    #[test]
    fn remove_agent_hooks_removes_legacy_waypost_handler() {
        let mut root = json!({
            "hooks": {
                "Stop": [
                    {"hooks": [
                        {"type": "command", "command": "waypost hook run --agent codex --route alerts"},
                        {"type": "command", "command": "other"}
                    ]}
                ]
            }
        });
        assert!(remove_agent_hooks(&mut root, HookAgent::Codex));
        let handlers = root["hooks"]["Stop"][0]["hooks"].as_array().unwrap();
        assert_eq!(handlers.len(), 1);
        assert_eq!(handlers[0]["command"], "other");
    }
}
