use std::path::{Path, PathBuf};

/// File paths used by the pipeline
pub const BLUEPRINT: &str = "BLUEPRINT.md";
pub const CRUCIBLE: &str = "PLAN.md";
pub const ORE_FILE: &str = "PRD.md";
pub const ALLOY_FILE: &str = "AGENTS.md";
pub const LEDGER: &str = "PROGRESS.md";
pub const LOG_DIR: &str = "logs";

/// Behavior constants
pub const MAX_ANVILS: usize = 3;
pub const HIGH_GRADE: u8 = 3;
pub const MAX_ITERATE: usize = 3;

const CLAUDE_SMITH_DEFAULT: &str = "claude --dangerously-skip-permissions -p";
const KIMI_CLAUDE_WRAPPER: &str = "kimi --dangerously-skip-permissions -p";
const KIMI_NATIVE_WRAPPER: &str =
    r#"sh -lc 'p=$(cat); kimi --print --prompt "$p" --output-format text'"#;
const CODEX_WRAPPER: &str = "codex -a never exec --skip-git-repo-check --color never -";
const GEMINI_WRAPPER: &str = r#"sh -lc 'p=$(cat); gemini -p "$p" --output-format text </dev/null'"#;
const OPENCODE_WRAPPER: &str = r#"sh -lc 'p=$(cat); opencode -q -p "$p" -f text'"#;

/// Smith configuration resolved from environment
pub struct SmithConfig {
    pub base: String,
    pub plan: String,
    pub web: String,
    pub web_plan: String,
    pub surveyor: String,
    pub founder: String,
    pub review: String,
    pub recovery: String,
    pub outcome: String,
    pub independent: Option<String>,
    pub confidence_threshold: f32,
    pub founder_confidence_threshold: f32,
    pub outcome_confidence_threshold: f32,
}

impl SmithConfig {
    pub fn from_env() -> Self {
        let base = parse_non_empty_env("SLAG_SMITH").unwrap_or_else(default_smith_command);
        let plan = format!("{base} --permission-mode plan");
        let web = format!("{base} --allowedTools 'Bash Edit Read Write Playwright'");
        let web_plan = format!("{web} --permission-mode plan");
        let surveyor = std::env::var("SLAG_SMITH_SURVEYOR").unwrap_or_else(|_| plan.clone());
        let founder = std::env::var("SLAG_SMITH_FOUNDER").unwrap_or_else(|_| base.clone());
        let review = std::env::var("SLAG_SMITH_REVIEW").unwrap_or_else(|_| base.clone());
        let recovery = std::env::var("SLAG_SMITH_RECOVERY").unwrap_or_else(|_| base.clone());
        let independent = parse_non_empty_env("SLAG_SMITH_INDEPENDENT");
        // Outcome validation should be non-interactive and deterministic by default.
        // Use plan mode unless explicitly overridden by SLAG_SMITH_OUTCOME.
        let outcome = std::env::var("SLAG_SMITH_OUTCOME").unwrap_or_else(|_| plan.clone());
        let confidence_threshold = parse_confidence("SLAG_CONFIDENCE_THRESHOLD", 0.65);
        let founder_confidence_threshold =
            parse_confidence("SLAG_FOUNDER_CONFIDENCE_THRESHOLD", confidence_threshold);
        let outcome_confidence_threshold =
            parse_confidence("SLAG_OUTCOME_CONFIDENCE_THRESHOLD", confidence_threshold);
        Self {
            base,
            plan,
            web,
            web_plan,
            surveyor,
            founder,
            review,
            recovery,
            outcome,
            independent,
            confidence_threshold,
            founder_confidence_threshold,
            outcome_confidence_threshold,
        }
    }

    /// Select smith command based on skill and grade
    pub fn select(&self, skill: &str, grade: u8) -> &str {
        match skill {
            "web" | "frontend" | "ui" | "css" | "html" => {
                if grade >= HIGH_GRADE {
                    &self.web_plan
                } else {
                    &self.web
                }
            }
            _ => {
                if grade >= HIGH_GRADE {
                    &self.plan
                } else {
                    &self.base
                }
            }
        }
    }
}

fn parse_non_empty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn default_smith_command() -> String {
    auto_detect_smith_command().unwrap_or_else(|| CLAUDE_SMITH_DEFAULT.to_string())
}

fn auto_detect_smith_command() -> Option<String> {
    let kimi_native = resolve_command_path("kimi")
        .as_deref()
        .map(is_native_kimi_cli)
        .unwrap_or(false);
    choose_detected_smith(|cmd| resolve_command_path(cmd).is_some(), kimi_native)
}

fn choose_detected_smith<F>(mut has_cmd: F, kimi_native: bool) -> Option<String>
where
    F: FnMut(&str) -> bool,
{
    if has_cmd("kimi") {
        let cmd = if kimi_native {
            KIMI_NATIVE_WRAPPER
        } else {
            KIMI_CLAUDE_WRAPPER
        };
        return Some(cmd.to_string());
    }
    if has_cmd("codex") {
        return Some(CODEX_WRAPPER.to_string());
    }
    if has_cmd("gemini") {
        return Some(GEMINI_WRAPPER.to_string());
    }
    if has_cmd("opencode") {
        return Some(OPENCODE_WRAPPER.to_string());
    }
    if has_cmd("claude") {
        return Some(CLAUDE_SMITH_DEFAULT.to_string());
    }
    None
}

fn resolve_command_path(command: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let candidate = dir.join(command);
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn is_native_kimi_cli(path: &Path) -> bool {
    let mut current = path.to_path_buf();
    for _ in 0..4 {
        if current
            .to_string_lossy()
            .to_ascii_lowercase()
            .contains("kimi-cli")
        {
            return true;
        }
        let Ok(link) = std::fs::read_link(&current) else {
            break;
        };
        current = if link.is_absolute() {
            link
        } else {
            current.parent().unwrap_or_else(|| Path::new("")).join(link)
        };
    }
    false
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|meta| meta.is_file() && (meta.permissions().mode() & 0o111) != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|meta| meta.is_file())
        .unwrap_or(false)
}

fn parse_confidence(name: &str, default: f32) -> f32 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<f32>().ok())
        .map(|v| v.clamp(0.0, 1.0))
        .unwrap_or(default)
}

/// Resolve a project-relative path
pub fn project_path(filename: &str) -> PathBuf {
    PathBuf::from(filename)
}

/// Pipeline execution configuration (from CLI flags)
#[derive(Debug, Clone, Default)]
pub struct PipelineConfig {
    /// Enable worktree isolation per ingot
    pub worktree: bool,
    /// Max parallel anvils
    pub max_anvils: usize,
    /// Skip the review phase
    pub skip_review: bool,
    /// Keep branches after review
    pub keep_branches: bool,
    /// CI checks only, no AI review
    pub ci_only: bool,
    /// Review even if CI fails
    pub review_all: bool,
    /// Max retry cycles when ingots crack
    pub max_retry: usize,
    /// Show detailed forge output
    pub verbose: bool,
    /// Run independent outcome-validation closing loop
    pub outcome_gate: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detected_smith_prefers_native_kimi_first() {
        let selected = choose_detected_smith(|cmd| cmd == "kimi" || cmd == "claude", true);
        assert_eq!(selected, Some(KIMI_NATIVE_WRAPPER.to_string()));
    }

    #[test]
    fn detected_smith_prefers_kimi_claude_wrapper_when_non_native() {
        let selected = choose_detected_smith(|cmd| cmd == "kimi" || cmd == "claude", false);
        assert_eq!(selected, Some(KIMI_CLAUDE_WRAPPER.to_string()));
    }

    #[test]
    fn detected_smith_prefers_codex_before_claude() {
        let selected = choose_detected_smith(|cmd| cmd == "codex" || cmd == "claude", false);
        assert_eq!(selected, Some(CODEX_WRAPPER.to_string()));
    }

    #[test]
    fn detected_smith_falls_back_to_claude() {
        let selected = choose_detected_smith(|cmd| cmd == "claude", false);
        assert_eq!(selected, Some(CLAUDE_SMITH_DEFAULT.to_string()));
    }

    #[test]
    fn detected_smith_none_when_no_commands_available() {
        let selected = choose_detected_smith(|_| false, false);
        assert_eq!(selected, None);
    }
}

impl PipelineConfig {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        worktree: bool,
        max_anvils: usize,
        skip_review: bool,
        keep_branches: bool,
        ci_only: bool,
        review_all: bool,
        max_retry: usize,
        verbose: bool,
        outcome_gate: bool,
    ) -> Self {
        Self {
            worktree,
            max_anvils,
            skip_review,
            keep_branches,
            ci_only,
            review_all,
            max_retry,
            verbose,
            outcome_gate,
        }
    }

    /// Check if review phase should run
    pub fn should_review(&self) -> bool {
        self.worktree && !self.skip_review
    }
}
