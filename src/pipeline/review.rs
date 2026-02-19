use std::collections::HashSet;
use std::path::Path;

use crate::config::{PipelineConfig, CRUCIBLE};
use crate::crucible::Crucible;
use crate::error::SlagError;
use crate::events;
use crate::flux;
use crate::sexp::parser::parse_ingot;
use crate::sexp::{Ingot, Status};
use crate::smith::Smith;
use crate::tui;

use super::forge::ForgeResult;

/// CI check results for a branch
#[derive(Debug, Clone)]
pub struct CiResult {
    pub fmt_passed: bool,
    pub fmt_output: String,
    pub clippy_passed: bool,
    pub clippy_output: String,
    pub test_passed: bool,
    pub test_output: String,
}

impl CiResult {
    pub fn passed(&self) -> bool {
        self.fmt_passed && self.clippy_passed && self.test_passed
    }

    pub fn summary(&self) -> String {
        let fmt = if self.fmt_passed { "✓" } else { "✗" };
        let clippy = if self.clippy_passed { "✓" } else { "✗" };
        let test = if self.test_passed { "✓" } else { "✗" };
        format!("fmt:{fmt} clippy:{clippy} test:{test}")
    }
}

/// Master review result
#[derive(Debug, Clone)]
pub struct ReviewResult {
    pub approved: bool,
    pub comments: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReviewerLane {
    Build,
    Behavior,
    Risk,
}

impl ReviewerLane {
    fn all() -> [Self; 3] {
        [Self::Build, Self::Behavior, Self::Risk]
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Build => "build",
            Self::Behavior => "behavior",
            Self::Risk => "risk",
        }
    }
}

#[derive(Debug, Clone)]
struct LaneReviewResult {
    lane: ReviewerLane,
    passed: bool,
    schema_valid: bool,
    evidence: String,
    fix_ingots: Vec<Ingot>,
}

/// Phase 3.5: Review — master agent quality gate
pub async fn run(
    smith: &dyn Smith,
    config: &PipelineConfig,
    forged_results: &[ForgeResult],
) -> Result<(), SlagError> {
    events::emit_info(
        "review.start",
        "starting review phase",
        serde_json::json!({
            "forged_results": forged_results.len(),
            "ci_only": config.ci_only,
            "review_all": config.review_all
        }),
    );
    tui::header("REVIEW · master agent quality gate");

    let branches: Vec<&ForgeResult> = forged_results
        .iter()
        .filter(|r| r.branch.is_some())
        .collect();

    if branches.is_empty() {
        println!("  \x1b[90mNo branches to review\x1b[0m");
        return Ok(());
    }

    println!(
        "  \x1b[38;5;208m⚖\x1b[0m Reviewing {} branches",
        branches.len()
    );

    let mut approved_count = 0;
    let mut rejected_count = 0;

    for forge_result in branches {
        let branch = forge_result.branch.as_ref().unwrap();
        let worktree_path = forge_result.worktree_path.as_deref();

        println!(
            "\n  \x1b[1;37m[{}]\x1b[0m branch: \x1b[90m{}\x1b[0m",
            forge_result.id, branch
        );

        // Run CI checks
        let ci_result = run_ci_checks(branch, worktree_path).await;
        println!("    CI: {}", ci_result.summary());

        if !ci_result.passed() {
            print_ci_failures(&ci_result);
            if !config.review_all {
                println!("    \x1b[31m✗\x1b[0m skipping AI review (CI failed)");
                rejected_count += 1;
                mark_ingot_cracked(&forge_result.id)?;
                cleanup_branch(&forge_result.id, config.keep_branches).await;
                continue;
            }
        }

        // Skip AI review if ci_only mode
        if config.ci_only {
            if ci_result.passed() {
                println!("    \x1b[38;5;220m◐\x1b[0m CI passed, merging (--ci-only)");
                merge_branch(&forge_result.id).await?;
                approved_count += 1;
            } else {
                rejected_count += 1;
                mark_ingot_cracked(&forge_result.id)?;
                cleanup_branch(&forge_result.id, config.keep_branches).await;
            }
            continue;
        }

        // Get diff for AI review
        let diff = get_branch_diff(branch).await;

        let spinner = tui::spinner("reviewing lanes...");
        let lane_results =
            run_reviewer_lanes(smith, &forge_result.id, branch, &diff, &ci_result).await;
        spinner.finish_and_clear();

        match lane_results {
            Ok(results) => {
                let failed: Vec<&LaneReviewResult> = results.iter().filter(|r| !r.passed).collect();
                if failed.is_empty() {
                    println!("    \x1b[1;37m█\x1b[0m approved (3 lanes)");
                    events::emit_info(
                        "review.approved",
                        "all reviewer lanes passed",
                        serde_json::json!({
                            "ingot_id": forge_result.id,
                            "branch": branch
                        }),
                    );
                    merge_branch(&forge_result.id).await?;
                    approved_count += 1;
                } else {
                    println!("    \x1b[31m✗\x1b[0m rejected by {} lane(s)", failed.len());
                    for result in &failed {
                        println!(
                            "    \x1b[90m↳ {}: {}\x1b[0m",
                            result.lane.as_str(),
                            tui::truncate(&result.evidence, 80)
                        );
                    }

                    let queued = queue_lane_fix_ingots(&forge_result.id, &results)?;
                    if queued > 0 {
                        println!(
                            "    \x1b[38;5;220m↺\x1b[0m queued {} reviewer fix ingot(s)",
                            queued
                        );
                    }
                    events::emit_warn(
                        "review.rejected",
                        "one or more reviewer lanes rejected branch",
                        serde_json::json!({
                            "ingot_id": forge_result.id,
                            "branch": branch,
                            "failed_lanes": failed.iter().map(|r| r.lane.as_str()).collect::<Vec<_>>(),
                            "queued_fix_ingots": queued,
                        }),
                    );
                    rejected_count += 1;
                    mark_ingot_cracked(&forge_result.id)?;
                    cleanup_branch(&forge_result.id, config.keep_branches).await;
                }
            }
            Err(e) => {
                eprintln!("    \x1b[31m✗\x1b[0m review error: {e}");
                events::emit_error(
                    "review.error",
                    "review lane execution failed",
                    serde_json::json!({
                        "ingot_id": forge_result.id,
                        "branch": branch,
                        "error": e.to_string()
                    }),
                );
                rejected_count += 1;
                mark_ingot_cracked(&forge_result.id)?;
                cleanup_branch(&forge_result.id, config.keep_branches).await;
            }
        }
    }

    // Summary
    println!();
    println!(
        "  \x1b[38;5;220m⚖\x1b[0m Review complete: \x1b[1;37m{}\x1b[0m approved, \x1b[31m{}\x1b[0m rejected",
        approved_count, rejected_count
    );

    if rejected_count > 0 && approved_count == 0 {
        return Err(SlagError::ReviewFailed(rejected_count));
    }

    Ok(())
}

/// Run CI checks on a branch
async fn run_ci_checks(branch: &str, worktree_path: Option<&str>) -> CiResult {
    let dir = worktree_path.unwrap_or(".");

    // Checkout branch if in main repo
    if worktree_path.is_none() {
        let _ = tokio::process::Command::new("git")
            .args(["checkout", branch])
            .output()
            .await;
    }

    // cargo fmt --check
    let fmt_output = tokio::process::Command::new("cargo")
        .args(["fmt", "--check"])
        .current_dir(dir)
        .output()
        .await;
    let (fmt_passed, fmt_output) = match fmt_output {
        Ok(o) => (
            o.status.success(),
            String::from_utf8_lossy(&o.stderr).to_string(),
        ),
        Err(e) => (false, format!("spawn error: {e}")),
    };

    // cargo clippy
    let clippy_output = tokio::process::Command::new("cargo")
        .args(["clippy", "--", "-D", "warnings"])
        .current_dir(dir)
        .output()
        .await;
    let (clippy_passed, clippy_output) = match clippy_output {
        Ok(o) => (
            o.status.success(),
            String::from_utf8_lossy(&o.stderr).to_string(),
        ),
        Err(e) => (false, format!("spawn error: {e}")),
    };

    // cargo test
    let test_output = tokio::process::Command::new("cargo")
        .args(["test", "--all"])
        .current_dir(dir)
        .output()
        .await;
    let (test_passed, test_output) = match test_output {
        Ok(o) => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            let stderr = String::from_utf8_lossy(&o.stderr);
            (o.status.success(), format!("{stdout}{stderr}"))
        }
        Err(e) => (false, format!("spawn error: {e}")),
    };

    // Checkout back to main if needed
    if worktree_path.is_none() {
        let _ = tokio::process::Command::new("git")
            .args(["checkout", "main"])
            .output()
            .await;
    }

    CiResult {
        fmt_passed,
        fmt_output,
        clippy_passed,
        clippy_output,
        test_passed,
        test_output,
    }
}

/// Get the diff for a branch compared to main
async fn get_branch_diff(branch: &str) -> String {
    let output = tokio::process::Command::new("git")
        .args(["diff", &format!("main...{branch}"), "--stat"])
        .output()
        .await;

    match output {
        Ok(o) if o.status.success() => {
            let diff_stat = String::from_utf8_lossy(&o.stdout).to_string();

            // Also get the actual diff (limited)
            let full_diff = tokio::process::Command::new("git")
                .args(["diff", &format!("main...{branch}")])
                .output()
                .await;

            let full_diff_text = match full_diff {
                Ok(o) => {
                    let text = String::from_utf8_lossy(&o.stdout).to_string();
                    // Limit diff size
                    if text.len() > 10000 {
                        format!("{}...(truncated)", &text[..10000])
                    } else {
                        text
                    }
                }
                Err(_) => String::new(),
            };

            format!("{diff_stat}\n\n{full_diff_text}")
        }
        _ => "Unable to get diff".to_string(),
    }
}

async fn run_reviewer_lanes(
    smith: &dyn Smith,
    ingot_id: &str,
    branch: &str,
    diff: &str,
    ci_result: &CiResult,
) -> Result<Vec<LaneReviewResult>, SlagError> {
    let mut results = Vec::new();
    for lane in ReviewerLane::all() {
        let prompt =
            flux::prepare_reviewer_lane_flux(lane.as_str(), ingot_id, branch, diff, ci_result);
        let response = smith.invoke(&prompt).await?;
        let parsed = parse_lane_review_response(lane, &response);
        events::emit_info(
            "review.lane.result",
            "review lane completed",
            serde_json::json!({
                "ingot_id": ingot_id,
                "branch": branch,
                "lane": lane.as_str(),
                "passed": parsed.passed,
                "schema_valid": parsed.schema_valid,
                "fix_ingots": parsed.fix_ingots.len()
            }),
        );
        results.push(parsed);
    }
    Ok(results)
}

fn parse_lane_review_response(lane: ReviewerLane, raw: &str) -> LaneReviewResult {
    let mut status: Option<bool> = None;
    let mut evidence = String::new();
    let mut fix_declared_present = false;
    let mut fix_declared_none = false;
    let mut fix_ingots = Vec::new();

    for line in raw.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("STATUS:") {
            let token = rest.trim().to_ascii_uppercase();
            status = match token.as_str() {
                "PASS" => Some(true),
                "FAIL" => Some(false),
                _ => None,
            };
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("EVIDENCE:") {
            evidence = rest.trim().to_string();
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("FIX_INGOTS:") {
            let token = rest.trim().to_ascii_uppercase();
            fix_declared_present = token == "PRESENT";
            fix_declared_none = token == "NONE";
            continue;
        }
        if trimmed.starts_with("(ingot ") {
            if let Some(ingot) = parse_ingot(trimmed) {
                fix_ingots.push(ingot);
            }
        }
    }

    let mut schema_valid = status.is_some() && !evidence.is_empty();
    let mut passed = status.unwrap_or(false);

    if passed && fix_declared_present {
        schema_valid = false;
        passed = false;
    }
    if passed && !fix_ingots.is_empty() {
        schema_valid = false;
        passed = false;
    }
    if !passed && fix_declared_none {
        schema_valid = false;
    }
    if !passed && fix_declared_present && fix_ingots.is_empty() {
        schema_valid = false;
    }

    if !schema_valid && evidence.is_empty() {
        evidence = "review lane output did not satisfy required schema".to_string();
    }
    if !schema_valid {
        passed = false;
    }

    LaneReviewResult {
        lane,
        passed,
        schema_valid,
        evidence,
        fix_ingots,
    }
}

/// Master agent review via Smith
async fn master_review(
    smith: &dyn Smith,
    ingot_id: &str,
    branch: &str,
    diff: &str,
    ci_result: &CiResult,
) -> Result<ReviewResult, SlagError> {
    let prompt = flux::prepare_review_flux(ingot_id, branch, diff, ci_result);

    let response = smith.invoke(&prompt).await?;

    // Parse response
    let approved =
        response.contains("STATUS: APPROVED") || response.to_uppercase().contains("APPROVED");
    let rejected =
        response.contains("STATUS: REJECTED") || response.to_uppercase().contains("REJECTED");

    // Extract comments
    let comments = response
        .lines()
        .skip_while(|l| !l.starts_with("COMMENTS:") && !l.starts_with("Comments:"))
        .skip(1)
        .take(10)
        .collect::<Vec<&str>>()
        .join(" ");

    let approved = if rejected {
        false
    } else {
        approved || ci_result.passed()
    };

    Ok(ReviewResult {
        approved,
        comments: if comments.is_empty() {
            response.lines().take(3).collect::<Vec<&str>>().join(" ")
        } else {
            comments
        },
    })
}

/// Merge a branch back to main
async fn merge_branch(ingot_id: &str) -> Result<(), SlagError> {
    use crate::anvil::worktree;
    worktree::merge_and_cleanup(ingot_id).await
}

/// Clean up a branch without merging
async fn cleanup_branch(ingot_id: &str, keep: bool) {
    if keep {
        println!("    \x1b[90m↳ keeping branch for debugging\x1b[0m");
        return;
    }
    use crate::anvil::worktree;
    worktree::cleanup_and_delete_branch(ingot_id).await;
}

fn mark_ingot_cracked(id: &str) -> Result<(), SlagError> {
    let mut crucible = Crucible::load(Path::new(CRUCIBLE))?;
    crucible.set_status(id, Status::Cracked);
    crucible.save()?;
    Ok(())
}

fn queue_lane_fix_ingots(id: &str, lane_results: &[LaneReviewResult]) -> Result<usize, SlagError> {
    let mut pending: Vec<(ReviewerLane, Ingot)> = Vec::new();
    for result in lane_results.iter().filter(|r| !r.passed) {
        for ingot in &result.fix_ingots {
            pending.push((result.lane, ingot.clone()));
        }
    }
    if pending.is_empty() {
        return Ok(0);
    }

    let mut crucible = Crucible::load(Path::new(CRUCIBLE))?;
    let mut existing_ids: HashSet<String> = crucible.ingots.iter().map(|i| i.id.clone()).collect();
    let mut queued = 0usize;

    for (idx, (lane, mut ingot)) in pending.into_iter().enumerate() {
        if queued >= 8 {
            break;
        }
        if !is_concrete_review_ingot(&ingot) {
            continue;
        }

        ingot.status = Status::Ore;
        ingot.heat = 0;
        ingot.smelt = 0;
        ingot.solo = false;

        let base_id = format!("rv_{}_{}_{}", id, lane.as_str(), idx + 1);
        ingot.id = unique_id(base_id, &mut existing_ids);
        crucible.ingots.push(ingot);
        queued += 1;
    }

    if queued > 0 {
        crucible.save()?;
    }
    Ok(queued)
}

fn is_concrete_review_ingot(ingot: &Ingot) -> bool {
    let proof = ingot.proof.trim().to_ascii_lowercase();
    let work = ingot.work.trim().to_ascii_lowercase();
    if proof.is_empty() || work.is_empty() {
        return false;
    }
    if matches!(
        proof.as_str(),
        "true" | "shell" | "proof" | "cmd" | "command"
    ) {
        return false;
    }
    if matches!(work.as_str(), "task" | "todo" | "fix task") {
        return false;
    }
    true
}

fn unique_id(base: String, existing: &mut HashSet<String>) -> String {
    if existing.insert(base.clone()) {
        return base;
    }
    let mut n = 2usize;
    loop {
        let candidate = format!("{base}_{n}");
        if existing.insert(candidate.clone()) {
            return candidate;
        }
        n += 1;
    }
}

/// Print CI failure details
fn print_ci_failures(ci: &CiResult) {
    if !ci.fmt_passed {
        println!(
            "    \x1b[31m↳ fmt:\x1b[0m {}",
            tui::truncate(&ci.fmt_output, 50)
        );
    }
    if !ci.clippy_passed {
        println!(
            "    \x1b[31m↳ clippy:\x1b[0m {}",
            tui::truncate(&ci.clippy_output, 50)
        );
    }
    if !ci.test_passed {
        println!(
            "    \x1b[31m↳ test:\x1b[0m {}",
            tui::truncate(&ci.test_output, 50)
        );
    }
}

/// List all forge branches
#[allow(dead_code)]
pub async fn list_forge_branches() -> Vec<String> {
    let output = tokio::process::Command::new("git")
        .args(["branch", "--list", "forge/*"])
        .output()
        .await;

    match output {
        Ok(o) if o.status.success() => {
            let text = String::from_utf8_lossy(&o.stdout);
            text.lines()
                .map(|l| l.trim().trim_start_matches("* ").to_string())
                .filter(|s| !s.is_empty())
                .collect()
        }
        _ => Vec::new(),
    }
}
