use std::{
    env, fs,
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, bail};
use clap::{Args as ClapArgs, Parser, Subcommand, ValueEnum};
use serde::Serialize;
use serde_json::Value;

use crate::{
    config::{AppConfig, home_dir},
    hook,
    message::{MessageService, SendChannelInput, SendChannelTextInput, SendOutput, SendRouteInput},
};

#[derive(Debug, Parser)]
#[command(
    name = "nudgepost",
    version,
    about = "Nudgepost local message sender",
    disable_help_subcommand = true
)]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    #[command(about = "Message commands")]
    Message {
        #[command(subcommand)]
        command: MessageCommand,
    },
    #[command(about = "Configuration utilities")]
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    #[command(about = "Install the current executable to ~/.local/bin/")]
    Install(InstallArgs),
    #[command(about = "Register AI agent status hooks")]
    Hook {
        #[command(subcommand)]
        command: HookCommand,
    },
    #[command(about = "Show user-oriented help")]
    Help {
        #[arg(value_enum)]
        topic: Option<HelpTopic>,
    },
}

#[derive(Debug, ClapArgs)]
struct InstallArgs {
    #[arg(long)]
    force: bool,
}

#[derive(Debug, Subcommand)]
enum ConfigCommand {
    #[command(about = "Parse and validate config")]
    Check {
        #[arg(
            long,
            value_name = "PATH",
            help = "Config path (default: ~/.local/config/nudgepost.toml)"
        )]
        config: Option<PathBuf>,
    },
}

#[derive(Debug, ClapArgs)]
struct ConfigArg {
    #[arg(
        long,
        value_name = "PATH",
        help = "Config path (default: ~/.local/config/nudgepost.toml)"
    )]
    config: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
enum MessageCommand {
    #[command(about = "Send title/text to a configured message route")]
    SendRoute {
        route: String,
        #[arg(long)]
        title: String,
        #[arg(long)]
        text: String,
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        config: ConfigArg,
    },
    #[command(about = "Send native JSON payload to a channel")]
    SendChannel {
        channel: String,
        #[arg(long)]
        payload: String,
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        config: ConfigArg,
    },
    #[command(about = "List configured message routes")]
    Routes {
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        config: ConfigArg,
    },
    #[command(about = "List configured message channels")]
    Channels {
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        config: ConfigArg,
    },
}

#[derive(Debug, Clone, ValueEnum)]
enum HelpTopic {
    Config,
    Hook,
}

#[derive(Debug, Subcommand)]
enum HookCommand {
    #[command(about = "Install a Codex or Claude status hook")]
    Install(HookInstallArgs),
    #[command(about = "Uninstall a Codex or Claude status hook")]
    Uninstall(HookUninstallArgs),
    #[command(hide = true)]
    Run(HookRunArgs),
}

#[derive(Debug, ClapArgs)]
struct HookInstallArgs {
    #[arg(value_enum)]
    agent: hook::HookAgent,
    #[arg(long)]
    route: Option<String>,
    #[arg(long)]
    channel: Option<String>,
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,
    #[arg(long)]
    force: bool,
    #[arg(long)]
    dry_run: bool,
}

#[derive(Debug, ClapArgs)]
struct HookUninstallArgs {
    #[arg(value_enum)]
    agent: hook::HookAgent,
    #[arg(long)]
    dry_run: bool,
}

#[derive(Debug, ClapArgs)]
struct HookRunArgs {
    #[arg(long, value_enum)]
    agent: hook::HookAgent,
    #[arg(long)]
    route: Option<String>,
    #[arg(long)]
    channel: Option<String>,
    #[arg(long)]
    event: Option<String>,
    #[arg(long)]
    message: Option<String>,
    #[command(flatten)]
    config: ConfigArg,
}

pub async fn run() -> anyhow::Result<()> {
    let args = Args::parse();
    match args.command {
        Some(Command::Message { command }) => handle_message_command(command).await,
        Some(Command::Config {
            command: ConfigCommand::Check { config },
        }) => check_config(resolve_config_path(config)?),
        Some(Command::Install(install_args)) => install_current_exe(install_args.force),
        Some(Command::Hook { command }) => handle_hook_command(command).await,
        Some(Command::Help { topic }) => {
            match topic {
                Some(HelpTopic::Config) => print_config_help(),
                Some(HelpTopic::Hook) => print_hook_help(),
                None => print_user_help(),
            }
            Ok(())
        }
        None => {
            print_user_help();
            Ok(())
        }
    }
}

async fn handle_message_command(command: MessageCommand) -> anyhow::Result<()> {
    match command {
        MessageCommand::SendRoute {
            route,
            title,
            text,
            json,
            config,
        } => {
            let service = load_message_service(resolve_config_path(config.config)?)?;
            let output = service
                .send_route(SendRouteInput { route, title, text })
                .await?;
            print_message_output(json, &output)?;
            fail_if_message_failed(&output)
        }
        MessageCommand::SendChannel {
            channel,
            payload,
            json,
            config,
        } => {
            let service = load_message_service(resolve_config_path(config.config)?)?;
            let payload: Value = serde_json::from_str(&payload)
                .with_context(|| "failed to parse --payload as JSON")?;
            let output = service
                .send_channel(SendChannelInput { channel, payload })
                .await?;
            print_message_output(json, &output)?;
            fail_if_message_failed(&output)
        }
        MessageCommand::Routes { json, config } => {
            let service = load_message_service(resolve_config_path(config.config)?)?;
            let routes = service.list_routes();
            print_output(json, &routes, || {
                for route in &routes {
                    println!(
                        "{} enabled={} send_when={} channels={}",
                        route.name, route.enabled, route.send_when, route.channel_count
                    );
                }
            })
        }
        MessageCommand::Channels { json, config } => {
            let service = load_message_service(resolve_config_path(config.config)?)?;
            let channels = service.list_channels();
            print_output(json, &channels, || {
                for channel in &channels {
                    println!(
                        "{} kind={} enabled={} send_when={} timeout_seconds={}",
                        channel.name,
                        channel.kind,
                        channel.enabled,
                        channel.send_when,
                        channel.timeout_seconds
                    );
                }
            })
        }
    }
}

async fn handle_hook_command(command: HookCommand) -> anyhow::Result<()> {
    match command {
        HookCommand::Install(args) => {
            let target = hook::HookTarget::from_route_channel(args.route, args.channel)?;
            hook::install(hook::InstallOptions {
                agent: args.agent,
                target,
                config: resolve_config_path(args.config)?,
                force: args.force,
                dry_run: args.dry_run,
            })
        }
        HookCommand::Uninstall(args) => hook::uninstall(args.agent, args.dry_run),
        HookCommand::Run(args) => {
            let target = hook::HookTarget::from_route_channel(args.route, args.channel)?;
            let mut stdin = String::new();
            std::io::stdin()
                .read_to_string(&mut stdin)
                .context("failed to read hook stdin")?;
            let (title, text) = hook::status_message(
                args.agent,
                args.event.as_deref(),
                args.message.as_deref(),
                &stdin,
            );
            let service = load_message_service(resolve_config_path(args.config.config)?)?;
            let output = match target {
                hook::HookTarget::Route(route) => {
                    service
                        .send_hook_route(SendRouteInput { route, title, text })
                        .await?
                }
                hook::HookTarget::Channel(channel) => {
                    service
                        .send_hook_channel(SendChannelTextInput {
                            channel,
                            title,
                            text,
                        })
                        .await?
                }
            };
            print_message_output(false, &output)?;
            if output.status == "failed" {
                println!("warning: hook delivery failed");
            }
            Ok(())
        }
    }
}

fn load_message_service(config_path: PathBuf) -> anyhow::Result<MessageService> {
    let config = Arc::new(AppConfig::load(config_path)?);
    Ok(MessageService::new(config))
}

fn check_config(config_path: PathBuf) -> anyhow::Result<()> {
    let config = AppConfig::load(&config_path)?;
    println!("config ok: {}", config_path.display());
    println!("log_enabled: {}", config.log.enabled);
    println!("log_path: {}", config.log.path);
    println!("channels: {}", config.channels.len());
    println!("routes: {}", config.routes.len());
    Ok(())
}

fn print_message_output(json: bool, output: &SendOutput) -> anyhow::Result<()> {
    print_output(json, output, || {
        println!("message_id: {}", output.message_id);
        println!("entry_type: {}", output.entry_type);
        println!("target: {}", output.target);
        println!("status: {}", output.status);
        println!("deliveries: {}", output.deliveries.len());
        for delivery in &output.deliveries {
            println!(
                "- {} status={} attempts={} http_status={:?} error={:?}",
                delivery.channel,
                delivery.status,
                delivery.attempts,
                delivery.http_status,
                delivery.error
            );
        }
    })
}

fn fail_if_message_failed(output: &SendOutput) -> anyhow::Result<()> {
    if output.status == "failed" {
        bail!("message send failed: {}", output.message_id);
    }
    Ok(())
}

fn print_output<T: Serialize>(json: bool, value: &T, human: impl FnOnce()) -> anyhow::Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(value)?);
    } else {
        human();
    }
    Ok(())
}

fn resolve_config_path(config: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    match config {
        Some(config) => Ok(config),
        None => default_config_path(),
    }
}

fn default_config_path() -> anyhow::Result<PathBuf> {
    Ok(home_dir()
        .context("HOME or USERPROFILE is not set")?
        .join(".local")
        .join("config")
        .join("nudgepost.toml"))
}

fn install_current_exe(force: bool) -> anyhow::Result<()> {
    let current_exe = env::current_exe().context("failed to locate current executable")?;
    let target_dir = home_dir()
        .context("HOME or USERPROFILE is not set")?
        .join(".local")
        .join("bin");
    fs::create_dir_all(&target_dir)
        .with_context(|| format!("failed to create {}", target_dir.display()))?;
    let target = install_target_path(&target_dir);
    if target.exists() && !force {
        bail!(
            "{} already exists; rerun with --force to overwrite",
            target.display()
        );
    }
    fs::copy(&current_exe, &target).with_context(|| {
        format!(
            "failed to copy {} to {}",
            current_exe.display(),
            target.display()
        )
    })?;
    set_executable_permission(&target)?;
    println!("installed: {}", target.display());
    Ok(())
}

fn install_target_path(target_dir: &Path) -> PathBuf {
    target_dir.join(target_exe_name())
}

fn target_exe_name() -> &'static str {
    if cfg!(windows) {
        "nudgepost.exe"
    } else {
        "nudgepost"
    }
}

#[cfg(unix)]
fn set_executable_permission(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(not(unix))]
fn set_executable_permission(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

fn print_user_help() {
    println!("{}", USER_HELP.trim());
}

fn print_config_help() {
    println!("{}", CONFIG_HELP.trim());
}

fn print_hook_help() {
    println!("{}", HOOK_HELP.trim());
}

const USER_HELP: &str = r#"
Nudgepost

Commands:
  nudgepost message send-route alerts --title test --text hello
      Send a title/text message through a configured route immediately.

  nudgepost message send-channel custom_main --payload '{"msgtype":"text"}'
      Send a channel-native JSON payload immediately.

  nudgepost message routes --json
  nudgepost message channels --json
      List configured routes or channels.

  nudgepost config check
      Parse and validate ~/.local/config/nudgepost.toml.

  nudgepost hook install codex --route alerts
  nudgepost hook install claude --channel feishu_main
      Send AI-agent status events directly through Nudgepost.

Notes:
  Nudgepost is small and lightweight. Optional message logs are controlled by [log].
"#;

const HOOK_HELP: &str = r#"
Nudgepost AI agent hooks

Commands:
  nudgepost hook install codex --route alerts
  nudgepost hook install codex --channel ding_main
  nudgepost hook install claude --route alerts
  nudgepost hook uninstall codex

Destinations:
  Exactly one of --route or --channel is required.
  --route uses configured route fan-out.
  --channel sends a standard title/text status message to that channel.

Files:
  Codex:  ~/.codex/hooks.json
  Claude: ~/.claude/settings.json

Events:
  Codex:  Stop, SubagentStop
  Claude: Notification, Stop, SubagentStop

Notes:
  Hook run sends status messages directly through Nudgepost.
  Enable [log] when you want one JSONL message record per hook/send.
"#;

const CONFIG_HELP: &str = r#"
nudgepost config.toml

Default path:
  ~/.local/config/nudgepost.toml

[log]
  enabled = false
  path = "~/.local/state/nudgepost/messages.jsonl"

[rate_limit]
  enabled = true
  default_per_second = 20.0
  default_burst = 20

[channels.<name>]
  Dingtalk:
    kind = "dingtalk"
    enabled = true
    send_when = "always"
    webhook = "https://oapi.dingtalk.com/robot/send?access_token=..."
    secret = ""
    timeout_seconds = 10
    rate_limit = { per_second = 1.0, burst = 1 }

  Feishu:
    kind = "feishu"
    enabled = true
    send_when = "always"
    webhook = "https://open.feishu.cn/open-apis/bot/v2/hook/..."
    secret = ""
    timeout_seconds = 10

  Feishu custom app:
    kind = "feishu_app"
    enabled = true
    send_when = "always"
    app_id = "cli_..."
    app_secret = "..."
    receive_id_type = "open_id"
    receive_id = "ou_..."
    timeout_seconds = 10

  Custom URL:
    kind = "custom"
    enabled = true
    send_when = "always"
    url = "https://example.com/webhook"
    timeout_seconds = 10

    [channels.<name>.headers]
    X-Source = "Nudgepost"

[routes.<route>]
  enabled = true
  send_when = "always"
  channels = ["ding_main", "feishu_main", "custom_main"]

send_when supports "always" or "screen_locked". If lock state is unknown,
Nudgepost sends the message and logs a warning.
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_message_send_route_command_without_queue_flags() {
        let args = Args::try_parse_from([
            "nudgepost",
            "message",
            "send-route",
            "alerts",
            "--title",
            "test",
            "--text",
            "hello",
            "--json",
        ])
        .unwrap();
        assert!(matches!(
            args.command,
            Some(Command::Message {
                command: MessageCommand::SendRoute { json: true, .. }
            })
        ));
    }

    #[test]
    fn rejects_removed_enqueue_flag() {
        let args = Args::try_parse_from([
            "nudgepost",
            "message",
            "send-route",
            "alerts",
            "--title",
            "test",
            "--text",
            "hello",
            "--enqueue-only",
        ]);
        assert!(args.is_err());
    }

    #[test]
    fn rejects_removed_status_command() {
        let args = Args::try_parse_from(["nudgepost", "message", "status", "mid"]);
        assert!(args.is_err());
    }

    #[test]
    fn parses_hook_install_command() {
        let args = Args::try_parse_from([
            "nudgepost",
            "hook",
            "install",
            "codex",
            "--route",
            "alerts",
            "--dry-run",
        ])
        .unwrap();
        assert!(matches!(
            args.command,
            Some(Command::Hook {
                command: HookCommand::Install(HookInstallArgs { dry_run: true, .. })
            })
        ));
    }

    #[test]
    fn default_config_path_uses_nudgepost_file() {
        let path = default_config_path().unwrap();
        assert!(path.ends_with(Path::new(".local").join("config").join("nudgepost.toml")));
    }

    #[test]
    fn install_target_uses_platform_exe_name() {
        let target = install_target_path(Path::new("/home/me/.local/bin"));
        if cfg!(windows) {
            assert!(target.ends_with("nudgepost.exe"));
        } else {
            assert!(target.ends_with("nudgepost"));
        }
    }
}
