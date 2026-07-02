# Nudgepost

[English](README.md) | 简体中文

Nudgepost 是一个小而轻量的本地 CLI，用来把消息直接发送到钉钉、飞书 webhook、飞书应用消息或自定义 webhook。

## 功能

- 通过配置好的 route 发送标题和正文。
- 向单个 channel 发送原生 JSON payload。
- 一个 route 可以 fan-out 到多个 channel。
- 支持钉钉、飞书 webhook、飞书应用消息和自定义 webhook。
- 支持 Codex 和 Claude 状态 hook。
- 支持 `send_when = "screen_locked"`，只在锁屏时发送通知。
- 可选 JSONL 消息日志，默认关闭。

## 安装

在 Windows 上构建并安装 release 二进制到 `~/.local/bin/nudgepost.exe`：

```powershell
rtk cargo build --release
.\target\release\nudgepost.exe install --force
```

也可以直接运行 `target\release\nudgepost.exe`。

## 配置

默认配置路径：

```text
~/.local/config/nudgepost.toml
```

从示例配置创建：

```powershell
New-Item -ItemType Directory -Force -Path "$HOME\.local\config"
Copy-Item -LiteralPath .\config.example.toml -Destination "$HOME\.local\config\nudgepost.toml"
nudgepost config check
```

route 会把消息发送到它引用的 channel：

```toml
[channels.feishu_main]
kind = "feishu"
webhook = "https://open.feishu.cn/open-apis/bot/v2/hook/CHANGE_ME"
timeout_seconds = 10

[routes.alerts]
send_when = "screen_locked"
channels = ["feishu_main"]
```

`send_when` 支持 `always` 和 `screen_locked`。如果无法判断锁屏状态，Nudgepost 会继续发送并记录 warning。

## 发送消息

```powershell
nudgepost message send-route alerts --title test --text hello
nudgepost message send-channel custom_main --payload '{"message":"hello"}'
nudgepost message routes --json
nudgepost message channels --json
```

任意 delivery 失败时进程会返回非零退出码。`screen_locked` 条件不满足时结果是 `skipped`，不会发送消息。

## Hook

安装 Codex 或 Claude hook：

```powershell
nudgepost hook install codex --route alerts --dry-run
nudgepost hook install codex --route alerts --force
nudgepost hook install claude --channel feishu_main --force
nudgepost hook uninstall codex
```

hook run 会通过 Nudgepost 直接发送状态消息。hook 文件位置：

- Codex：`~/.codex/hooks.json`
- Claude：`~/.claude/settings.json`

agent 参数用于选择 hook 文件、事件集、标题标签和 transcript 摘要适配器。hook run
会优先从 hook stdin 里的 transcript 路径读取内容；如果 transcript 不可读或无法解析，
则回退到 stdin 中的安全字段。

## 可选消息日志

消息日志默认关闭：

```toml
[log]
enabled = false
path = "~/.local/state/nudgepost/messages.jsonl"
```

启用后，Nudgepost 每次发送后追加一行 JSON。日志只记录结果元数据和 delivery 结果；不会记录 secret、签名后的 webhook URL、完整 headers、Authorization、cookie 或飞书 tenant token。

## 验证

```powershell
rtk cargo test
rtk cargo build --release
rtk cargo run --bin nudgepost -- config check --config .\config.example.toml
```
