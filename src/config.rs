use std::path::{Path, PathBuf};

/// File paths used by the pipeline
pub const BLUEPRINT: &str = "BLUEPRINT.md";
pub const CRUCIBLE: &str = "PLAN.md";
pub const ORE_FILE: &str = "PRD.md";
pub const ALLOY_FILE: &str = "AGENTS.md";
pub const LEDGER: &str = "PROGRESS.md";
pub const PHASES_FILE: &str = "PHASES.md";
pub const LOG_DIR: &str = "logs";
pub const EXPERIMENT_LOG: &str = "logs/experiments.toon";
pub const EXPERIMENT_LOG_JSONL: &str = "logs/experiments.jsonl";
pub const DEFAULT_INGOT_BUDGET_SECS: u64 = 300;

/// Behavior constants
pub const MAX_ANVILS: usize = 6;
pub const HIGH_GRADE: u8 = 3;
pub const MAX_ITERATE: usize = 3;
pub const DEFAULT_PROMPT_TIMEOUT_SECS: u64 = 45;

const CLAUDE_SMITH_DEFAULT: &str = "claude -p --permission-mode bypassPermissions";
const CLAUDE_PLAN_WRAPPER: &str = "claude -p --permission-mode plan";
const KIMI_CLAUDE_WRAPPER: &str = "kimi -p --permission-mode bypassPermissions";
const KIMI_CLAUDE_PLAN_WRAPPER: &str = "kimi -p --permission-mode plan";
const KIMI_NATIVE_WRAPPER: &str =
    r#"sh -lc 'p=$(cat); kimi --print --prompt "$p" --output-format text'"#;
const CODEX_WRAPPER: &str = "codex -a never exec --skip-git-repo-check --color never -";
const GEMINI_WRAPPER: &str = r#"sh -lc 'p=$(cat); gemini -p "$p" --output-format text </dev/null'"#;
const OPENCODE_WRAPPER: &str = r#"sh -lc 'p=$(cat); opencode -q -p "$p" -f text'"#;
const CLAUDE_WEB_ALLOWED_TOOLS: &str = "--allowedTools 'Bash Edit Read Write Playwright'";
const CLAUDE_PLAN_MODE: &str = "--permission-mode plan";

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
    base_chain: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct SmithOverrides {
    pub base: Option<String>,
    pub chain: Option<String>,
}

impl SmithConfig {
    pub fn from_env() -> Self {
        Self::from_env_with_overrides(&SmithOverrides::default())
    }

    pub fn from_env_with_overrides(overrides: &SmithOverrides) -> Self {
        let detected_chain = auto_detect_smith_chain();
        let base = smith_command_from_override_or_env(
            "SLAG_SMITH",
            overrides.base.as_deref(),
            &detected_chain,
        );
        let effort = parse_non_empty_env("SLAG_EFFORT");
        let surveyor_effort =
            parse_non_empty_env("SLAG_SURVEYOR_EFFORT").or_else(|| Some("low".to_string()));
        let plan = route_smith_command(&base, "default", HIGH_GRADE, effort.as_deref());
        let web = route_smith_command(&base, "web", 1, effort.as_deref());
        let web_plan = route_smith_command(&base, "web", HIGH_GRADE, effort.as_deref());
        let surveyor_cmd =
            route_smith_command(&base, "default", HIGH_GRADE, surveyor_effort.as_deref());
        let surveyor = smith_command_override_or("SLAG_SMITH_SURVEYOR", surveyor_cmd);
        let founder = smith_command_override_or("SLAG_SMITH_FOUNDER", base.clone());
        let review = smith_command_override_or("SLAG_SMITH_REVIEW", base.clone());
        let recovery = smith_command_override_or("SLAG_SMITH_RECOVERY", base.clone());
        let independent =
            parse_non_empty_env("SLAG_SMITH_INDEPENDENT").map(normalize_smith_command);
        // Outcome validation should be non-interactive and deterministic by default.
        // Reuse the routed high-grade planning variant unless explicitly overridden.
        let outcome = smith_command_override_or("SLAG_SMITH_OUTCOME", plan.clone());
        let confidence_threshold = parse_confidence("SLAG_CONFIDENCE_THRESHOLD", 0.65);
        let founder_confidence_threshold =
            parse_confidence("SLAG_FOUNDER_CONFIDENCE_THRESHOLD", confidence_threshold);
        let outcome_confidence_threshold =
            parse_confidence("SLAG_OUTCOME_CONFIDENCE_THRESHOLD", confidence_threshold);
        let base_chain = smith_chain_from_override_or_detected(
            &base,
            &detected_chain,
            overrides.chain.as_deref(),
        );
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
            base_chain,
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

    /// Return the base chain for doctor diagnostics (deduped command list).
    pub fn base_chain_for_doctor(&self) -> &[String] {
        &self.base_chain
    }

    /// Select smith command chain (primary + fallbacks) for skill and grade.
    pub fn select_chain(&self, skill: &str, grade: u8) -> Vec<String> {
        let mut chain = Vec::new();
        for base in &self.base_chain {
            let routed = route_smith_command(base, skill, grade, None);
            if !chain.iter().any(|existing| existing == &routed) {
                chain.push(routed);
            }
        }
        if chain.is_empty() {
            chain.push(self.select(skill, grade).to_string());
        }
        chain
    }
}

fn parse_non_empty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn smith_command_from_env(name: &str) -> String {
    parse_non_empty_env(name)
        .map(|value| resolve_smith_selector(&value))
        .unwrap_or_else(default_smith_command)
}

fn smith_command_from_override_or_env(
    name: &str,
    override_value: Option<&str>,
    detected_chain: &[String],
) -> String {
    override_value
        .map(resolve_smith_selector)
        .or_else(|| parse_non_empty_env(name).map(|value| resolve_smith_selector(&value)))
        .unwrap_or_else(|| {
            detected_chain
                .first()
                .cloned()
                .unwrap_or_else(|| CLAUDE_SMITH_DEFAULT.to_string())
        })
}

fn smith_command_override_or(name: &str, fallback: String) -> String {
    parse_non_empty_env(name)
        .map(|value| resolve_smith_selector(&value))
        .unwrap_or(fallback)
}

fn default_smith_command() -> String {
    auto_detect_smith_command().unwrap_or_else(|| CLAUDE_SMITH_DEFAULT.to_string())
}

/// Resolve subagent smith command from env with auto-detect fallback.
pub fn subagent_smith_command_from_env() -> String {
    smith_command_from_env("SLAG_SMITH_SUBAGENT")
}

fn auto_detect_smith_command() -> Option<String> {
    auto_detect_smith_chain().into_iter().next()
}

fn auto_detect_smith_chain() -> Vec<String> {
    let kimi_path = resolve_command_path("kimi");
    let kimi_native = kimi_path
        .as_deref()
        .map(is_native_kimi_cli)
        .unwrap_or(false);
    let kimi_claude_compat = kimi_path
        .as_deref()
        .map(is_claude_compatible_kimi_cli)
        .unwrap_or(false);
    let vllm_available = std::env::var_os("SLAG_VLLM_BASE_URL").is_some()
        || std::env::var_os("OPENAI_BASE_URL").is_some();
    choose_detected_smith_chain_with_policy(
        |cmd| resolve_command_path(cmd).is_some(),
        kimi_native,
        kimi_claude_compat,
        avoid_claude_autodetect_when_api_key_present(),
        vllm_available,
    )
}

fn normalize_smith_command(command: String) -> String {
    resolve_smith_selector(&command)
}

fn resolve_smith_selector(command: &str) -> String {
    let kimi_claude_compat = resolve_command_path("kimi")
        .as_deref()
        .map(is_claude_compatible_kimi_cli)
        .unwrap_or(false);
    let kimi_native = resolve_command_path("kimi")
        .as_deref()
        .map(is_native_kimi_cli)
        .unwrap_or(false);
    resolve_smith_selector_with_detection(command, kimi_native, kimi_claude_compat)
}

fn resolve_smith_selector_with_detection(
    command: &str,
    kimi_native: bool,
    kimi_claude_compat: bool,
) -> String {
    if let Some(resolved) = resolve_smith_alias(command, kimi_native, kimi_claude_compat) {
        resolved
    } else {
        normalize_smith_command_with_detection(command, kimi_claude_compat)
    }
}

fn normalize_smith_command_with_detection(command: &str, kimi_claude_compat: bool) -> String {
    if looks_like_legacy_kimi_wrapper(command) && kimi_claude_compat {
        KIMI_CLAUDE_WRAPPER.to_string()
    } else {
        command.to_string()
    }
}

fn looks_like_legacy_kimi_wrapper(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    lower.contains("kimi")
        && lower.contains("--print")
        && lower.contains("--prompt")
        && lower.contains("--output-format")
}

fn choose_detected_smith<F>(
    has_cmd: F,
    kimi_native: bool,
    kimi_claude_compat: bool,
) -> Option<String>
where
    F: FnMut(&str) -> bool,
{
    choose_detected_smith_chain_with_policy(has_cmd, kimi_native, kimi_claude_compat, false, false)
        .into_iter()
        .next()
}

fn choose_detected_smith_chain<F>(
    has_cmd: F,
    kimi_native: bool,
    kimi_claude_compat: bool,
) -> Vec<String>
where
    F: FnMut(&str) -> bool,
{
    choose_detected_smith_chain_with_policy(has_cmd, kimi_native, kimi_claude_compat, false, false)
}

fn choose_detected_smith_chain_with_policy<F>(
    mut has_cmd: F,
    kimi_native: bool,
    kimi_claude_compat: bool,
    avoid_claude_autodetect: bool,
    vllm_available: bool,
) -> Vec<String>
where
    F: FnMut(&str) -> bool,
{
    let mut chain = Vec::new();
    let has_kimi = has_cmd("kimi");
    let has_claude = has_cmd("claude");
    if has_claude && !avoid_claude_autodetect {
        chain.push(CLAUDE_SMITH_DEFAULT.to_string());
    }
    if has_cmd("codex") {
        chain.push(CODEX_WRAPPER.to_string());
    }
    if has_cmd("gemini") {
        chain.push(GEMINI_WRAPPER.to_string());
    }
    if has_cmd("opencode") {
        chain.push(OPENCODE_WRAPPER.to_string());
    }
    if has_kimi && kimi_claude_compat {
        chain.push(KIMI_CLAUDE_WRAPPER.to_string());
    }
    if has_kimi {
        let cmd = if kimi_native && !kimi_claude_compat {
            KIMI_NATIVE_WRAPPER
        } else {
            KIMI_CLAUDE_WRAPPER
        };
        chain.push(cmd.to_string());
    }
    if has_claude && chain.is_empty() {
        chain.push(CLAUDE_SMITH_DEFAULT.to_string());
    }
    // Check for SLAG_VLLM_BASE_URL — add "vllm" to the chain if set.
    // vLLM is appended after all subprocess smiths so it acts as a fallback
    // in auto-detection, not as the primary smith.
    if vllm_available {
        chain.push("vllm".to_string());
    }
    dedup_preserve_order(chain)
}

fn avoid_claude_autodetect_when_api_key_present() -> bool {
    std::env::var_os("ANTHROPIC_API_KEY").is_some()
}

fn smith_chain_from_override_or_detected(
    base: &str,
    detected_chain: &[String],
    override_value: Option<&str>,
) -> Vec<String> {
    let env_value = parse_non_empty_env("SLAG_SMITH_CHAIN");
    if let Some(raw) = override_value.or(env_value.as_deref()) {
        let kimi_path = resolve_command_path("kimi");
        let kimi_native = kimi_path
            .as_deref()
            .map(is_native_kimi_cli)
            .unwrap_or(false);
        let kimi_claude_compat = kimi_path
            .as_deref()
            .map(is_claude_compatible_kimi_cli)
            .unwrap_or(false);
        let mut chain = parse_smith_chain_tokens(raw, kimi_native, kimi_claude_compat);
        if chain.is_empty() {
            chain.push(base.to_string());
        }
        return dedup_preserve_order(chain);
    }

    let mut chain = Vec::new();
    chain.push(base.to_string());
    chain.extend(detected_chain.iter().cloned());
    dedup_preserve_order(chain)
}

fn parse_smith_chain_tokens(raw: &str, kimi_native: bool, kimi_claude_compat: bool) -> Vec<String> {
    let mut out = Vec::new();
    for token in raw.split(',') {
        let trimmed = token.trim();
        if trimmed.is_empty() {
            continue;
        }
        out.push(resolve_smith_selector_with_detection(
            trimmed,
            kimi_native,
            kimi_claude_compat,
        ));
    }
    dedup_preserve_order(out)
}

fn resolve_smith_alias(token: &str, kimi_native: bool, kimi_claude_compat: bool) -> Option<String> {
    match token.trim().to_ascii_lowercase().as_str() {
        "claude-plan" => Some(CLAUDE_PLAN_WRAPPER.to_string()),
        "kimi" | "kimi-compat" => {
            if kimi_claude_compat {
                Some(KIMI_CLAUDE_WRAPPER.to_string())
            } else if kimi_native {
                Some(KIMI_NATIVE_WRAPPER.to_string())
            } else {
                None
            }
        }
        "kimi-native" => {
            if kimi_native {
                Some(KIMI_NATIVE_WRAPPER.to_string())
            } else {
                None
            }
        }
        "kimi-plan" | "kimi-compat-plan" => {
            if kimi_claude_compat {
                Some(KIMI_CLAUDE_PLAN_WRAPPER.to_string())
            } else {
                None
            }
        }
        "claude" => Some(CLAUDE_SMITH_DEFAULT.to_string()),
        "codex" => Some(CODEX_WRAPPER.to_string()),
        "gemini" => Some(GEMINI_WRAPPER.to_string()),
        "opencode" => Some(OPENCODE_WRAPPER.to_string()),
        "vllm" | "qwen" | "qwen3" => Some("vllm".to_string()),
        _ => None,
    }
}

fn route_smith_command(base: &str, skill: &str, grade: u8, effort: Option<&str>) -> String {
    if !supports_claude_routing_flags(base) {
        return base.to_string();
    }
    let mut routed = match base.trim() {
        CLAUDE_SMITH_DEFAULT if grade >= HIGH_GRADE => CLAUDE_PLAN_WRAPPER.to_string(),
        KIMI_CLAUDE_WRAPPER if grade >= HIGH_GRADE => KIMI_CLAUDE_PLAN_WRAPPER.to_string(),
        _ => base.to_string(),
    };
    if is_web_skill(skill) && !routed.contains("--allowedTools") {
        routed.push(' ');
        routed.push_str(CLAUDE_WEB_ALLOWED_TOOLS);
    }
    if grade >= HIGH_GRADE
        && !routed.contains("--permission-mode")
        && !routed.contains("--dangerously-skip-permissions")
    {
        routed.push(' ');
        routed.push_str(CLAUDE_PLAN_MODE);
    }
    if let Some(level) = effort {
        if !routed.contains("--effort") {
            routed.push_str(&format!(" --effort {level}"));
        }
    }
    routed
}

fn supports_claude_routing_flags(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    if lower.starts_with("vllm") {
        return false;
    }
    if lower.contains("codex ") || lower.contains("gemini ") || lower.contains("opencode ") {
        return false;
    }
    lower.contains("claude")
        || lower.contains("--dangerously-skip-permissions")
        || lower.contains("--permission-mode")
}

fn is_web_skill(skill: &str) -> bool {
    matches!(
        skill.to_ascii_lowercase().as_str(),
        "web" | "frontend" | "ui" | "css" | "html"
    )
}

fn dedup_preserve_order(mut input: Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    for item in input.drain(..) {
        if !out.iter().any(|existing| existing == &item) {
            out.push(item);
        }
    }
    out
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

fn is_claude_compatible_kimi_cli(path: &Path) -> bool {
    let Ok(output) = std::process::Command::new(path).arg("--help").output() else {
        return false;
    };
    let mut text = String::from_utf8_lossy(&output.stdout).to_string();
    if !output.stderr.is_empty() {
        text.push('\n');
        text.push_str(&String::from_utf8_lossy(&output.stderr));
    }
    help_text_suggests_claude_compat(&text)
}

fn help_text_suggests_claude_compat(help_text: &str) -> bool {
    let lower = help_text.to_ascii_lowercase();
    lower.contains("usage: claude")
        || lower.contains("claude code")
        || lower.contains("--dangerously-skip-permissions")
        || lower.contains("--permission-mode")
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

/// Controls when prompts are repeated to improve instruction following.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptRepeatMode {
    Off,
    NonPlan,
    Always,
}

impl PromptRepeatMode {
    pub fn from_env() -> Self {
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

/// Strategy for recasting (rewriting/splitting) cracked ingots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecastStrategy {
    Conservative,
    Aggressive,
}

/// Capability profile for a smith backend.
pub struct SmithCapabilities {
    pub name: &'static str,
    pub context_window: usize,
    pub supports_thinking_toggle: bool,
    pub supports_structured_outputs: bool,
    pub recast_strategy: RecastStrategy,
    pub few_shot_examples: bool,
    pub prompt_repeat_mode: PromptRepeatMode,
    pub default_temperature: f32,
    pub default_top_p: f32,
    pub default_top_k: u32,
}

impl SmithCapabilities {
    /// Conservative profile used as the trait default for unknown smiths.
    pub fn conservative() -> Self {
        Self {
            name: "unknown",
            context_window: 200_000,
            supports_thinking_toggle: false,
            supports_structured_outputs: false,
            recast_strategy: RecastStrategy::Conservative,
            few_shot_examples: false,
            prompt_repeat_mode: PromptRepeatMode::NonPlan,
            default_temperature: 1.0,
            default_top_p: 1.0,
            default_top_k: 0,
        }
    }

    /// Profile for Claude subprocess smiths.
    pub fn claude() -> Self {
        Self {
            name: "claude",
            context_window: 200_000,
            supports_thinking_toggle: false,
            supports_structured_outputs: false,
            recast_strategy: RecastStrategy::Conservative,
            few_shot_examples: false,
            prompt_repeat_mode: PromptRepeatMode::NonPlan,
            default_temperature: 1.0,
            default_top_p: 1.0,
            default_top_k: 0,
        }
    }

    /// Profile for vLLM/Qwen smiths.
    pub fn vllm() -> Self {
        Self {
            name: "vllm",
            context_window: 32_768,
            supports_thinking_toggle: true,
            supports_structured_outputs: true,
            recast_strategy: RecastStrategy::Aggressive,
            few_shot_examples: true,
            prompt_repeat_mode: PromptRepeatMode::Always,
            default_temperature: 0.7,
            default_top_p: 0.8,
            default_top_k: 20,
        }
    }
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
    /// Operator prompt handling policy
    pub prompt_policy: PromptPolicy,
    /// Prompt timeout in seconds for interactive choices
    pub prompt_timeout_secs: u64,
    /// Terminal log renderer format
    pub log_format: LogFormat,
    /// Disable commission chunking (quarrier phase)
    pub no_quarry: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PromptPolicy {
    #[default]
    Ask,
    AutoRequeue,
    AutoCrack,
    AutoAbort,
}

impl PromptPolicy {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "ask" => Some(Self::Ask),
            "auto-requeue" | "autorequeue" | "requeue" => Some(Self::AutoRequeue),
            "auto-crack" | "autocrack" | "crack" => Some(Self::AutoCrack),
            "auto-abort" | "autoabort" | "abort" => Some(Self::AutoAbort),
            _ => None,
        }
    }

    pub fn from_env() -> Self {
        std::env::var("SLAG_PROMPT_POLICY")
            .ok()
            .and_then(|v| Self::parse(&v))
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LogFormat {
    #[default]
    Text,
    Json,
}

impl LogFormat {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "text" => Some(Self::Text),
            "json" => Some(Self::Json),
            _ => None,
        }
    }

    pub fn from_env() -> Self {
        std::env::var("SLAG_LOG_FORMAT")
            .ok()
            .and_then(|v| Self::parse(&v))
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detected_smith_prefers_claude_over_codex() {
        let selected = choose_detected_smith(|cmd| cmd == "codex" || cmd == "claude", false, false);
        assert_eq!(selected, Some(CLAUDE_SMITH_DEFAULT.to_string()));
    }

    #[test]
    fn detected_smith_prefers_codex_over_native_kimi() {
        let selected = choose_detected_smith(|cmd| cmd == "kimi" || cmd == "codex", true, false);
        assert_eq!(selected, Some(CODEX_WRAPPER.to_string()));
    }

    #[test]
    fn detected_smith_uses_native_kimi_when_only_option() {
        let selected = choose_detected_smith(|cmd| cmd == "kimi", true, false);
        assert_eq!(selected, Some(KIMI_NATIVE_WRAPPER.to_string()));
    }

    #[test]
    fn detected_smith_prefers_claude_over_kimi_claude_wrapper() {
        let selected = choose_detected_smith(|cmd| cmd == "kimi" || cmd == "claude", false, true);
        assert_eq!(selected, Some(CLAUDE_SMITH_DEFAULT.to_string()));
    }

    #[test]
    fn detected_smith_falls_back_to_claude() {
        let selected = choose_detected_smith(|cmd| cmd == "claude", false, false);
        assert_eq!(selected, Some(CLAUDE_SMITH_DEFAULT.to_string()));
    }

    #[test]
    fn detected_smith_none_when_no_commands_available() {
        let selected = choose_detected_smith(|_| false, false, false);
        assert_eq!(selected, None);
    }

    #[test]
    fn detected_smith_uses_claude_wrapper_for_claude_compatible_kimi() {
        let selected = choose_detected_smith(|cmd| cmd == "kimi", true, true);
        assert_eq!(selected, Some(KIMI_CLAUDE_WRAPPER.to_string()));
    }

    #[test]
    fn detected_smith_chain_orders_fallbacks() {
        let chain = choose_detected_smith_chain(
            |cmd| matches!(cmd, "kimi" | "codex" | "claude" | "gemini"),
            true,
            true,
        );
        assert_eq!(
            chain,
            vec![
                CLAUDE_SMITH_DEFAULT.to_string(),
                CODEX_WRAPPER.to_string(),
                GEMINI_WRAPPER.to_string(),
                KIMI_CLAUDE_WRAPPER.to_string()
            ]
        );
    }

    #[test]
    fn detected_smith_autodetect_skips_claude_when_api_key_present() {
        let chain = choose_detected_smith_chain_with_policy(
            |cmd| matches!(cmd, "claude" | "codex" | "gemini"),
            false,
            false,
            true,
            false,
        );
        assert_eq!(
            chain,
            vec![CODEX_WRAPPER.to_string(), GEMINI_WRAPPER.to_string()]
        );
    }

    #[test]
    fn detected_smith_autodetect_keeps_claude_when_only_option() {
        let chain =
            choose_detected_smith_chain_with_policy(|cmd| cmd == "claude", false, false, true, false);
        assert_eq!(chain, vec![CLAUDE_SMITH_DEFAULT.to_string()]);
    }

    #[test]
    fn parse_smith_chain_resolves_aliases_and_dedups() {
        let chain = parse_smith_chain_tokens("kimi,codex,claude,claude-plan,kimi", true, true);
        assert_eq!(
            chain,
            vec![
                KIMI_CLAUDE_WRAPPER.to_string(),
                CODEX_WRAPPER.to_string(),
                CLAUDE_SMITH_DEFAULT.to_string(),
                CLAUDE_PLAN_WRAPPER.to_string()
            ]
        );
    }

    #[test]
    fn normalize_legacy_kimi_wrapper_to_claude_compat() {
        let normalized = normalize_smith_command_with_detection(KIMI_NATIVE_WRAPPER, true);
        assert_eq!(normalized, KIMI_CLAUDE_WRAPPER);
    }

    #[test]
    fn keep_legacy_kimi_wrapper_when_non_compatible() {
        let normalized = normalize_smith_command_with_detection(KIMI_NATIVE_WRAPPER, false);
        assert_eq!(normalized, KIMI_NATIVE_WRAPPER);
    }

    #[test]
    fn leaves_other_commands_unchanged() {
        let normalized = normalize_smith_command_with_detection("codex -a never exec -", true);
        assert_eq!(normalized, "codex -a never exec -");
    }

    #[test]
    fn route_smith_command_replaces_default_claude_bypass_with_plan_for_high_grade() {
        let claude_routed = route_smith_command(CLAUDE_SMITH_DEFAULT, "web", HIGH_GRADE, None);
        assert!(claude_routed.contains("--allowedTools"));
        assert!(claude_routed.contains("--permission-mode plan"));
        assert!(!claude_routed.contains("bypassPermissions"));

        // A base command without --dangerously-skip-permissions SHOULD get plan mode.
        let plain_claude = route_smith_command("claude -p", "web", HIGH_GRADE, None);
        assert!(plain_claude.contains("--allowedTools"));
        assert!(plain_claude.contains("--permission-mode plan"));

        let codex_routed = route_smith_command(CODEX_WRAPPER, "web", HIGH_GRADE, None);
        assert_eq!(codex_routed, CODEX_WRAPPER);
    }

    #[test]
    fn resolve_smith_selector_maps_aliases() {
        assert_eq!(
            resolve_smith_selector_with_detection("claude", true, true),
            CLAUDE_SMITH_DEFAULT
        );
        assert_eq!(
            resolve_smith_selector_with_detection("claude-plan", true, true),
            CLAUDE_PLAN_WRAPPER
        );
        assert_eq!(
            resolve_smith_selector_with_detection("codex", true, true),
            CODEX_WRAPPER
        );
    }

    #[test]
    fn route_smith_command_appends_effort_for_claude() {
        let routed = route_smith_command(CLAUDE_SMITH_DEFAULT, "default", HIGH_GRADE, Some("low"));
        assert!(routed.contains("--effort low"));
    }

    #[test]
    fn route_smith_command_skips_effort_for_non_claude() {
        let routed = route_smith_command(CODEX_WRAPPER, "default", HIGH_GRADE, Some("low"));
        assert!(!routed.contains("--effort"));
    }

    #[test]
    fn route_smith_command_no_effort_when_none() {
        let routed = route_smith_command(CLAUDE_SMITH_DEFAULT, "default", HIGH_GRADE, None);
        assert!(!routed.contains("--effort"));
    }

    #[test]
    fn help_text_detector_flags_claude_compat() {
        let help = "Usage: claude [options]\n--dangerously-skip-permissions";
        assert!(help_text_suggests_claude_compat(help));
        assert!(!help_text_suggests_claude_compat(
            "Usage: kimi [options]\n--print --prompt"
        ));
    }

    #[test]
    fn prompt_policy_parse_aliases() {
        assert_eq!(PromptPolicy::parse("ask"), Some(PromptPolicy::Ask));
        assert_eq!(
            PromptPolicy::parse("auto-requeue"),
            Some(PromptPolicy::AutoRequeue)
        );
        assert_eq!(
            PromptPolicy::parse("autorequeue"),
            Some(PromptPolicy::AutoRequeue)
        );
        assert_eq!(
            PromptPolicy::parse("auto-crack"),
            Some(PromptPolicy::AutoCrack)
        );
        assert_eq!(
            PromptPolicy::parse("auto-abort"),
            Some(PromptPolicy::AutoAbort)
        );
        assert_eq!(PromptPolicy::parse("invalid"), None);
    }

    #[test]
    fn log_format_parse_variants() {
        assert_eq!(LogFormat::parse("text"), Some(LogFormat::Text));
        assert_eq!(LogFormat::parse("json"), Some(LogFormat::Json));
        assert_eq!(LogFormat::parse("invalid"), None);
    }

    #[test]
    fn smith_capabilities_claude_profile() {
        let caps = SmithCapabilities::claude();
        assert_eq!(caps.name, "claude");
        assert!(!caps.supports_structured_outputs);
        assert_eq!(caps.prompt_repeat_mode, PromptRepeatMode::NonPlan);
        assert_eq!(caps.recast_strategy, RecastStrategy::Conservative);
    }

    #[test]
    fn smith_capabilities_vllm_profile() {
        let caps = SmithCapabilities::vllm();
        assert_eq!(caps.name, "vllm");
        assert!(caps.supports_structured_outputs);
        assert_eq!(caps.prompt_repeat_mode, PromptRepeatMode::Always);
        assert_eq!(caps.recast_strategy, RecastStrategy::Aggressive);
    }

    #[test]
    fn smith_capabilities_conservative_profile() {
        let caps = SmithCapabilities::conservative();
        assert_eq!(caps.name, "unknown");
        assert_eq!(caps.prompt_repeat_mode, PromptRepeatMode::NonPlan);
        assert_eq!(caps.recast_strategy, RecastStrategy::Conservative);
    }

    fn parse_repeat_mode(s: &str) -> PromptRepeatMode {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" | "never" | "0" => PromptRepeatMode::Off,
            "always" | "all" | "on" => PromptRepeatMode::Always,
            _ => PromptRepeatMode::NonPlan,
        }
    }

    #[test]
    fn prompt_repeat_mode_parses_default_value() {
        // The default env string "non-plan" falls through to NonPlan
        assert_eq!(parse_repeat_mode("non-plan"), PromptRepeatMode::NonPlan);
        assert_eq!(parse_repeat_mode("unknown-value"), PromptRepeatMode::NonPlan);
        assert_eq!(parse_repeat_mode(""), PromptRepeatMode::NonPlan);
    }

    #[test]
    fn prompt_repeat_mode_parses_off() {
        assert_eq!(parse_repeat_mode("off"), PromptRepeatMode::Off);
        assert_eq!(parse_repeat_mode("never"), PromptRepeatMode::Off);
        assert_eq!(parse_repeat_mode("0"), PromptRepeatMode::Off);
    }

    #[test]
    fn prompt_repeat_mode_parses_always() {
        assert_eq!(parse_repeat_mode("always"), PromptRepeatMode::Always);
        assert_eq!(parse_repeat_mode("all"), PromptRepeatMode::Always);
        assert_eq!(parse_repeat_mode("on"), PromptRepeatMode::Always);
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
        prompt_policy: PromptPolicy,
        prompt_timeout_secs: u64,
        log_format: LogFormat,
        no_quarry: bool,
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
            prompt_policy,
            prompt_timeout_secs,
            log_format,
            no_quarry,
        }
    }

    /// Check if review phase should run
    pub fn should_review(&self) -> bool {
        self.worktree && !self.skip_review
    }
}
