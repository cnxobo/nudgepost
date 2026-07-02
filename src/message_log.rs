use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use fs2::FileExt;

use crate::{config::LogConfig, error::AppResult, message::SendOutput};

pub fn append_message_log(config: &LogConfig, output: &SendOutput) -> AppResult<()> {
    if !config.enabled {
        return Ok(());
    }
    let path = PathBuf::from(&config.path);
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    append_json_line(&path, output)
}

fn append_json_line(path: &Path, output: &SendOutput) -> AppResult<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(path)?;
    file.lock_exclusive()?;
    let result = (|| -> AppResult<()> {
        serde_json::to_writer(&mut file, output)?;
        file.write_all(b"\n")?;
        file.flush()?;
        Ok(())
    })();
    let unlock_result = file.unlock();
    result?;
    unlock_result?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;
    use crate::message::DeliveryResult;

    #[test]
    fn disabled_log_does_not_create_file() {
        let path =
            std::env::temp_dir().join(format!("nudgepost-disabled-{}.jsonl", uuid::Uuid::new_v4()));
        let output = sample_output();
        append_message_log(
            &LogConfig {
                enabled: false,
                path: path.to_string_lossy().into_owned(),
            },
            &output,
        )
        .unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn enabled_log_writes_one_json_line_without_secret_markers() {
        let path =
            std::env::temp_dir().join(format!("nudgepost-enabled-{}.jsonl", uuid::Uuid::new_v4()));
        let output = sample_output();
        append_message_log(
            &LogConfig {
                enabled: true,
                path: path.to_string_lossy().into_owned(),
            },
            &output,
        )
        .unwrap();
        let content = std::fs::read_to_string(path).unwrap();
        let lines = content.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 1);
        let value: Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(value["target"], "alerts");
        assert!(!content.contains("secret"));
        assert!(!content.contains("Authorization"));
        assert!(!content.contains("access_token"));
    }

    fn sample_output() -> SendOutput {
        SendOutput {
            timestamp: chrono::Utc::now(),
            message_id: "mid".into(),
            entry_type: "route".into(),
            target: "alerts".into(),
            title: Some("title".into()),
            text: Some("text".into()),
            status: "succeeded".into(),
            duration_ms: 1,
            deliveries: vec![DeliveryResult {
                channel: "custom_main".into(),
                status: "succeeded".into(),
                http_status: Some(200),
                error: None,
                attempts: 1,
            }],
        }
    }
}
