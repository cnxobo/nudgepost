# Nudgepost

English | [简体中文](README.zh-CN.md)

Nudgepost is a small, lightweight local CLI for direct message delivery to DingTalk, Feishu, Feishu app messages, and custom webhooks.

## Features

- Send title/text messages through configured routes.
- Send channel-native JSON payloads directly to one channel.
- Fan out one route to multiple channels.
- Support DingTalk, Feishu webhook, Feishu app, and custom webhook channels.
- Support Codex and Claude status hooks.
- Support `send_when = "screen_locked"` for lock-screen-only notifications.
- Optional JSONL message log, disabled by default.

## Install

Build and install the current release binary to `~/.local/bin/nudgepost.exe` on Windows:

```powershell
rtk cargo build --release
.\target\release\nudgepost.exe install --force
```

The executable can also be run directly from `target\release\nudgepost.exe`.

## Configure

Default config path:

```text
~/.local/config/nudgepost.toml
```

Create a config from the example:

```powershell
New-Item -ItemType Directory -Force -Path "$HOME\.local\config"
Copy-Item -LiteralPath .\config.example.toml -Destination "$HOME\.local\config\nudgepost.toml"
nudgepost config check
```

Routes fan out to channels:

```toml
[channels.feishu_main]
kind = "feishu"
webhook = "https://open.feishu.cn/open-apis/bot/v2/hook/CHANGE_ME"
timeout_seconds = 10

[routes.alerts]
send_when = "screen_locked"
channels = ["feishu_main"]
```

`send_when` supports `always` and `screen_locked`. If the lock state cannot be determined, Nudgepost sends the message and logs a warning.

## Send Messages

```powershell
nudgepost message send-route alerts --title test --text hello
nudgepost message send-channel custom_main --payload '{"message":"hello"}'
nudgepost message routes --json
nudgepost message channels --json
```

The process exits non-zero when any delivery fails. A `screen_locked` condition that is not met produces a `skipped` result and does not send.

## Hooks

Install Codex or Claude hooks:

```powershell
nudgepost hook install codex --route alerts --dry-run
nudgepost hook install codex --route alerts --force
nudgepost hook install claude --channel feishu_main --force
nudgepost hook uninstall codex
```

Hook runs send status messages directly through Nudgepost. Hook files:

- Codex: `~/.codex/hooks.json`
- Claude: `~/.claude/settings.json`

The agent argument selects the hook file, event set, title label, and transcript
summary adapter. Hook runs read the transcript path from hook stdin when present
and fall back to safe stdin fields if the transcript cannot be read or parsed.

## Optional Message Log

Message logging is disabled by default:

```toml
[log]
enabled = false
path = "~/.local/state/nudgepost/messages.jsonl"
```

When enabled, Nudgepost appends one JSON object per line after a send. The log records result metadata and delivery outcomes; it does not record secrets, signed webhook URLs, full headers, Authorization, cookies, or Feishu tenant tokens.

## Verification

```powershell
rtk cargo test
rtk cargo build --release
rtk cargo run --bin nudgepost -- config check --config .\config.example.toml
```
