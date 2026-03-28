use std::path::{Path, PathBuf};

use crate::config::{PipelineConfig, SmithConfig};
use crate::error::SlagError;
use crate::tui;

const BRANCH_NAME: &str = "self-improve";
const SLAG_ARTIFACTS: &[&str] = &[
    "PLAN.md",
    "BLUEPRINT.md",
    "PRD.md",
    "PROGRESS.md",
    "AGENTS.md",
    "PHASES.md",
];

/// Metrics measured before and after self-improvement.
#[derive(Debug, Clone)]
struct Metrics {
    test_pass: usize,
    test_fail: usize,
    clippy_warnings: usize,
}

impl Metrics {
    async fn measure(dir: &Path) -> Self {
        let (test_pass, test_fail) = parse_test_output(dir).await;
        let clippy_warnings = count_clippy_warnings(dir).await;
        Self {
            test_pass,
            test_fail,
            clippy_warnings,
        }
    }

    fn is_improvement_over(&self, baseline: &Self) -> bool {
        // Must not regress on tests
        if self.test_fail > baseline.test_fail {
            return false;
        }
        // At least one metric must improve
        let more_tests = self.test_pass > baseline.test_pass;
        let fewer_warnings = self.clippy_warnings < baseline.clippy_warnings;
        let fewer_failures = self.test_fail < baseline.test_fail;
        more_tests || fewer_warnings || fewer_failures
    }

    fn summary(&self) -> String {
        format!(
            "tests: {} pass / {} fail, clippy: {} warnings",
            self.test_pass, self.test_fail, self.clippy_warnings
        )
    }

    fn delta_summary(&self, baseline: &Self) -> String {
        let test_delta = self.test_pass as i64 - baseline.test_pass as i64;
        let clippy_delta = self.clippy_warnings as i64 - baseline.clippy_warnings as i64;
        format!("tests: {:+}, clippy: {:+}", test_delta, clippy_delta)
    }
}

/// Run the self-improvement loop:
/// 1. Pre-flight checks
/// 2. Measure baseline
/// 3. Create worktree sandbox
/// 4. Run forge pipeline in sandbox
/// 5. Measure after
/// 6. Keep if improved, discard if not
pub async fn run(
    target: &str,
    smith_config: &SmithConfig,
    pipeline_config: &PipelineConfig,
) -> Result<(), SlagError> {
    let source_dir = std::env::current_dir().map_err(|e| SlagError::Io(e))?;

    tui::header("SELF-IMPROVE · meta-forge");

    // Pre-flight: clean working tree
    if !git_is_clean(&source_dir).await {
        return Err(SlagError::WorktreeError(
            "working tree has uncommitted changes — commit or stash first".into(),
        ));
    }

    // Pre-flight: cargo test passes
    tui::status_line("1", tui::COLD, "Pre-flight: verifying cargo test passes...");
    let baseline = Metrics::measure(&source_dir).await;
    if baseline.test_fail > 0 {
        return Err(SlagError::WorktreeError(format!(
            "baseline has {} failing tests — fix before self-improving",
            baseline.test_fail
        )));
    }
    println!(
        "  \x1b[90mbaseline: {}\x1b[0m",
        baseline.summary()
    );

    // Create sandbox worktree
    tui::status_line("2", tui::COLD, "Creating worktree sandbox...");
    let sandbox = create_sandbox(&source_dir).await?;
    println!("  \x1b[90msandbox: {}\x1b[0m", sandbox.display());

    // Generate commission
    let commission = generate_commission(target, &baseline);
    let prd_path = sandbox.join("PRD.md");
    std::fs::write(&prd_path, &commission)?;
    tui::status_line("3", tui::WARM, &format!("Commission: {target}"));

    // Run pipeline in sandbox
    tui::status_line("4", tui::HOT, "Forging in sandbox...");
    let original_dir = std::env::current_dir().map_err(|e| SlagError::Io(e))?;
    std::env::set_current_dir(&sandbox).map_err(|e| SlagError::Io(e))?;

    // Ensure logs dir exists in sandbox
    let _ = std::fs::create_dir_all("logs");

    let forge_result =
        crate::pipeline::run(Some(&commission), smith_config, pipeline_config).await;

    // Restore original directory
    std::env::set_current_dir(&original_dir).map_err(|e| SlagError::Io(e))?;

    if let Err(ref e) = forge_result {
        tui::status_line("x", tui::WARM, &format!("Forge failed: {e}"));
        discard_and_cleanup(&source_dir, &sandbox).await;
        return forge_result;
    }

    // Measure after
    tui::status_line("5", tui::BRIGHT, "Measuring improvement...");
    let after = Metrics::measure(&sandbox).await;
    println!(
        "  \x1b[90mafter: {}\x1b[0m",
        after.summary()
    );
    println!(
        "  \x1b[90mdelta: {}\x1b[0m",
        after.delta_summary(&baseline)
    );

    // Decide
    if after.is_improvement_over(&baseline) {
        tui::status_line("6", tui::PURE, "Improvement detected — merging");

        // Strip slag artifacts from branch before merge
        strip_artifacts_from_sandbox(&sandbox).await;

        // Merge back to main
        merge_and_cleanup(&source_dir, &sandbox).await?;

        tui::status_line(
            ">>",
            tui::PURE,
            &format!("Self-improvement accepted ({})", after.delta_summary(&baseline)),
        );
    } else {
        tui::status_line("6", tui::WARM, "No improvement — discarding sandbox");
        discard_and_cleanup(&source_dir, &sandbox).await;
    }

    Ok(())
}

fn generate_commission(target: &str, baseline: &Metrics) -> String {
    match target {
        "quality" => format!(
            "Improve code quality in this Rust project.\n\
            Current state: {} tests pass, {} clippy warnings.\n\
            Fix clippy warnings in src/pipeline/ and src/flux.rs.\n\
            Do NOT modify test files unless adding new tests.\n\
            Acceptance: cargo test --all passes with 0 failures, cargo clippy -- -D warnings shows fewer warnings.",
            baseline.test_pass, baseline.clippy_warnings
        ),
        "tests" => format!(
            "Add unit tests to this Rust project.\n\
            Current state: {} tests pass.\n\
            Add tests for untested functions in src/pipeline/forge.rs and src/pipeline/resmelt.rs.\n\
            Target: increase test count by at least 5.\n\
            Acceptance: cargo test --all passes with more than {} tests.",
            baseline.test_pass, baseline.test_pass
        ),
        "performance" => format!(
            "Optimize performance in this Rust project.\n\
            Focus on src/pipeline/forge.rs — reduce unnecessary allocations, \
            avoid redundant file reads, minimize string copies in the heat loop.\n\
            Acceptance: cargo test --all passes with 0 failures."
        ),
        "tokens" => format!(
            "Reduce LLM token usage in prompts generated by src/flux.rs.\n\
            Shorten templates, compress repeated data, remove redundant sections.\n\
            Do NOT change the prompt semantics — the smith must still understand the task.\n\
            Acceptance: cargo test --all passes with 0 failures."
        ),
        _ => format!(
            "Improve this Rust project: {target}\n\
            Acceptance: cargo test --all passes with 0 failures."
        ),
    }
}

// --- Worktree sandbox lifecycle ---

async fn create_sandbox(source_dir: &Path) -> Result<PathBuf, SlagError> {
    let ts = chrono::Utc::now().timestamp();
    let sandbox = PathBuf::from(format!("/tmp/slag-self-improve-{ts}"));

    // Clean stale branch if exists
    let _ = tokio::process::Command::new("git")
        .args(["branch", "-D", BRANCH_NAME])
        .current_dir(source_dir)
        .output()
        .await;

    let output = tokio::process::Command::new("git")
        .args([
            "worktree",
            "add",
            sandbox.to_str().unwrap(),
            "-b",
            BRANCH_NAME,
        ])
        .current_dir(source_dir)
        .output()
        .await
        .map_err(|e| SlagError::WorktreeError(format!("spawn failed: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(SlagError::WorktreeError(format!(
            "worktree add failed: {stderr}"
        )));
    }

    Ok(sandbox)
}

async fn strip_artifacts_from_sandbox(sandbox: &Path) {
    // Remove slag artifacts from git index (keep on disk but don't merge them)
    for artifact in SLAG_ARTIFACTS {
        let _ = tokio::process::Command::new("git")
            .args(["rm", "--cached", "--ignore-unmatch", artifact])
            .current_dir(sandbox)
            .output()
            .await;
    }
    let _ = tokio::process::Command::new("git")
        .args(["rm", "-r", "--cached", "--ignore-unmatch", "logs"])
        .current_dir(sandbox)
        .output()
        .await;

    // Commit the artifact removal
    let _ = tokio::process::Command::new("git")
        .args([
            "commit",
            "--allow-empty",
            "-m",
            "self-improve: strip artifacts before merge",
            "--quiet",
        ])
        .current_dir(sandbox)
        .output()
        .await;
}

async fn merge_and_cleanup(
    source_dir: &Path,
    sandbox: &Path,
) -> Result<(), SlagError> {
    let output = tokio::process::Command::new("git")
        .args(["merge", BRANCH_NAME, "--no-edit"])
        .current_dir(source_dir)
        .output()
        .await
        .map_err(|e| SlagError::WorktreeError(format!("merge failed: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Clean up even on merge failure
        discard_and_cleanup(source_dir, sandbox).await;
        return Err(SlagError::WorktreeError(format!(
            "merge self-improve failed: {stderr}"
        )));
    }

    // Remove worktree
    let _ = tokio::process::Command::new("git")
        .args(["worktree", "remove", sandbox.to_str().unwrap()])
        .current_dir(source_dir)
        .output()
        .await;

    // Delete branch
    let _ = tokio::process::Command::new("git")
        .args(["branch", "-D", BRANCH_NAME])
        .current_dir(source_dir)
        .output()
        .await;

    Ok(())
}

async fn discard_and_cleanup(source_dir: &Path, sandbox: &Path) {
    let _ = tokio::process::Command::new("git")
        .args(["worktree", "remove", "--force", sandbox.to_str().unwrap()])
        .current_dir(source_dir)
        .output()
        .await;
    let _ = tokio::process::Command::new("git")
        .args(["branch", "-D", BRANCH_NAME])
        .current_dir(source_dir)
        .output()
        .await;
}

// --- Metrics collection ---

async fn parse_test_output(dir: &Path) -> (usize, usize) {
    let output = tokio::process::Command::new("cargo")
        .args(["test", "--all"])
        .current_dir(dir)
        .output()
        .await;

    let Ok(output) = output else {
        return (0, 0);
    };

    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    // Parse "test result: ok. N passed; M failed" or "N passed"
    let mut pass = 0usize;
    let mut fail = 0usize;
    for line in text.lines() {
        let lower = line.to_lowercase();
        if lower.contains("passed") {
            if let Some(n) = extract_number_before(&lower, "passed") {
                pass = n;
            }
        }
        if lower.contains("failed") && !lower.contains("0 failed") {
            if let Some(n) = extract_number_before(&lower, "failed") {
                fail = n;
            }
        }
    }
    (pass, fail)
}

async fn count_clippy_warnings(dir: &Path) -> usize {
    let output = tokio::process::Command::new("cargo")
        .args(["clippy", "--all", "--", "-W", "clippy::all"])
        .current_dir(dir)
        .output()
        .await;

    let Ok(output) = output else {
        return 0;
    };

    let text = String::from_utf8_lossy(&output.stderr);
    text.lines()
        .filter(|l| l.trim_start().starts_with("warning:") || l.trim_start().starts_with("error:"))
        .count()
}

async fn git_is_clean(dir: &Path) -> bool {
    tokio::process::Command::new("git")
        .args(["diff", "--quiet", "HEAD"])
        .current_dir(dir)
        .output()
        .await
        .ok()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn extract_number_before(text: &str, keyword: &str) -> Option<usize> {
    let idx = text.find(keyword)?;
    let before = &text[..idx];
    before
        .split_whitespace()
        .next_back()?
        .trim_end_matches(';')
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_improvement_detection() {
        let baseline = Metrics {
            test_pass: 100,
            test_fail: 0,
            clippy_warnings: 5,
        };
        let improved = Metrics {
            test_pass: 105,
            test_fail: 0,
            clippy_warnings: 3,
        };
        let regressed = Metrics {
            test_pass: 100,
            test_fail: 2,
            clippy_warnings: 3,
        };
        let unchanged = Metrics {
            test_pass: 100,
            test_fail: 0,
            clippy_warnings: 5,
        };

        assert!(improved.is_improvement_over(&baseline));
        assert!(!regressed.is_improvement_over(&baseline));
        assert!(!unchanged.is_improvement_over(&baseline));
    }

    #[test]
    fn extract_number_from_test_output() {
        assert_eq!(extract_number_before("134 passed; 0 failed", "passed"), Some(134));
        assert_eq!(extract_number_before("test result: ok. 42 passed", "passed"), Some(42));
        assert_eq!(extract_number_before("2 failed", "failed"), Some(2));
    }

    #[test]
    fn commission_generation_includes_baseline() {
        let baseline = Metrics {
            test_pass: 134,
            test_fail: 0,
            clippy_warnings: 3,
        };
        let commission = generate_commission("quality", &baseline);
        assert!(commission.contains("134"));
        assert!(commission.contains("3 clippy"));
    }

    #[test]
    fn delta_summary_format() {
        let baseline = Metrics { test_pass: 100, test_fail: 0, clippy_warnings: 5 };
        let after = Metrics { test_pass: 105, test_fail: 0, clippy_warnings: 2 };
        assert_eq!(after.delta_summary(&baseline), "tests: +5, clippy: -3");
    }
}
