use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Output;
use std::time::Duration;

use serde::Deserialize;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::timeout;

use super::Smith;
use crate::config::{SmithCapabilities, SmithConfig, PromptRepeatMode};
use crate::error::SlagError;
use crate::events;

const DEFAULT_PROMPT_REPEAT_COUNT: usize = 2;
const DEFAULT_PROMPT_REPEAT_MAX_CHARS: usize = 40_000;
const DEFAULT_SMITH_TIMEOUT_SECS: u64 = 300;
const CLAUDE_AUTH_STATUS_TIMEOUT_SECS: u64 = 10;

/// Subprocess-backed smith adapter used for Claude-compatible and stdin-driven CLIs.
pub struct ClaudeSmith {
    command: String,
    capabilities: SmithCapabilities,
}

struct SmithSubprocessResult {
    output: Output,
    stdin_error: Option<String>,
}

impl ClaudeSmith {
    pub fn new(command: String) -> Self {
        Self {
            command,
            capabilities: SmithCapabilities::claude(),
        }
    }

    pub fn from_config(config: &SmithConfig, skill: &str, grade: u8) -> Self {
        Self {
            command: config.select(skill, grade).to_string(),
            capabilities: SmithCapabilities::claude(),
        }
    }

    pub fn plan(config: &SmithConfig) -> Self {
        Self {
            command: config.plan.clone(),
            capabilities: SmithCapabilities::claude(),
        }
    }

    pub fn base(config: &SmithConfig) -> Self {
        Self {
            command: config.base.clone(),
            capabilities: SmithCapabilities::claude(),
        }
    }

    async fn invoke_impl(&self, prompt: &str, cwd: Option<&Path>) -> Result<String, SlagError> {
        let is_plan_mode = self.command.contains("--permission-mode plan");
        let repeated_prompt = maybe_repeat_prompt(prompt, &self.capabilities, is_plan_mode);
        let parts: Vec<&str> = shell_words(&self.command);
        if parts.is_empty() {
            return Err(SlagError::SmithFailed("empty smith command".into()));
        }
        let timeout_secs = smith_timeout_secs();

        let program = parts[0];
        let args = &parts[1..];

        let output =
            run_smith_subprocess(program, args, &repeated_prompt, cwd, timeout_secs, false).await?;

        if output.output.status.success() {
            if let Some(stdin_error) = &output.stdin_error {
                return Err(SlagError::SmithFailed(format!(
                    "stdin write failed before smith completed: {stdin_error}"
                )));
            }
            return Ok(String::from_utf8_lossy(&output.output.stdout).to_string());
        }

        let detail = smith_failure_detail(&output);
        if should_retry_with_claude_subscription(program, &self.command, &detail)
            && claude_subscription_auth_available(program, cwd).await
        {
            events::emit_warn(
                "smith.invoke.claude.subscription_fallback",
                "retrying Claude invocation without ANTHROPIC_API_KEY after API-key failure",
                serde_json::json!({
                    "command": truncate_for_log(&self.command, 160),
                    "reason": truncate_for_log(&detail, 200),
                }),
            );

            let retry_output =
                run_smith_subprocess(program, args, &repeated_prompt, cwd, timeout_secs, true)
                    .await?;
            if retry_output.output.status.success() {
                if let Some(stdin_error) = &retry_output.stdin_error {
                    return Err(SlagError::SmithFailed(format!(
                        "Claude subscription fallback succeeded but stdin write failed: {stdin_error}"
                    )));
                }
                events::emit_info(
                    "smith.invoke.claude.subscription_fallback.success",
                    "Claude invocation succeeded after removing ANTHROPIC_API_KEY",
                    serde_json::json!({
                        "command": truncate_for_log(&self.command, 160),
                    }),
                );
                return Ok(String::from_utf8_lossy(&retry_output.output.stdout).to_string());
            }

            let retry_detail = smith_failure_detail(&retry_output);
            let retry_hint = smith_failure_hint(&retry_detail);
            return Err(SlagError::SmithFailed(format!(
                "exit {}: {} (subscription fallback removed ANTHROPIC_API_KEY, but retry exit {}: {}{})",
                output.output.status.code().unwrap_or(-1),
                detail,
                retry_output.output.status.code().unwrap_or(-1),
                retry_detail,
                retry_hint
            )));
        }

        Err(SlagError::SmithFailed(format!(
            "exit {}: {}{}",
            output.output.status.code().unwrap_or(-1),
            detail,
            smith_failure_hint(&detail)
        )))
    }
}

async fn run_smith_subprocess(
    program: &str,
    args: &[&str],
    prompt: &str,
    cwd: Option<&Path>,
    timeout_secs: u64,
    remove_anthropic_api_key: bool,
) -> Result<SmithSubprocessResult, SlagError> {
    let mut command = Command::new(program);
    command
        .args(args)
        .env_remove("CLAUDECODE")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    if remove_anthropic_api_key {
        command.env_remove("ANTHROPIC_API_KEY");
    }
    if let Some(dir) = cwd {
        command.current_dir(dir);
    }
    let mut child = command
        .spawn()
        .map_err(|e| SlagError::SmithFailed(format!("failed to spawn {program}: {e}")))?;

    let stdin_error = if let Some(mut stdin) = child.stdin.take() {
        match stdin.write_all(prompt.as_bytes()).await {
            Ok(()) => None,
            Err(e) => Some(e.to_string()),
        }
    } else {
        None
    };

    match timeout(Duration::from_secs(timeout_secs), child.wait_with_output()).await {
        Ok(Ok(output)) => Ok(SmithSubprocessResult {
            output,
            stdin_error,
        }),
        Ok(Err(e)) => Err(SlagError::SmithFailed(format!("wait failed: {e}"))),
        Err(_) => {
            events::emit_warn(
                "smith.invoke.timeout",
                "smith invocation timed out",
                serde_json::json!({
                    "timeout_secs": timeout_secs,
                    "command": truncate_for_log(program, 160),
                    "anthropic_api_key_removed": remove_anthropic_api_key,
                }),
            );
            Err(SlagError::SmithFailed(format!(
                "timeout after {timeout_secs}s"
            )))
        }
    }
}

fn smith_failure_detail(result: &SmithSubprocessResult) -> String {
    let stderr = String::from_utf8_lossy(&result.output.stderr);
    let detail = if stderr.trim().is_empty() {
        let stdout = String::from_utf8_lossy(&result.output.stdout);
        truncate_for_log(stdout.trim(), 400).to_string()
    } else {
        stderr.trim().to_string()
    };

    if let Some(stdin_error) = &result.stdin_error {
        if detail.is_empty() {
            format!("stdin write failed: {stdin_error}")
        } else {
            format!("{detail} (stdin write failed: {stdin_error})")
        }
    } else {
        detail
    }
}

fn smith_failure_hint(detail: &str) -> &'static str {
    let lower = detail.to_ascii_lowercase();
    if lower.contains("invalid api key") || lower.contains("api key") {
        " (try: claude auth login)"
    } else if lower.contains("cannot be launched inside") {
        " (slag already strips CLAUDECODE; check for wrapper scripts)"
    } else {
        ""
    }
}

fn should_retry_with_claude_subscription(program: &str, command: &str, detail: &str) -> bool {
    anthropic_api_key_present()
        && looks_like_claude_command(program, command)
        && is_api_key_auth_failure(detail)
}

fn anthropic_api_key_present() -> bool {
    std::env::var_os("ANTHROPIC_API_KEY").is_some()
}

fn looks_like_claude_command(program: &str, command: &str) -> bool {
    let program_lower = program.to_ascii_lowercase();
    let command_lower = command.to_ascii_lowercase();
    program_lower.contains("claude")
        || (command_lower.contains("claude") && !command_lower.contains("codex"))
}

fn is_api_key_auth_failure(detail: &str) -> bool {
    let lower = detail.to_ascii_lowercase();
    lower.contains("invalid api key")
        || lower.contains("api key")
        || lower.contains("anthropic_api_key")
        || lower.contains("credit balance")
        || lower.contains("insufficient credits")
        || lower.contains("billing")
}

async fn claude_subscription_auth_available(program: &str, cwd: Option<&Path>) -> bool {
    let mut command = Command::new(program);
    command
        .arg("auth")
        .arg("status")
        .env_remove("ANTHROPIC_API_KEY")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    if let Some(dir) = cwd {
        command.current_dir(dir);
    }

    let output = match timeout(
        Duration::from_secs(CLAUDE_AUTH_STATUS_TIMEOUT_SECS),
        command.output(),
    )
    .await
    {
        Ok(Ok(output)) => output,
        _ => return false,
    };

    if !output.status.success() {
        return false;
    }

    let mut text = String::from_utf8_lossy(&output.stdout).to_string();
    if !output.stderr.is_empty() {
        text.push('\n');
        text.push_str(&String::from_utf8_lossy(&output.stderr));
    }
    auth_status_indicates_subscription(&text)
}

#[derive(Debug, Deserialize)]
struct ClaudeAuthStatus {
    #[serde(rename = "loggedIn")]
    logged_in: Option<bool>,
    #[serde(rename = "authMethod")]
    auth_method: Option<String>,
}

fn auth_status_indicates_subscription(raw: &str) -> bool {
    if let Ok(status) = serde_json::from_str::<ClaudeAuthStatus>(raw.trim()) {
        if status.logged_in == Some(true) {
            return status
                .auth_method
                .as_deref()
                .map(|method| !method.eq_ignore_ascii_case("api_key"))
                .unwrap_or(true);
        }
        return false;
    }

    let compact = raw
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    compact.contains("\"loggedin\":true") && !compact.contains("api_key")
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

fn maybe_repeat_prompt(
    prompt: &str,
    capabilities: &SmithCapabilities,
    is_plan_mode: bool,
) -> String {
    // SLAG_PROMPT_REPEAT_MODE env var overrides the capability profile's mode.
    let mode = std::env::var("SLAG_PROMPT_REPEAT_MODE")
        .ok()
        .map(|v| match v.trim().to_ascii_lowercase().as_str() {
            "off" | "never" | "0" => PromptRepeatMode::Off,
            "always" | "all" | "on" => PromptRepeatMode::Always,
            _ => PromptRepeatMode::NonPlan,
        })
        .unwrap_or(capabilities.prompt_repeat_mode);

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
        if !should_repeat(mode, is_plan_mode) {
            return prompt.to_string();
        }
        return repeat_tail(prompt, count);
    }
    if !should_repeat(mode, is_plan_mode) {
        return prompt.to_string();
    }

    events::emit_debug(
        "smith.prompt_repeat.applied",
        "applied prompt repetition",
        serde_json::json!({
            "mode": format!("{mode:?}"),
            "count": count,
            "prompt_chars": prompt.len(),
            "is_plan_mode": is_plan_mode
        }),
    );

    repeat_prompt(prompt, count)
}

fn should_repeat(mode: PromptRepeatMode, is_plan_mode: bool) -> bool {
    match mode {
        PromptRepeatMode::Off => false,
        PromptRepeatMode::Always => true,
        PromptRepeatMode::NonPlan => !is_plan_mode,
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

    fn capabilities(&self) -> &SmithCapabilities {
        &self.capabilities
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
    use std::ffi::OsString;

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
        // is_plan_mode=true → should NOT repeat
        assert!(!should_repeat(PromptRepeatMode::NonPlan, true));
        // is_plan_mode=false → should repeat
        assert!(should_repeat(PromptRepeatMode::NonPlan, false));
    }

    #[test]
    fn claude_smith_capabilities_name_is_claude() {
        let smith = ClaudeSmith::new("claude -p".to_string());
        assert_eq!(smith.capabilities().name, "claude");
    }

    #[test]
    fn claude_smith_capabilities_does_not_support_structured_outputs() {
        let smith = ClaudeSmith::new("claude -p --permission-mode bypassPermissions".to_string());
        assert!(!smith.capabilities().supports_structured_outputs);
    }

    #[test]
    fn should_repeat_off_mode_never_repeats() {
        assert!(!should_repeat(PromptRepeatMode::Off, false));
        assert!(!should_repeat(PromptRepeatMode::Off, true));
    }

    #[test]
    fn should_repeat_always_mode_always_repeats() {
        assert!(should_repeat(PromptRepeatMode::Always, false));
        assert!(should_repeat(PromptRepeatMode::Always, true));
    }

    #[test]
    fn auth_status_subscription_true_for_non_api_login() {
        let raw = r#"{"loggedIn":true,"authMethod":"oauth"}"#;
        assert!(auth_status_indicates_subscription(raw));
    }

    #[test]
    fn auth_status_subscription_false_for_api_key_login() {
        let raw = r#"{"loggedIn":true,"authMethod":"api_key"}"#;
        assert!(!auth_status_indicates_subscription(raw));
    }

    #[test]
    fn api_key_failure_detector_matches_auth_and_billing_terms() {
        assert!(is_api_key_auth_failure("Invalid API key provided"));
        assert!(is_api_key_auth_failure("credit balance is too low"));
        assert!(!is_api_key_auth_failure(
            "Cannot read properties of undefined"
        ));
    }

    #[test]
    fn claude_subscription_retry_requires_api_key_and_claude_command() {
        let prior = std::env::var_os("ANTHROPIC_API_KEY");
        std::env::set_var("ANTHROPIC_API_KEY", "test-key");
        let eligible = should_retry_with_claude_subscription(
            "claude",
            "claude -p --permission-mode bypassPermissions",
            "invalid api key",
        );
        if let Some(value) = prior {
            std::env::set_var("ANTHROPIC_API_KEY", value);
        } else {
            std::env::remove_var("ANTHROPIC_API_KEY");
        }
        assert!(eligible);
    }

    #[test]
    fn claude_subscription_retry_ignores_non_claude_commands() {
        let prior = std::env::var_os("ANTHROPIC_API_KEY");
        std::env::set_var("ANTHROPIC_API_KEY", "test-key");
        let eligible = should_retry_with_claude_subscription(
            "codex",
            "codex -a never exec -",
            "invalid api key",
        );
        restore_env_var("ANTHROPIC_API_KEY", prior);
        assert!(!eligible);
    }

    #[tokio::test]
    async fn subprocess_captures_auth_error_even_if_stdin_closes_early() {
        let result = run_smith_subprocess(
            "sh",
            &[
                "-lc",
                "exec 0<&-; sleep 0.05; echo 'Invalid API key provided' >&2; exit 1",
            ],
            &"x".repeat(1_000_000),
            None,
            5,
            false,
        )
        .await
        .expect("subprocess should return collected output");

        assert!(!result.output.status.success());
        assert!(result.stdin_error.is_some());
        assert!(smith_failure_detail(&result).contains("Invalid API key provided"));
    }

    fn restore_env_var(name: &str, value: Option<OsString>) {
        if let Some(value) = value {
            std::env::set_var(name, value);
        } else {
            std::env::remove_var(name);
        }
    }
}
