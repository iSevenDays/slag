use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::timeout;

use super::Smith;
use crate::config::SmithConfig;
use crate::error::SlagError;
use crate::events;

const DEFAULT_PROMPT_REPEAT_COUNT: usize = 2;
const DEFAULT_PROMPT_REPEAT_MAX_CHARS: usize = 40_000;
const DEFAULT_SMITH_TIMEOUT_SECS: u64 = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PromptRepeatMode {
    Off,
    NonPlan,
    Always,
}

impl PromptRepeatMode {
    fn from_env() -> Self {
        match std::env::var("SLAG_PROMPT_REPEAT_MODE")
            .unwrap_or_else(|_| "non-plan".to_string())
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "off" | "never" | "0" => Self::Off,
            "always" | "all" | "on" => Self::Always,
            _ => Self::NonPlan,
        }
    }
}

/// Claude CLI smith that spawns `claude -p` as a subprocess.
pub struct ClaudeSmith {
    command: String,
}

impl ClaudeSmith {
    pub fn new(command: String) -> Self {
        Self { command }
    }

    pub fn from_config(config: &SmithConfig, skill: &str, grade: u8) -> Self {
        Self {
            command: config.select(skill, grade).to_string(),
        }
    }

    pub fn plan(config: &SmithConfig) -> Self {
        Self {
            command: config.plan.clone(),
        }
    }

    pub fn base(config: &SmithConfig) -> Self {
        Self {
            command: config.base.clone(),
        }
    }

    async fn invoke_impl(&self, prompt: &str, cwd: Option<&Path>) -> Result<String, SlagError> {
        let repeated_prompt = maybe_repeat_prompt(prompt, &self.command);
        let parts: Vec<&str> = shell_words(&self.command);
        if parts.is_empty() {
            return Err(SlagError::SmithFailed("empty smith command".into()));
        }
        let timeout_secs = smith_timeout_secs();

        let program = parts[0];
        let args = &parts[1..];

        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        if let Some(dir) = cwd {
            command.current_dir(dir);
        }
        let mut child = command
            .spawn()
            .map_err(|e| SlagError::SmithFailed(format!("failed to spawn {program}: {e}")))?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(repeated_prompt.as_bytes())
                .await
                .map_err(|e| SlagError::SmithFailed(format!("stdin write failed: {e}")))?;
        }

        let output =
            match timeout(Duration::from_secs(timeout_secs), child.wait_with_output()).await {
                Ok(Ok(output)) => output,
                Ok(Err(e)) => return Err(SlagError::SmithFailed(format!("wait failed: {e}"))),
                Err(_) => {
                    events::emit_warn(
                        "smith.invoke.timeout",
                        "smith invocation timed out",
                        serde_json::json!({
                            "timeout_secs": timeout_secs,
                            "command": truncate_for_log(&self.command, 160),
                        }),
                    );
                    return Err(SlagError::SmithFailed(format!(
                        "timeout after {timeout_secs}s"
                    )));
                }
            };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let detail = if stderr.trim().is_empty() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                truncate_for_log(stdout.trim(), 400).to_string()
            } else {
                stderr.trim().to_string()
            };
            return Err(SlagError::SmithFailed(format!(
                "exit {}: {}",
                output.status.code().unwrap_or(-1),
                detail
            )));
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }
}

fn smith_timeout_secs() -> u64 {
    std::env::var("SLAG_SMITH_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_SMITH_TIMEOUT_SECS)
}

fn truncate_for_log(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let mut chars = text.chars();
    let truncated: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{truncated}...")
    } else {
        truncated
    }
}

fn maybe_repeat_prompt(prompt: &str, command: &str) -> String {
    let mode = PromptRepeatMode::from_env();
    let count = std::env::var("SLAG_PROMPT_REPEAT_COUNT")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_PROMPT_REPEAT_COUNT)
        .clamp(1, 4);
    let max_chars = std::env::var("SLAG_PROMPT_REPEAT_MAX_CHARS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_PROMPT_REPEAT_MAX_CHARS);

    if count <= 1 {
        return prompt.to_string();
    }
    if prompt.len() > max_chars {
        // Partial repetition: repeat just the tail (instructions/rules)
        // per Leviathan et al. 2025 arXiv:2512.14982 §5 item 5
        events::emit_debug(
            "smith.prompt_repeat.partial_tail",
            "applied partial tail repetition for long prompt",
            serde_json::json!({
                "prompt_chars": prompt.len(),
                "max_chars": max_chars,
                "count": count
            }),
        );
        if !should_repeat(mode, command) {
            return prompt.to_string();
        }
        return repeat_tail(prompt, count);
    }
    if !should_repeat(mode, command) {
        return prompt.to_string();
    }

    events::emit_debug(
        "smith.prompt_repeat.applied",
        "applied prompt repetition",
        serde_json::json!({
            "mode": format!("{mode:?}"),
            "count": count,
            "prompt_chars": prompt.len(),
            "command_has_plan": command.contains("--permission-mode plan")
        }),
    );

    repeat_prompt(prompt, count)
}

fn should_repeat(mode: PromptRepeatMode, command: &str) -> bool {
    match mode {
        PromptRepeatMode::Off => false,
        PromptRepeatMode::Always => true,
        PromptRepeatMode::NonPlan => !command.contains("--permission-mode plan"),
    }
}

fn repeat_tail(prompt: &str, count: usize) -> String {
    const TAIL_CHARS: usize = 2000;
    let tail_start = prompt.len().saturating_sub(TAIL_CHARS);
    // Find a clean line break near the cut point
    let tail_start = prompt[tail_start..]
        .find('\n')
        .map(|p| tail_start + p + 1)
        .unwrap_or(tail_start);
    let tail = &prompt[tail_start..];
    let mut out = String::with_capacity(prompt.len() + tail.len() * (count - 1) + 10);
    out.push_str(prompt);
    for _ in 1..count {
        out.push_str("\n\n");
        out.push_str(tail);
    }
    out
}

fn repeat_prompt(prompt: &str, count: usize) -> String {
    if count <= 1 {
        return prompt.to_string();
    }
    let mut out = String::with_capacity(prompt.len() * count + 2 * (count - 1));
    for idx in 0..count {
        if idx > 0 {
            out.push('\n');
            out.push('\n');
        }
        out.push_str(prompt);
    }
    out
}

impl Smith for ClaudeSmith {
    fn invoke(
        &self,
        prompt: &str,
    ) -> Pin<Box<dyn Future<Output = Result<String, SlagError>> + Send + '_>> {
        let prompt = prompt.to_string();
        Box::pin(async move { self.invoke_impl(&prompt, None).await })
    }

    fn invoke_in_dir(
        &self,
        prompt: &str,
        dir: &Path,
    ) -> Pin<Box<dyn Future<Output = Result<String, SlagError>> + Send + '_>> {
        let prompt = prompt.to_string();
        let dir = PathBuf::from(dir);
        Box::pin(async move { self.invoke_impl(&prompt, Some(&dir)).await })
    }
}

/// Simple shell word splitting (handles single/double quotes).
fn shell_words(s: &str) -> Vec<&str> {
    let mut words = Vec::new();
    let mut i = 0;
    let bytes = s.as_bytes();
    let len = bytes.len();

    while i < len {
        while i < len && bytes[i] == b' ' {
            i += 1;
        }
        if i >= len {
            break;
        }

        if bytes[i] == b'\'' {
            i += 1;
            let start = i;
            while i < len && bytes[i] != b'\'' {
                i += 1;
            }
            words.push(&s[start..i]);
            if i < len {
                i += 1;
            }
        } else if bytes[i] == b'"' {
            i += 1;
            let start = i;
            while i < len && bytes[i] != b'"' {
                i += 1;
            }
            words.push(&s[start..i]);
            if i < len {
                i += 1;
            }
        } else {
            let start = i;
            while i < len && bytes[i] != b' ' {
                i += 1;
            }
            words.push(&s[start..i]);
        }
    }

    words
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_words_basic() {
        let words = shell_words("claude --dangerously-skip-permissions -p");
        assert_eq!(
            words,
            vec!["claude", "--dangerously-skip-permissions", "-p"]
        );
    }

    #[test]
    fn shell_words_quoted() {
        let words = shell_words("claude -p --allowedTools 'Bash Edit Read'");
        assert_eq!(
            words,
            vec!["claude", "-p", "--allowedTools", "Bash Edit Read"]
        );
    }

    #[test]
    fn shell_words_double_quoted() {
        let words = shell_words(r#"claude -p --allowedTools "Bash Edit Read""#);
        assert_eq!(
            words,
            vec!["claude", "-p", "--allowedTools", "Bash Edit Read"]
        );
    }

    #[test]
    fn repeat_prompt_duplicates_with_separator() {
        let repeated = repeat_prompt("abc", 2);
        assert_eq!(repeated, "abc\n\nabc");
    }

    #[test]
    fn non_plan_mode_detects_plan_flag() {
        assert!(!should_repeat(
            PromptRepeatMode::NonPlan,
            "claude -p --permission-mode plan"
        ));
        assert!(should_repeat(
            PromptRepeatMode::NonPlan,
            "claude -p --dangerously-skip-permissions"
        ));
    }
}
