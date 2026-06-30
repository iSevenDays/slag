use crate::config::{LogFormat, PromptPolicy, SmithOverrides};
use clap::{Parser, Subcommand};

#[derive(Debug, Subcommand)]
pub enum SmithCommands {
    /// Probe each configured smith and print reachability + capability summary
    Doctor,
}

#[derive(Parser)]
#[command(
    name = "slag",
    about = "Smelt ideas, skim the bugs, forge the product.",
    version,
    author
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Commission (new project description)
    #[arg(trailing_var_arg = true)]
    pub commission: Vec<String>,

    /// Enable branch-per-ingot worktree isolation with master review
    #[arg(long)]
    pub worktree: bool,

    /// Max parallel anvil workers
    #[arg(long, default_value_t = crate::config::MAX_ANVILS)]
    pub anvils: usize,

    /// Skip the master review phase (legacy behavior)
    #[arg(long)]
    pub skip_review: bool,

    /// Don't delete branches after review
    #[arg(long)]
    pub keep_branches: bool,

    /// Run CI checks but skip AI review
    #[arg(long)]
    pub ci_only: bool,

    /// Review even if CI fails
    #[arg(long)]
    pub review_all: bool,

    /// Max retry cycles when ingots crack (0 = no retry)
    #[arg(long, default_value_t = 3)]
    pub retry: usize,

    /// Show detailed forge output (commands, retries, previews, and stall heartbeats)
    #[arg(long, visible_alias = "debug")]
    pub verbose: bool,

    /// Disable independent outcome-validation closing loop
    #[arg(long)]
    pub no_outcome: bool,

    /// Disable commission chunking (quarrier phase)
    #[arg(long)]
    pub no_quarry: bool,

    /// Operator prompt policy (ask|auto-requeue|auto-crack|auto-abort)
    #[arg(long, value_enum)]
    pub prompt_policy: Option<PromptPolicyArg>,

    /// Timeout for interactive prompts in seconds
    #[arg(long)]
    pub prompt_timeout_secs: Option<u64>,

    /// Log renderer format (text or json)
    #[arg(long, value_enum)]
    pub log_format: Option<LogFormatArg>,

    /// Smith effort level (low, medium, high) — controls extended thinking budget
    #[arg(long)]
    pub effort: Option<String>,

    /// Smith to use (alias or full command, e.g. claude, claude-plan, codex)
    #[arg(long)]
    pub smith: Option<String>,

    /// Fallback smith chain (comma-separated aliases or full commands)
    #[arg(long)]
    pub smith_chain: Option<String>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Show crucible state
    Status,

    /// Resume existing forge
    Resume,

    /// Self-update to latest release
    Update,

    /// Run slag on its own codebase to improve code quality
    SelfImprove {
        /// Improvement target: quality|tests|performance|tokens or freeform text
        #[arg(trailing_var_arg = true, default_values_t = vec!["quality".to_string()])]
        target: Vec<String>,
    },

    /// Smith diagnostics and configuration tools
    Smith {
        #[command(subcommand)]
        subcommand: SmithCommands,
    },
}

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
pub enum PromptPolicyArg {
    Ask,
    AutoRequeue,
    AutoCrack,
    AutoAbort,
}

impl PromptPolicyArg {
    pub fn to_config(self) -> PromptPolicy {
        match self {
            Self::Ask => PromptPolicy::Ask,
            Self::AutoRequeue => PromptPolicy::AutoRequeue,
            Self::AutoCrack => PromptPolicy::AutoCrack,
            Self::AutoAbort => PromptPolicy::AutoAbort,
        }
    }
}

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
pub enum LogFormatArg {
    Text,
    Json,
}

impl LogFormatArg {
    pub fn to_config(self) -> LogFormat {
        match self {
            Self::Text => LogFormat::Text,
            Self::Json => LogFormat::Json,
        }
    }
}

impl Cli {
    pub fn commission_text(&self) -> Option<String> {
        if self.commission.is_empty() {
            None
        } else {
            Some(self.commission.join(" "))
        }
    }

    pub fn smith_overrides(&self) -> SmithOverrides {
        SmithOverrides {
            base: self.smith.clone(),
            chain: self.smith_chain.clone(),
        }
    }
}
