use std::collections::{HashMap, HashSet};
use std::time::Duration;

use crate::config::{BLUEPRINT, CRUCIBLE, MAX_ITERATE, ORE_FILE};
use crate::crucible::{self, Crucible};
use crate::error::SlagError;
use crate::flux;
use crate::sexp::Ingot;
use crate::smith::{self, Smith};
use crate::tui;

/// Phase 2: Read blueprint and produce S-expression ingots in PLAN.md
pub async fn run(
    smith: &dyn Smith,
    verbose: bool,
    confidence_threshold: f32,
) -> Result<(), SlagError> {
    tui::header("FOUNDER · casting mold");

    let ore = std::fs::read_to_string(ORE_FILE).map_err(|_| SlagError::NoOre)?;
    let blueprint = std::fs::read_to_string(BLUEPRINT).unwrap_or_else(|_| "No blueprint".into());

    let prompt = flux::founder_prompt(&ore, &blueprint);
    log_to_file("FOUNDER_PROMPT", &prompt);

    let spinner = tui::spinner("casting...");
    let raw = smith.invoke(&prompt).await.map_err(|e| {
        spinner.finish_and_clear();
        SlagError::FounderFailed(e.to_string())
    })?;
    spinner.finish_and_clear();

    log_to_file("FOUNDER_RAW", &raw);

    // Self-iterate if questions
    let mut raw = smith::self_iterate(smith, raw, MAX_ITERATE).await?;
    let mut ingots = sanitize_founder_ingots(crucible::parse_ingot_lines(&raw));
    let mut format_retries = 0usize;

    // Recovery path: some models return prose/XML despite strict format instructions.
    for attempt in 1..=MAX_ITERATE {
        if !ingots.is_empty() {
            break;
        }
        format_retries += 1;
        tui::status_line(
            "↺",
            tui::COLD,
            &format!("Founder format retry {attempt}/{MAX_ITERATE}"),
        );
        let repair_prompt = flux::founder_recast_prompt(&ore, &blueprint, &raw);
        log_to_file(&format!("FOUNDER_RECAST_PROMPT_{attempt}"), &repair_prompt);
        let retry_spinner = tui::spinner("re-casting...");
        let retry_raw = smith.invoke(&repair_prompt).await.map_err(|e| {
            retry_spinner.finish_and_clear();
            SlagError::FounderFailed(e.to_string())
        })?;
        retry_spinner.finish_and_clear();
        log_to_file(&format!("FOUNDER_RECAST_RAW_{attempt}"), &retry_raw);

        raw = smith::self_iterate(smith, retry_raw, MAX_ITERATE).await?;
        ingots = sanitize_founder_ingots(crucible::parse_ingot_lines(&raw));
    }

    let mut confidence = founder_confidence(&raw, &ingots, format_retries);
    println!(
        "  \x1b[90mconfidence:\x1b[0m {:.2} (threshold {:.2})",
        confidence, confidence_threshold
    );
    log_to_file(
        "FOUNDER_CONFIDENCE",
        &format!(
            "confidence={:.3}\nthreshold={:.3}\ningots={}\nretries={}",
            confidence,
            confidence_threshold,
            ingots.len(),
            format_retries
        ),
    );

    if ingots.is_empty() || confidence < confidence_threshold {
        tui::status_line(
            "↺",
            tui::COLD,
            if ingots.is_empty() {
                "Founder produced no ingots; escalating to subagent"
            } else {
                "Founder confidence below threshold; escalating to subagent"
            },
        );
        if let Some((handoff_raw, handoff_ingots)) =
            try_subagent_founder(&ore, &blueprint, &raw).await
        {
            raw = handoff_raw;
            ingots = sanitize_founder_ingots(handoff_ingots);
            confidence = founder_confidence(&raw, &ingots, 0);
            println!(
                "  \x1b[90mconfidence (subagent):\x1b[0m {:.2} (threshold {:.2})",
                confidence, confidence_threshold
            );
            log_to_file(
                "FOUNDER_CONFIDENCE_SUBAGENT",
                &format!(
                    "confidence={:.3}\nthreshold={:.3}\ningots={}",
                    confidence,
                    confidence_threshold,
                    ingots.len()
                ),
            );
        }
    }

    if ingots.is_empty() {
        return Err(SlagError::NoIngots);
    }
    if confidence < confidence_threshold {
        tui::status_line(
            "↺",
            tui::COLD,
            "Founder confidence still low after escalation; proceeding with caution",
        );
    }

    // Create crucible
    let crucible_path = std::path::PathBuf::from(CRUCIBLE);
    let crucible = Crucible::new(&crucible_path, ingots.clone());
    crucible.save()?;

    // Stats
    let count = ingots.len();
    let simple = ingots.iter().filter(|i| !i.is_complex()).count();
    let complex = ingots.iter().filter(|i| i.is_complex()).count();
    let web = ingots.iter().filter(|i| i.is_web()).count();

    tui::status_line(
        "█",
        tui::PURE,
        &format!("Mold: {count} ingots ({simple} simple, {complex} complex, {web} web)"),
    );

    // Show table
    println!();
    println!("  \x1b[90m{:<5} {:<10} WORK\x1b[0m", "ID", "STATUS");
    let preview_rows = if verbose { 10 } else { 6 };
    for (i, ingot) in ingots.iter().enumerate() {
        if i >= preview_rows {
            break;
        }
        let status_display = match ingot.status {
            crate::sexp::Status::Ore => "\x1b[90m🧱 ore\x1b[0m",
            crate::sexp::Status::Molten => "\x1b[38;5;208m🔥 hot\x1b[0m",
            crate::sexp::Status::Forged => "✅ done",
            crate::sexp::Status::Cracked => "\x1b[31m❌ fail\x1b[0m",
        };
        println!(
            "  \x1b[38;5;208m{:<5}\x1b[0m {:<10} {}",
            ingot.id,
            status_display,
            tui::truncate(&ingot.work, 55),
        );
    }
    if count > preview_rows {
        println!(
            "  \x1b[90m+{} more{}\x1b[0m",
            count - preview_rows,
            if verbose {
                ""
            } else {
                " (use --verbose for longer preview)"
            }
        );
    }

    Ok(())
}

fn log_to_file(label: &str, content: &str) {
    let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let path = format!("{}/{ts}_{label}.log", crate::config::LOG_DIR);
    let _ = std::fs::write(&path, content);
}

fn founder_confidence(raw: &str, ingots: &[Ingot], format_retries: usize) -> f32 {
    let mut score = 0.0f32;
    let count = ingots.len();

    if count > 0 {
        score += 0.35;
    }
    if (2..=24).contains(&count) {
        score += 0.20;
    } else if count == 1 {
        score += 0.10;
    } else if count > 24 {
        score += 0.12;
    }

    if count > 0 {
        let valid_fields = ingots
            .iter()
            .filter(|i| {
                !i.id.trim().is_empty()
                    && !i.work.trim().is_empty()
                    && !i.proof.trim().is_empty()
                    && i.max > 0
            })
            .count();
        score += 0.25 * (valid_fields as f32 / count as f32);

        let mut seen = HashSet::new();
        let mut unique = 0usize;
        for ingot in ingots {
            if seen.insert(ingot.id.clone()) {
                unique += 1;
            }
        }
        score += 0.10 * (unique as f32 / count as f32);
    }

    let raw_lower = raw.to_ascii_lowercase();
    if raw.contains("```") || raw_lower.contains("<xml") || raw_lower.contains("<output") {
        score -= 0.10;
    }
    score -= (format_retries.min(3) as f32) * 0.08;
    score.clamp(0.0, 1.0)
}

fn sanitize_founder_ingots(parsed: Vec<Ingot>) -> Vec<Ingot> {
    let mut sanitized = Vec::new();
    let mut assigned: HashSet<String> = HashSet::new();
    let mut seen_base: HashMap<String, usize> = HashMap::new();

    for mut ingot in parsed {
        if !is_concrete_proof(&ingot.proof) || !is_concrete_work(&ingot.work) {
            continue;
        }

        let mut base = ingot.id.trim().to_string();
        if base.is_empty() {
            base = "i_auto".to_string();
        }
        let seen_count = seen_base.entry(base.clone()).or_insert(0);
        *seen_count += 1;

        let id = if *seen_count == 1 && assigned.insert(base.clone()) {
            base
        } else {
            let mut n = (*seen_count).max(2);
            let mut candidate = format!("{base}_{n}");
            while assigned.contains(&candidate) {
                n += 1;
                candidate = format!("{base}_{n}");
            }
            assigned.insert(candidate.clone());
            candidate
        };

        ingot.id = id;
        ingot.status = crate::sexp::Status::Ore;
        ingot.heat = 0;
        ingot.smelt = 0;
        if ingot.max == 0 {
            ingot.max = 5;
        }
        if ingot.grade == 0 {
            ingot.grade = 1;
        }
        sanitized.push(ingot);
    }

    sanitized
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
    if matches!(lower.as_str(), "task" | "todo" | "tbd" | "sub-task") {
        return false;
    }
    true
}

fn subagent_command() -> String {
    crate::config::subagent_smith_command_from_env()
}

fn subagent_timeout_secs() -> u64 {
    std::env::var("SLAG_SUBAGENT_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(90)
}

async fn try_subagent_founder(
    ore: &str,
    blueprint: &str,
    previous_raw: &str,
) -> Option<(String, Vec<crate::sexp::Ingot>)> {
    let subagent = match crate::smith::build_smith(&subagent_command()) {
        Ok(s) => s,
        Err(e) => {
            tui::status_line(
                "↺",
                tui::COLD,
                &format!("Subagent founder build failed: {e}"),
            );
            return None;
        }
    };
    let prompt = format!(
        "{}\n\n[SUBAGENT ESCALATION]\n\
        Primary founder returned no valid ingots after retries.\n\
        Return ONLY valid `(ingot ...)` S-expression lines. No prose.",
        flux::founder_recast_prompt(ore, blueprint, previous_raw)
    );
    log_to_file("FOUNDER_SUBAGENT_PROMPT", &prompt);

    let raw = match tokio::time::timeout(
        Duration::from_secs(subagent_timeout_secs()),
        subagent.invoke(&prompt),
    )
    .await
    {
        Ok(Ok(raw)) => raw,
        Ok(Err(e)) => {
            tui::status_line(
                "↺",
                tui::COLD,
                &format!("Subagent founder handoff failed: {e}"),
            );
            return None;
        }
        Err(_) => {
            tui::status_line(
                "↺",
                tui::COLD,
                "Subagent founder handoff timed out; keeping original founder output",
            );
            return None;
        }
    };

    let raw = match smith::self_iterate(&*subagent, raw, MAX_ITERATE).await {
        Ok(v) => v,
        Err(e) => {
            tui::status_line(
                "↺",
                tui::COLD,
                &format!("Subagent founder self-iterate failed: {e}"),
            );
            return None;
        }
    };
    log_to_file("FOUNDER_SUBAGENT_RAW", &raw);
    let ingots = crucible::parse_ingot_lines(&raw);
    if ingots.is_empty() {
        tui::status_line(
            "↺",
            tui::COLD,
            "Subagent founder handoff returned no ingots",
        );
        return None;
    }
    Some((raw, ingots))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::smith::mock::MockSmith;
    use crate::sexp::{Skill, Status};

    fn sample_ingot(id: &str) -> Ingot {
        Ingot {
            id: id.to_string(),
            status: Status::Ore,
            solo: true,
            grade: 2,
            skill: Skill::Default,
            heat: 0,
            max: 5,
            smelt: 0,
            proof: "cargo test --all".to_string(),
            work: "Implement feature".to_string(),
            budget: None,
            extra: vec![],
        }
    }

    #[test]
    fn founder_confidence_is_low_with_no_ingots() {
        let score = founder_confidence("no ingots", &[], 2);
        assert!(score < 0.30);
    }

    #[test]
    fn founder_confidence_is_high_with_valid_ingots() {
        let ingots = vec![sample_ingot("i1"), sample_ingot("i2"), sample_ingot("i3")];
        let score = founder_confidence("(ingot ...)", &ingots, 0);
        assert!(score > 0.70);
    }

    #[test]
    fn sanitize_founder_ingots_drops_placeholders() {
        let mut bad = sample_ingot("i1");
        bad.proof = "SHELL".to_string();
        let mut bad_work = sample_ingot("i2");
        bad_work.work = "task".to_string();

        let clean = sanitize_founder_ingots(vec![bad, bad_work]);
        assert!(clean.is_empty());
    }

    #[test]
    fn sanitize_founder_ingots_uniquifies_ids_and_normalizes() {
        let mut a = sample_ingot("r1");
        a.status = Status::Forged;
        a.heat = 9;
        a.smelt = 9;
        a.max = 0;
        a.grade = 0;

        let mut b = sample_ingot("r1");
        b.work = "Second work item".to_string();

        let out = sanitize_founder_ingots(vec![a, b]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].id, "r1");
        assert_eq!(out[1].id, "r1_2");
        assert_eq!(out[0].status, Status::Ore);
        assert_eq!(out[0].heat, 0);
        assert_eq!(out[0].smelt, 0);
        assert_eq!(out[0].max, 5);
        assert_eq!(out[0].grade, 1);
    }

    // Characterization tests: capture the format-retry loop behavior so refactoring
    // to bounded_retry can be verified as behavior-preserving.

    /// When the smith returns valid ingot S-expressions on the first attempt,
    /// no format retries should occur (the loop breaks immediately).
    #[tokio::test]
    async fn format_retry_loop_succeeds_first_attempt_no_retries() {
        // Simulate: first smith response already contains valid ingot lines.
        // The loop guard `if !ingots.is_empty() { break; }` fires immediately.
        let raw = "(ingot :id \"i1\" :status ore :solo t :grade 1 :skill default \
                   :heat 0 :max 5 :proof \"cargo test\" :work \"Add tests\")";
        let ingots = crucible::parse_ingot_lines(raw);
        assert!(!ingots.is_empty(), "setup: raw should parse to at least one ingot");

        let sanitized = sanitize_founder_ingots(ingots);
        assert_eq!(sanitized.len(), 1, "one valid ingot should survive sanitization");
        assert_eq!(sanitized[0].id, "i1");
        // format_retries stays 0, confidence is based on the sanitized output
        let confidence = founder_confidence(raw, &sanitized, 0);
        assert!(confidence > 0.70, "clean first-attempt output should yield high confidence");
    }

    /// When the smith returns prose on attempt 0 (no ingots parsed),
    /// the loop retries up to MAX_ITERATE times.  Each retry calls smith.invoke once.
    /// After exhaustion, ingots remain empty.
    #[tokio::test]
    async fn format_retry_loop_exhausts_on_all_prose_responses() {
        // All responses are prose — no ingots are ever parsed.
        let prose = "Here is my analysis of the requirements...".to_string();
        let smith = MockSmith::fixed(&prose);

        // Simulate the loop body: initial call + up to MAX_ITERATE retries.
        // We don't call founder::run (requires filesystem), so we exercise
        // the loop logic directly using the same primitives.
        let mut ingots = crucible::parse_ingot_lines(&prose);
        let mut format_retries = 0usize;

        for attempt in 1..=MAX_ITERATE {
            if !ingots.is_empty() {
                break;
            }
            format_retries += 1;
            let repair_prompt = flux::founder_recast_prompt("ore text", "blueprint text", &prose);
            let retry_raw = smith.invoke(&repair_prompt).await.unwrap();
            let iterated = smith::self_iterate(&smith, retry_raw, MAX_ITERATE).await.unwrap();
            ingots = sanitize_founder_ingots(crucible::parse_ingot_lines(&iterated));
            let _ = attempt; // used only to match real loop variable
        }

        assert!(ingots.is_empty(), "prose-only responses should leave ingots empty");
        assert_eq!(format_retries, MAX_ITERATE, "all MAX_ITERATE retries should be consumed");
        // smith was called at least MAX_ITERATE times (one per retry)
        assert!(smith.call_count() >= MAX_ITERATE);
    }

    /// Characterization: the format-retry loop calls smith.invoke at least once per
    /// retry attempt, and self_iterate may consume additional slots in the cycling mock.
    /// When all responses are prose (no ingots), the loop exhausts all MAX_ITERATE
    /// retries and the smith is called at least MAX_ITERATE times total.
    /// This documents the current behavior so any refactor can be verified against it.
    #[tokio::test]
    async fn format_retry_loop_characterization_calls_smith_per_retry() {
        // All responses are prose — no ingots ever parsed, so loop runs all MAX_ITERATE attempts.
        let prose = "Here is a description without any ingots.".to_string();
        let smith = MockSmith::fixed(&prose);

        let mut ingots = crucible::parse_ingot_lines(&prose);
        let mut format_retries = 0usize;
        let mut final_raw = prose.clone();

        for _attempt in 1..=MAX_ITERATE {
            if !ingots.is_empty() {
                break;
            }
            format_retries += 1;
            let repair_prompt =
                flux::founder_recast_prompt("ore text", "blueprint text", &final_raw);
            let retry_raw = smith.invoke(&repair_prompt).await.unwrap();
            let iterated = smith::self_iterate(&smith, retry_raw, MAX_ITERATE).await.unwrap();
            final_raw = iterated.clone();
            ingots = sanitize_founder_ingots(crucible::parse_ingot_lines(&iterated));
        }

        // All prose → loop exhausts every retry slot
        assert_eq!(format_retries, MAX_ITERATE, "all retries consumed on prose-only responses");
        // self_iterate doesn't call smith when there are no questions (prose without '?'),
        // so smith.call_count == MAX_ITERATE (one per retry attempt).
        assert_eq!(
            smith.call_count(),
            MAX_ITERATE,
            "smith called exactly once per retry attempt when self_iterate is a noop"
        );
        assert!(ingots.is_empty(), "no ingots should have been parsed from prose");
    }

    /// Confidence degrades with each format retry consumed.
    #[tokio::test]
    async fn founder_confidence_degrades_linearly_with_retries() {
        let ingots = vec![sample_ingot("i1"), sample_ingot("i2")];
        let raw = "(ingot ...)";
        let score_0 = founder_confidence(raw, &ingots, 0);
        let score_1 = founder_confidence(raw, &ingots, 1);
        let score_2 = founder_confidence(raw, &ingots, 2);
        assert!(score_0 > score_1, "0 retries should score higher than 1");
        assert!(score_1 > score_2, "1 retry should score higher than 2");
    }
}
