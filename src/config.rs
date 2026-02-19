use std::path::PathBuf;

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
        let base = std::env::var("SLAG_SMITH")
            .unwrap_or_else(|_| "claude --dangerously-skip-permissions -p".to_string());
        let plan = format!("{base} --permission-mode plan");
        let web = format!("{base} --allowedTools 'Bash Edit Read Write Playwright'");
        let web_plan = format!("{web} --permission-mode plan");
        let surveyor = std::env::var("SLAG_SMITH_SURVEYOR").unwrap_or_else(|_| plan.clone());
        let founder = std::env::var("SLAG_SMITH_FOUNDER").unwrap_or_else(|_| base.clone());
        let review = std::env::var("SLAG_SMITH_REVIEW").unwrap_or_else(|_| base.clone());
        let recovery = std::env::var("SLAG_SMITH_RECOVERY").unwrap_or_else(|_| base.clone());
        let independent = std::env::var("SLAG_SMITH_INDEPENDENT").ok().and_then(|v| {
            let trimmed = v.trim().to_string();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        });
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
