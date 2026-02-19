use std::collections::HashSet;
use std::path::Path;

use crate::config::{PipelineConfig, PromptPolicy, SmithConfig, CRUCIBLE};
use crate::crucible::Crucible;
use crate::error::SlagError;
use crate::events;
use crate::flux;
use crate::prompt;
use crate::sexp::{Ingot, Status};
use crate::smith::Smith;
use crate::tui;

/// Failure pattern detected during analysis
#[derive(Debug, Clone)]
pub enum FailurePattern {
    /// File/directory not found - dependency ordering issue
    MissingDependency { file: String },
    /// CMD: line missing from smith output - protocol failure
    ProtocolFailure,
    /// Proof command failed - work/proof mismatch
    ProofMismatch,
    /// Unknown failure
    Unknown,
}

/// Analysis result for a cracked ingot
#[derive(Debug)]
pub struct CrackedAnalysis {
    pub id: String,
    pub pattern: FailurePattern,
    pub recommendation: AnalysisAction,
}

/// Recommended action from analysis
#[derive(Debug, Clone)]
pub enum AnalysisAction {
    /// Reset to ore and retry (simple retry)
    Retry,
    /// Mark as sequential (was parallel but has dependencies)
    MakeSequential,
    /// Needs founder to regenerate
    Regenerate,
    /// Truly impossible, skip
    Skip,
}

/// Analyze cracked ingots and prepare for retry
pub async fn analyze_and_prepare(
    smith: &dyn Smith,
    _config: &SmithConfig,
    pipeline_config: &PipelineConfig,
    cycle: usize,
) -> Result<bool, SlagError> {
    let crucible_path = Path::new(CRUCIBLE);
    let mut crucible = Crucible::load(crucible_path)?;
    let counts = crucible.counts();

    if counts.cracked == 0 {
        return Ok(false); // Nothing to analyze
    }
    events::emit_info(
        "analysis.start",
        "analyzing cracked ingots",
        serde_json::json!({
            "cycle": cycle,
            "cracked": counts.cracked
        }),
    );

    tui::header(&format!("ANALYSIS · retry cycle {}", cycle));

    println!(
        "  \x1b[38;5;208m⚗\x1b[0m Analyzing {} cracked ingots...",
        counts.cracked
    );

    // Gather failure logs and analyze each cracked ingot
    let cracked_ids: Vec<String> = crucible
        .ingots
        .iter()
        .filter(|i| i.status == Status::Cracked)
        .map(|i| i.id.clone())
        .collect();

    let mut analyses: Vec<CrackedAnalysis> = Vec::new();

    for id in &cracked_ids {
        if let Some(ingot) = crucible.get(id) {
            let pattern = detect_failure_pattern(ingot);
            let recommendation = recommend_action(&pattern, ingot);
            println!(
                "    \x1b[90m[{}]\x1b[0m {:?} → {:?}",
                id, pattern, recommendation
            );
            analyses.push(CrackedAnalysis {
                id: id.clone(),
                pattern,
                recommendation,
            });
        }
    }

    // Check if we should regenerate via founder or just retry
    let needs_regenerate = analyses
        .iter()
        .any(|a| matches!(a.recommendation, AnalysisAction::Regenerate));

    if needs_regenerate {
        // Use AI to regenerate the failed portion
        println!(
            "\n  \x1b[38;5;220m♻\x1b[0m Regenerating {} cracked ingots via founder...",
            cracked_ids.len()
        );
        regenerate_cracked(smith, &mut crucible, &cracked_ids).await?;
    } else {
        // Apply fixes and reset to ore
        for analysis in &analyses {
            match analysis.recommendation {
                AnalysisAction::Retry => {
                    // Reset to ore, clear heat and smelt
                    if let Some(ingot) = crucible.get_mut(&analysis.id) {
                        ingot.status = Status::Ore;
                        ingot.heat = 0;
                        ingot.smelt = 0;
                    }
                    println!("    \x1b[38;5;220m↺\x1b[0m [{}] reset to ore", analysis.id);
                }
                AnalysisAction::MakeSequential => {
                    // Mark as sequential and reset
                    if let Some(ingot) = crucible.get_mut(&analysis.id) {
                        ingot.status = Status::Ore;
                        ingot.heat = 0;
                        ingot.smelt = 0;
                        ingot.solo = false; // Make sequential
                    }
                    println!(
                        "    \x1b[38;5;220m↺\x1b[0m [{}] reset to ore (now sequential)",
                        analysis.id
                    );
                }
                AnalysisAction::Skip => {
                    println!(
                        "    \x1b[31m✗\x1b[0m [{}] skipped (truly impossible)",
                        analysis.id
                    );
                }
                AnalysisAction::Regenerate => {
                    // Already handled above
                }
            }
        }
    }

    crucible.save()?;

    // Check if we have any ore to forge
    let new_counts = crucible.counts();
    let has_work = new_counts.ore > 0;

    if has_work {
        println!(
            "\n  \x1b[38;5;220m⚒\x1b[0m Ready to retry: {} ingots reset to ore",
            new_counts.ore
        );
        Ok(true)
    } else {
        println!("\n  \x1b[31m✗\x1b[0m No recoverable ingots");
        events::emit_warn(
            "analysis.no_recoverable",
            "no recoverable ingots after analysis",
            serde_json::json!({
                "cracked": counts.cracked
            }),
        );

        // Ask user if they want to force retry all cracked ingots
        if ask_force_retry(
            counts.cracked,
            pipeline_config.prompt_policy,
            pipeline_config.prompt_timeout_secs,
        )
        .await?
        {
            let mut crucible = Crucible::load(crucible_path)?;
            for id in &cracked_ids {
                if let Some(ingot) = crucible.get_mut(id) {
                    ingot.status = Status::Ore;
                    ingot.heat = 0;
                    // Keep smelt count to track attempts
                }
            }
            crucible.save()?;
            println!(
                "\n  \x1b[38;5;220m⚒\x1b[0m Force retry: {} ingots reset to ore",
                counts.cracked
            );
            events::emit_warn(
                "analysis.force_retry",
                "operator forced retry for cracked ingots",
                serde_json::json!({
                    "cracked": counts.cracked
                }),
            );
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

/// Detect the failure pattern for a cracked ingot by reading logs
fn detect_failure_pattern(ingot: &Ingot) -> FailurePattern {
    let log_dir = Path::new(crate::config::LOG_DIR);

    // Collect all matching log files (sorted by time, newest first)
    let mut matching_logs: Vec<_> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(log_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            // Match logs for this ingot (ASSAY, STRIKE, FLUX)
            if name.contains(&ingot.id) {
                if let Ok(content) = std::fs::read_to_string(entry.path()) {
                    matching_logs.push((name, content));
                }
            }
        }
    }

    // Analyze all logs for this ingot
    for (_name, content) in &matching_logs {
        // Check for dependency/file issues
        if content.contains("No such file or directory")
            || content.contains("file not found")
            || content.contains("does not exist")
            || content.contains("ENOENT")
            || content.contains("cannot open")
        {
            return FailurePattern::MissingDependency {
                file: extract_missing_file(content).unwrap_or_else(|| "unknown".to_string()),
            };
        }

        // Check for JSON/parsing issues (common with data tasks)
        if content.contains("parse error")
            || content.contains("invalid JSON")
            || content.contains("jq:")
            || content.contains("SyntaxError")
        {
            return FailurePattern::ProofMismatch;
        }

        // Check for protocol failures
        if content.contains("NO CMD:") || content.contains("missing \"CMD:\"") {
            return FailurePattern::ProtocolFailure;
        }

        // Check for proof failures
        if content.contains("proof failed") || content.contains("non-zero exit") {
            return FailurePattern::ProofMismatch;
        }
    }

    // No logs found or no patterns matched - try to infer from ingot properties
    // If proof involves file operations and ingot is parallel, it may need sequencing
    let proof = ingot.proof.to_lowercase();
    if ingot.solo
        && (proof.contains("test -f")
            || proof.contains("test -d")
            || proof.contains("cat ")
            || proof.contains("jq "))
    {
        // Likely depends on a file that should be created by another ingot
        return FailurePattern::MissingDependency {
            file: extract_file_from_proof(&ingot.proof).unwrap_or_else(|| "unknown".to_string()),
        };
    }

    FailurePattern::Unknown
}

/// Extract file path from a proof command
fn extract_file_from_proof(proof: &str) -> Option<String> {
    // Look for patterns like "test -f FILE" or "jq . FILE" or "cat FILE"
    let parts: Vec<&str> = proof.split_whitespace().collect();
    for (i, part) in parts.iter().enumerate() {
        if (*part == "-f" || *part == "-d" || *part == "." || *part == "-e") && i + 1 < parts.len()
        {
            let file = parts[i + 1];
            if !file.starts_with('-') && !file.starts_with('|') {
                return Some(file.to_string());
            }
        }
        // Last argument is often the file
        if i == parts.len() - 1
            && !part.starts_with('-')
            && !part.starts_with('|')
            && part.contains('/')
        {
            return Some(part.to_string());
        }
    }
    None
}

/// Extract the missing file name from error output
fn extract_missing_file(content: &str) -> Option<String> {
    // Look for patterns like "test -f FILE" or "No such file: FILE"
    for line in content.lines() {
        if line.contains("No such file") {
            if let Some(pos) = line.rfind(':') {
                return Some(line[pos + 1..].trim().to_string());
            }
        }
        if line.contains("test -f") || line.contains("test -d") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 3 {
                return Some(parts[2].to_string());
            }
        }
    }
    None
}

/// Recommend action based on failure pattern
fn recommend_action(pattern: &FailurePattern, ingot: &Ingot) -> AnalysisAction {
    match pattern {
        FailurePattern::MissingDependency { .. } => {
            // If it was parallel, make it sequential
            if ingot.solo {
                AnalysisAction::MakeSequential
            } else {
                // Already sequential, needs regeneration
                AnalysisAction::Regenerate
            }
        }
        FailurePattern::ProtocolFailure => {
            // Smith didn't follow protocol - retry with reset
            if ingot.smelt >= 2 {
                AnalysisAction::Regenerate
            } else {
                AnalysisAction::Retry
            }
        }
        FailurePattern::ProofMismatch => {
            // Proof/work mismatch - needs regeneration if already re-smelted
            if ingot.smelt >= 1 {
                AnalysisAction::Regenerate
            } else {
                AnalysisAction::Retry
            }
        }
        FailurePattern::Unknown => {
            // Unknown failure - be aggressive with retries
            // Only skip if truly exhausted (smelt >= 3 means reconsidered twice)
            if ingot.smelt >= 3 {
                AnalysisAction::Skip
            } else if ingot.smelt >= 2 {
                // Already reconsidered once, regenerate with fresh approach
                AnalysisAction::Regenerate
            } else {
                AnalysisAction::Retry
            }
        }
    }
}

/// Ask user if they want to force retry all cracked ingots
async fn ask_force_retry(
    cracked_count: usize,
    policy: PromptPolicy,
    timeout_secs: u64,
) -> Result<bool, SlagError> {
    match policy {
        PromptPolicy::AutoRequeue => {
            events::emit_info(
                "prompt.analysis.auto_requeue",
                "applied prompt policy auto-requeue (force retry)",
                serde_json::json!({
                    "cracked_count": cracked_count
                }),
            );
            return Ok(true);
        }
        PromptPolicy::AutoCrack => {
            events::emit_info(
                "prompt.analysis.auto_crack",
                "applied prompt policy auto-crack (no force retry)",
                serde_json::json!({
                    "cracked_count": cracked_count
                }),
            );
            return Ok(false);
        }
        PromptPolicy::AutoAbort => {
            events::emit_warn(
                "prompt.analysis.auto_abort",
                "applied prompt policy auto-abort",
                serde_json::json!({
                    "cracked_count": cracked_count
                }),
            );
            return Err(SlagError::OperatorAbort(
                "analysis retry decision was auto-aborted by prompt policy".to_string(),
            ));
        }
        PromptPolicy::Ask => {}
    }

    if !prompt::stdin_is_interactive() {
        events::emit_debug(
            "prompt.analysis.non_interactive",
            "stdin is not interactive, defaulting to no-retry",
            serde_json::json!({
                "cracked_count": cracked_count
            }),
        );
        return Ok(false);
    }

    use std::io::{self, Write};

    print!(
        "\n  \x1b[38;5;220m?\x1b[0m Force retry {} cracked ingots? [y/N/a] ",
        cracked_count
    );
    io::stdout().flush().unwrap();

    let Some(input) = prompt::read_line_timeout(timeout_secs) else {
        events::emit_warn(
            "prompt.analysis.timeout",
            "force-retry prompt timed out, defaulted to no-retry",
            serde_json::json!({
                "timeout_secs": timeout_secs,
                "cracked_count": cracked_count
            }),
        );
        tui::status_line(
            "↺",
            tui::COLD,
            &format!(
                "No operator input after {}s, defaulting to no-retry",
                timeout_secs
            ),
        );
        return Ok(false);
    };

    let trimmed = input.trim().to_lowercase();
    if trimmed == "a" || trimmed == "abort" {
        events::emit_warn(
            "prompt.analysis.abort",
            "operator aborted at force-retry prompt",
            serde_json::json!({
                "cracked_count": cracked_count
            }),
        );
        return Err(SlagError::OperatorAbort(
            "analysis retry decision aborted by operator".to_string(),
        ));
    }
    events::emit_info(
        "prompt.analysis.choice",
        "operator selected force-retry decision",
        serde_json::json!({
            "choice": trimmed,
            "cracked_count": cracked_count
        }),
    );
    Ok(trimmed == "y" || trimmed == "yes")
}

/// Regenerate cracked ingots using the founder
async fn regenerate_cracked(
    smith: &dyn Smith,
    crucible: &mut Crucible,
    cracked_ids: &[String],
) -> Result<(), SlagError> {
    // Build a prompt describing what needs to be regenerated
    let cracked_descriptions: Vec<String> = cracked_ids
        .iter()
        .filter_map(|id| crucible.get(id))
        .map(|i| format!("[{}] {}", i.id, i.work))
        .collect();

    let prompt = flux::regenerate_prompt(&cracked_descriptions.join("\n"));

    let spinner = tui::spinner("regenerating...");
    let response = smith.invoke(&prompt).await.map_err(|e| {
        spinner.finish_and_clear();
        SlagError::FounderFailed(e.to_string())
    })?;
    spinner.finish_and_clear();

    // Parse new ingots from response
    let parsed_ingots = crate::crucible::parse_ingot_lines(&response);
    let (new_ingots, dropped) = sanitize_regenerated_ingots(crucible, cracked_ids, parsed_ingots);

    if !dropped.is_empty() {
        println!(
            "    \x1b[38;5;214m⚠\x1b[0m dropped {} malformed regeneration candidate(s)",
            dropped.len()
        );
        for reason in dropped.iter().take(5) {
            println!("      \x1b[90m- {reason}\x1b[0m");
        }
        if dropped.len() > 5 {
            println!("      \x1b[90m... +{} more\x1b[0m", dropped.len() - 5);
        }
    }

    if new_ingots.is_empty() {
        println!("    \x1b[31m✗\x1b[0m could not regenerate valid ingots");
        reset_cracked_to_ore(crucible, cracked_ids);
        return Ok(());
    }

    println!(
        "    \x1b[38;5;220m♻\x1b[0m generated {} replacement ingots",
        new_ingots.len()
    );

    // Remove all cracked ingots
    for id in cracked_ids {
        crucible.ingots.retain(|i| i.id != *id);
    }

    // Add new ingots (they're already marked as ore with smelt=0)
    for ingot in new_ingots {
        println!(
            "    \x1b[38;5;220m+\x1b[0m [{}] {}",
            ingot.id,
            tui::truncate(&ingot.work, 50)
        );
        crucible.ingots.push(ingot);
    }

    Ok(())
}

fn reset_cracked_to_ore(crucible: &mut Crucible, cracked_ids: &[String]) {
    for id in cracked_ids {
        if let Some(ingot) = crucible.get_mut(id) {
            ingot.status = Status::Ore;
            ingot.heat = 0;
            ingot.smelt = 0;
            ingot.solo = false; // Make sequential to be safe
        }
    }
}

fn sanitize_regenerated_ingots(
    crucible: &Crucible,
    cracked_ids: &[String],
    parsed: Vec<Ingot>,
) -> (Vec<Ingot>, Vec<String>) {
    let cracked_set: HashSet<String> = cracked_ids.iter().cloned().collect();
    let mut existing_ids: HashSet<String> = crucible
        .ingots
        .iter()
        .filter(|ingot| !cracked_set.contains(&ingot.id))
        .map(|ingot| ingot.id.clone())
        .collect();

    let mut accepted = Vec::new();
    let mut dropped = Vec::new();
    let mut seen = HashSet::new();

    for mut ingot in parsed {
        let id = ingot.id.trim().to_string();
        if id.is_empty() {
            dropped.push("empty ingot id".to_string());
            continue;
        }

        if !seen.insert(id.clone()) {
            dropped.push(format!("[{id}] duplicate id in regeneration output"));
            continue;
        }

        if existing_ids.contains(&id) {
            dropped.push(format!(
                "[{id}] collides with existing non-cracked ingot id"
            ));
            continue;
        }

        if ingot.status != Status::Ore {
            dropped.push(format!(
                "[{id}] must be :status ore, got {:?}",
                ingot.status
            ));
            continue;
        }

        if !is_concrete_proof(&ingot.proof) {
            dropped.push(format!(
                "[{id}] invalid :proof (empty/placeholder): {}",
                tui::truncate(&ingot.proof, 40)
            ));
            continue;
        }

        if !is_concrete_work(&ingot.work) {
            dropped.push(format!(
                "[{id}] invalid :work (empty/placeholder): {}",
                tui::truncate(&ingot.work, 40)
            ));
            continue;
        }

        ingot.id = id.clone();
        ingot.status = Status::Ore;
        ingot.heat = 0;
        ingot.smelt = 0;
        if ingot.max == 0 {
            ingot.max = 5;
        }
        if ingot.grade == 0 {
            ingot.grade = 1;
        }

        existing_ids.insert(id);
        accepted.push(ingot);
    }

    let max_regenerated = cracked_ids.len().saturating_mul(6).max(1);
    if accepted.len() > max_regenerated {
        dropped.push(format!(
            "too many regenerated ingots ({} > max {})",
            accepted.len(),
            max_regenerated
        ));
        return (Vec::new(), dropped);
    }

    (accepted, dropped)
}

fn is_concrete_proof(proof: &str) -> bool {
    let trimmed = proof.trim();
    if trimmed.is_empty() {
        return false;
    }
    let lower = trimmed.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "true" | "shell" | "proof" | "cmd" | "command" | "n/a"
    ) {
        return false;
    }
    !lower.contains("<shell")
}

fn is_concrete_work(work: &str) -> bool {
    let trimmed = work.trim();
    if trimmed.is_empty() {
        return false;
    }
    let lower = trimmed.to_ascii_lowercase();
    if matches!(lower.as_str(), "task" | "todo" | "tbd") {
        return false;
    }
    !lower.starts_with("sub-task")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sexp::{Ingot, Skill};
    use std::path::PathBuf;

    fn ingot(id: &str, status: Status, proof: &str, work: &str) -> Ingot {
        Ingot {
            id: id.to_string(),
            status,
            solo: true,
            grade: 1,
            skill: Skill::Default,
            heat: 0,
            max: 5,
            smelt: 0,
            proof: proof.to_string(),
            work: work.to_string(),
            extra: vec![],
        }
    }

    fn crucible_with(ingots: Vec<Ingot>) -> Crucible {
        Crucible::new(PathBuf::from("PLAN.md").as_path(), ingots)
    }

    #[test]
    fn sanitize_regenerated_rejects_placeholders_and_collisions() {
        let crucible = crucible_with(vec![
            ingot("i1", Status::Forged, "test -f a", "done"),
            ingot("i2", Status::Cracked, "test -f b", "retry me"),
        ]);
        let cracked = vec!["i2".to_string()];
        let parsed = vec![
            ingot("i1", Status::Ore, "test -f x", "collision"),
            ingot("r1", Status::Ore, "SHELL", "Task"),
            ingot("r2", Status::Forged, "echo ok", "wrong status"),
        ];

        let (accepted, dropped) = sanitize_regenerated_ingots(&crucible, &cracked, parsed);
        assert!(accepted.is_empty());
        assert_eq!(dropped.len(), 3);
    }

    #[test]
    fn sanitize_regenerated_accepts_valid_entries_and_normalizes() {
        let crucible = crucible_with(vec![
            ingot("i1", Status::Forged, "test -f a", "done"),
            ingot("i2", Status::Cracked, "test -f b", "retry me"),
        ]);
        let cracked = vec!["i2".to_string()];
        let mut candidate = ingot(
            "r1",
            Status::Ore,
            "node --check src/main.js",
            "Rewire startup",
        );
        candidate.heat = 3;
        candidate.smelt = 9;
        candidate.max = 0;
        candidate.grade = 0;

        let (accepted, dropped) = sanitize_regenerated_ingots(&crucible, &cracked, vec![candidate]);
        assert!(dropped.is_empty());
        assert_eq!(accepted.len(), 1);
        assert_eq!(accepted[0].heat, 0);
        assert_eq!(accepted[0].smelt, 0);
        assert_eq!(accepted[0].max, 5);
        assert_eq!(accepted[0].grade, 1);
    }
}
