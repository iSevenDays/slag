use std::collections::HashSet;
use std::path::Path;

use crate::crucible::Crucible;
use crate::error::SlagError;
use crate::flux;
use crate::sexp::parser::parse_crucible;
use crate::sexp::{Ingot, Status};
use crate::smith::Smith;
use crate::tui;

/// Attempt to repair a cracked ingot via re-smelt or reconsider.
/// Uses strict retry contract validation and optional independent fallback lane.
pub async fn resmelt_ingot(
    crucible: &mut Crucible,
    ingot: &Ingot,
    smith: &dyn Smith,
    independent_smith: Option<&dyn Smith>,
) -> Result<(), SlagError> {
    if ingot.smelt >= 2 {
        println!("    \x1b[31m⚠\x1b[0m already reconsidered, truly cracked");
        return Err(SlagError::IngotCracked(ingot.id.clone(), ingot.max));
    }

    let mode = if ingot.smelt >= 1 {
        RepairMode::Reconsider
    } else {
        RepairMode::Resmelt
    };

    show_mode_header(mode, &ingot.id);

    let failure_logs = gather_failure_logs(&ingot.id);
    let prompt = build_prompt(mode, ingot, &failure_logs);
    log_to_file(&format!("{}_{}", mode.prompt_label(), ingot.id), &prompt);

    let new_ingots = generate_repair_candidates(
        mode,
        ingot,
        &prompt,
        &failure_logs,
        smith,
        independent_smith,
    )
    .await?;

    if new_ingots.len() == 1 {
        println!(
            "    \x1b[38;5;220m{}\x1b[0m {}",
            mode.icon(),
            mode.success_verb(&new_ingots[0].work),
        );
    } else {
        println!(
            "    \x1b[38;5;220m{}\x1b[0m {} {} sub-ingots",
            mode.icon(),
            mode.split_verb(),
            new_ingots.len()
        );
    }

    crucible.replace(&ingot.id, new_ingots);
    Ok(())
}

#[derive(Clone, Copy)]
enum RepairMode {
    Resmelt,
    Reconsider,
}

impl RepairMode {
    fn expected_smelt(self) -> u8 {
        match self {
            Self::Resmelt => 1,
            Self::Reconsider => 2,
        }
    }

    fn prompt_label(self) -> &'static str {
        match self {
            Self::Resmelt => "RESMELT",
            Self::Reconsider => "RECONSIDER",
        }
    }

    fn result_label(self) -> &'static str {
        match self {
            Self::Resmelt => "RESMELT_RESULT",
            Self::Reconsider => "RECONSIDER_RESULT",
        }
    }

    fn spinner_label(self) -> &'static str {
        match self {
            Self::Resmelt => "re-smelting...",
            Self::Reconsider => "reconsidering...",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Self::Resmelt => "♻",
            Self::Reconsider => "⚖",
        }
    }

    fn success_verb(self, work: &str) -> String {
        let label = match self {
            Self::Resmelt => "rewritten",
            Self::Reconsider => "rethought",
        };
        format!("{label}: {}", tui::truncate(work, 50))
    }

    fn split_verb(self) -> &'static str {
        match self {
            Self::Resmelt => "split into",
            Self::Reconsider => "decomposed into",
        }
    }
}

fn show_mode_header(mode: RepairMode, id: &str) {
    match mode {
        RepairMode::Resmelt => println!(
            "\n  \x1b[38;5;208m♻\x1b[0m \x1b[1;37mRE-SMELTING [{id}]\x1b[0m — analyzing failure..."
        ),
        RepairMode::Reconsider => println!(
            "\n  \x1b[38;5;220m⚖\x1b[0m \x1b[1;37mRECONSIDERING [{id}]\x1b[0m — rethinking approach..."
        ),
    }
}

fn build_prompt(mode: RepairMode, ingot: &Ingot, failure_logs: &str) -> String {
    match mode {
        RepairMode::Resmelt => flux::prepare_resmelt_flux(ingot, failure_logs),
        RepairMode::Reconsider => flux::prepare_reconsider_flux(ingot, failure_logs),
    }
}

async fn generate_repair_candidates(
    mode: RepairMode,
    ingot: &Ingot,
    prompt: &str,
    failure_logs: &str,
    smith: &dyn Smith,
    independent_smith: Option<&dyn Smith>,
) -> Result<Vec<Ingot>, SlagError> {
    let failed_signatures = collect_failed_signatures(failure_logs);
    let expected_smelt = mode.expected_smelt();

    let primary = attempt_lane(
        mode,
        Lane::Primary,
        smith,
        prompt,
        ingot,
        expected_smelt,
        &failed_signatures,
    )
    .await;

    if let Ok(ingots) = primary {
        return Ok(ingots);
    }

    let primary_reason = primary.unwrap_err();
    println!(
        "    \x1b[31m✗\x1b[0m primary repair rejected: {}",
        tui::truncate(&primary_reason, 80)
    );

    if let Some(independent) = independent_smith {
        println!(
            "    \x1b[38;5;220m⇄\x1b[0m escalating [{}] to independent lane",
            ingot.id
        );
        let fallback_prompt = format!(
            "{prompt}\n\n\
            PREVIOUS OUTPUT REJECTED:\n\
            {primary_reason}\n\n\
            RETRY DIRECTIVE:\n\
            - Use a different approach than the rejected output\n\
            - Do not repeat previous proof command patterns\n\
            - Follow the output format exactly\n"
        );

        let independent_result = attempt_lane(
            mode,
            Lane::Independent,
            independent,
            &fallback_prompt,
            ingot,
            expected_smelt,
            &failed_signatures,
        )
        .await;

        if let Ok(ingots) = independent_result {
            return Ok(ingots);
        }

        let independent_reason = independent_result.unwrap_err();
        println!(
            "    \x1b[31m✗\x1b[0m independent repair rejected: {}",
            tui::truncate(&independent_reason, 80)
        );
        return Err(SlagError::IngotCracked(ingot.id.clone(), ingot.max));
    }

    Err(SlagError::IngotCracked(ingot.id.clone(), ingot.max))
}

#[derive(Clone, Copy)]
enum Lane {
    Primary,
    Independent,
}

impl Lane {
    fn label(self) -> &'static str {
        match self {
            Self::Primary => "PRIMARY",
            Self::Independent => "INDEPENDENT",
        }
    }
}

async fn attempt_lane(
    mode: RepairMode,
    lane: Lane,
    smith: &dyn Smith,
    prompt: &str,
    ingot: &Ingot,
    expected_smelt: u8,
    failed_signatures: &HashSet<String>,
) -> Result<Vec<Ingot>, String> {
    let spinner = tui::spinner(mode.spinner_label());
    let response = smith
        .invoke(prompt)
        .await
        .map_err(|e| format!("smith invoke failed: {e}"))?;
    spinner.finish_and_clear();

    log_to_file(
        &format!("{}_{}_{}", mode.result_label(), ingot.id, lane.label()),
        &response,
    );

    if response.contains("IMPOSSIBLE:") {
        let reason = response
            .lines()
            .find(|l| l.starts_with("IMPOSSIBLE:"))
            .map(|l| l.strip_prefix("IMPOSSIBLE:").unwrap_or("").trim())
            .unwrap_or("unknown");
        return Err(format!("declared impossible: {reason}"));
    }

    let ingots = parse_crucible(&response);
    if ingots.is_empty() {
        return Err("no ingot output parsed".into());
    }

    validate_retry_contract(ingot, &ingots, expected_smelt, failed_signatures)?;
    Ok(ingots)
}

fn validate_retry_contract(
    original: &Ingot,
    candidates: &[Ingot],
    expected_smelt: u8,
    failed_signatures: &HashSet<String>,
) -> Result<(), String> {
    if candidates.is_empty() {
        return Err("empty retry output".into());
    }

    let base_work = normalize_signature(&original.work);
    let base_proof = normalize_signature(&original.proof);
    let mut changed = false;
    let mut ids = HashSet::new();

    for candidate in candidates {
        if !ids.insert(candidate.id.clone()) {
            return Err(format!(
                "duplicate ingot id in retry output: {}",
                candidate.id
            ));
        }

        if candidate.status != Status::Ore {
            return Err(format!(
                "ingot {} must be :status ore, got {}",
                candidate.id,
                candidate.status.as_str()
            ));
        }

        if candidate.smelt != expected_smelt {
            return Err(format!(
                "ingot {} must have :smelt {}, got {}",
                candidate.id, expected_smelt, candidate.smelt
            ));
        }

        if candidate.work.trim().is_empty() {
            return Err(format!("ingot {} has empty :work", candidate.id));
        }

        if candidate.proof.trim().is_empty() || candidate.proof.trim() == "true" {
            return Err(format!(
                "ingot {} must provide concrete :proof (not empty/true)",
                candidate.id
            ));
        }

        let proof_sig = normalize_signature(&candidate.proof);
        if failed_signatures.contains(&proof_sig) {
            return Err(format!(
                "ingot {} reuses failed proof/cmd signature: {}",
                candidate.id, candidate.proof
            ));
        }

        let work_sig = normalize_signature(&candidate.work);
        if work_sig != base_work || proof_sig != base_proof {
            changed = true;
        }
    }

    if !changed {
        return Err("retry output did not change approach (same :work/:proof)".into());
    }

    Ok(())
}

fn collect_failed_signatures(logs: &str) -> HashSet<String> {
    let mut signatures = HashSet::new();

    for line in logs.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Proof:") {
            push_signature(&mut signatures, rest);
        }

        if let Some(rest) = trimmed.strip_prefix("CMD:") {
            push_signature(&mut signatures, rest);
        }

        if let Some(start) = trimmed.find("Proof failed [") {
            let bracketed = &trimmed[start + "Proof failed [".len()..];
            if let Some(end) = bracketed.find(']') {
                push_signature(&mut signatures, &bracketed[..end]);
            }
        }
    }

    signatures
}

fn push_signature(signatures: &mut HashSet<String>, raw: &str) {
    let cleaned = raw.trim().trim_matches('`');
    let normalized = normalize_signature(cleaned);
    if !normalized.is_empty() {
        signatures.insert(normalized);
    }
}

fn normalize_signature(value: &str) -> String {
    let squashed = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut normalized = squashed.to_lowercase();
    while normalized.contains("\\\\") {
        normalized = normalized.replace("\\\\", "\\");
    }
    normalized.trim().to_string()
}

fn gather_failure_logs(id: &str) -> String {
    let log_dir = Path::new(crate::config::LOG_DIR);
    let mut logs = String::new();

    if let Ok(entries) = std::fs::read_dir(log_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.contains(id) {
                if let Ok(content) = std::fs::read_to_string(entry.path()) {
                    let lines: Vec<&str> = content.lines().collect();
                    let tail: Vec<&str> = lines.iter().rev().take(50).rev().copied().collect();
                    logs.push_str(&format!("--- {name} ---\n"));
                    logs.push_str(&tail.join("\n"));
                    logs.push('\n');
                }
            }
        }
    }

    if logs.is_empty() {
        "No failure logs found".into()
    } else {
        logs
    }
}

fn log_to_file(label: &str, content: &str) {
    let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let path = format!("{}/{ts}_{label}.log", crate::config::LOG_DIR);
    let _ = std::fs::write(&path, content);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sexp::Skill;

    fn ingot_with(work: &str, proof: &str, smelt: u8) -> Ingot {
        Ingot {
            id: "i9".to_string(),
            status: Status::Ore,
            solo: true,
            grade: 2,
            skill: Skill::Default,
            heat: 0,
            max: 5,
            smelt,
            proof: proof.to_string(),
            work: work.to_string(),
            budget: None,
            extra: vec![],
        }
    }

    #[test]
    fn retry_contract_rejects_unchanged_rewrite() {
        let original = ingot_with("Verify page title", "grep -qF 'CRACKS' index.html", 0);
        let candidate = ingot_with("Verify page title", "grep -qF 'CRACKS' index.html", 1);
        let err = validate_retry_contract(&original, &[candidate], 1, &HashSet::new()).unwrap_err();
        assert!(err.contains("did not change approach"));
    }

    #[test]
    fn retry_contract_rejects_failed_signature_reuse() {
        let original = ingot_with("Verify page title", "grep -qF 'CRACKS' index.html", 0);
        let candidate = ingot_with("Retry proof", "grep -q 'CRACKS\\|canvas' index.html", 1);
        let mut failed = HashSet::new();
        failed.insert(normalize_signature("grep -q 'CRACKS\\|canvas' index.html"));
        let err = validate_retry_contract(&original, &[candidate], 1, &failed).unwrap_err();
        assert!(err.contains("reuses failed proof"));
    }

    #[test]
    fn retry_contract_accepts_novel_rewrite() {
        let original = ingot_with("Verify page title", "grep -qF 'CRACKS' index.html", 0);
        let candidate = ingot_with(
            "Check runtime canvas and title",
            "node scripts/web_smoke.js",
            1,
        );
        assert!(validate_retry_contract(&original, &[candidate], 1, &HashSet::new()).is_ok());
    }

    #[test]
    fn collect_failed_signatures_extracts_proof_and_cmd() {
        let logs = "\
Proof: curl -sf http://localhost:5175/ | grep -q 'CRACKS\\\\|canvas'\n\
CMD: `curl -sf http://localhost:5175/ | grep -qF 'CRACKS'`\n\
Proof failed [curl -sf http://localhost:5175/ | grep -q 'CRACKS\\\\|canvas']:";
        let signatures = collect_failed_signatures(logs);
        assert!(signatures.contains(&normalize_signature(
            "curl -sf http://localhost:5175/ | grep -q 'CRACKS\\|canvas'"
        )));
        assert!(signatures.contains(&normalize_signature(
            "curl -sf http://localhost:5175/ | grep -qF 'CRACKS'"
        )));
    }
}
