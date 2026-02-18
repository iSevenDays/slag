use std::collections::HashSet;
use std::path::Path;

use crate::config::{BLUEPRINT, CRUCIBLE, LEDGER, ORE_FILE};
use crate::crucible::{self, Crucible};
use crate::error::SlagError;
use crate::flux;
use crate::proof;
use crate::sexp::{Ingot, Status};
use crate::smith::Smith;
use crate::tui;

const MAX_REPAIR_INGOTS: usize = 4;

/// Independent closing-loop validation focused on user-visible outcomes.
/// Returns Ok(true) when outcome passes, Ok(false) when repair ingots were queued.
pub async fn validate_and_queue(
    smith: &dyn Smith,
    cycle: usize,
    verbose: bool,
) -> Result<bool, SlagError> {
    tui::header("OUTCOME · independent validation");

    let ore = std::fs::read_to_string(ORE_FILE).unwrap_or_else(|_| "No commission".into());
    let blueprint = std::fs::read_to_string(BLUEPRINT).unwrap_or_else(|_| "No blueprint".into());
    let crucible = std::fs::read_to_string(CRUCIBLE).unwrap_or_else(|_| "No crucible".into());
    let ledger_tail = read_tail(LEDGER, 60);

    let prompt = flux::prepare_outcome_flux(&ore, &blueprint, &crucible, &ledger_tail);
    log_to_file("OUTCOME_PROMPT", &prompt);
    let requires_browser_test = likely_browser_outcome(&ore, &blueprint, &crucible);

    let spinner = tui::spinner("validating outcome...");
    let response = smith.invoke(&prompt).await.map_err(|e| {
        spinner.finish_and_clear();
        SlagError::OutcomeFailed(format!("validator invocation failed: {e}"))
    })?;
    spinner.finish_and_clear();

    log_to_file("OUTCOME_RAW", &response);

    let decision = parse_outcome_response(&response);
    let comment = if decision.comment.is_empty() {
        "no comment".to_string()
    } else {
        decision.comment.clone()
    };
    let test_cmd = decision.test_cmd.trim();

    if test_cmd.is_empty() {
        return Err(SlagError::OutcomeFailed(
            "validator did not provide a TEST command".into(),
        ));
    }

    println!(
        "  \x1b[90mTEST:\x1b[0m {}",
        if verbose {
            test_cmd.to_string()
        } else {
            tui::truncate(test_cmd, 80)
        }
    );
    let browser_shape_ok = !requires_browser_test || looks_like_browser_test(test_cmd);
    if requires_browser_test && !browser_shape_ok {
        println!(
            "  \x1b[31m✗\x1b[0m outcome TEST must be browser/runtime-aware for web/simulation outcomes"
        );
    }
    let (test_ok, test_output) = proof::run_shell(test_cmd).await;
    log_to_file(
        "OUTCOME_TEST",
        &format!(
            "cmd={}\nexit={}\n{}",
            test_cmd,
            if test_ok { 0 } else { 1 },
            test_output
        ),
    );

    if decision.passed && test_ok && browser_shape_ok {
        println!(
            "  \x1b[1;37m✓\x1b[0m outcome PASS: {}",
            if verbose {
                comment
            } else {
                tui::truncate(&comment, 90)
            }
        );
        return Ok(true);
    }

    println!(
        "  \x1b[31m✗\x1b[0m outcome FAIL: {}",
        if verbose {
            comment.clone()
        } else {
            tui::truncate(&comment, 90)
        }
    );
    if !test_ok {
        println!("  \x1b[31m✗\x1b[0m outcome TEST failed (exit 1)");
        if verbose {
            println!("  \x1b[90m{}\x1b[0m", tui::truncate(&test_output, 200));
        }
    }
    if requires_browser_test && !browser_shape_ok {
        println!("  \x1b[31m✗\x1b[0m validator TEST did not include browser checks");
    }

    if decision.repair_ingots.is_empty() {
        return Err(SlagError::OutcomeFailed(
            "validator returned FAIL without repair ingots".into(),
        ));
    }

    let mut crucible = Crucible::load(Path::new(CRUCIBLE))?;
    let added = append_repair_ingots(&mut crucible, decision.repair_ingots, cycle);
    crucible.save()?;

    if added == 0 {
        return Err(SlagError::OutcomeFailed(
            "validator returned no usable repair ingots".into(),
        ));
    }

    println!(
        "  \x1b[38;5;220m↺\x1b[0m queued {} outcome repair ingot(s)",
        added
    );
    Ok(false)
}

#[derive(Debug)]
struct OutcomeDecision {
    passed: bool,
    comment: String,
    test_cmd: String,
    repair_ingots: Vec<Ingot>,
}

fn parse_outcome_response(raw: &str) -> OutcomeDecision {
    let mut status: Option<bool> = None;
    let mut comment = String::new();
    let mut test_cmd = String::new();

    for line in raw.lines() {
        let trimmed = line.trim();
        let upper = trimmed.to_ascii_uppercase();

        if upper.starts_with("STATUS:") {
            let value = trimmed
                .split_once(':')
                .map(|(_, v)| v.trim().to_ascii_uppercase())
                .unwrap_or_default();
            if value.starts_with("PASS") {
                status = Some(true);
            } else if value.starts_with("FAIL") {
                status = Some(false);
            }
        } else if upper.starts_with("COMMENT:") && comment.is_empty() {
            comment = trimmed
                .split_once(':')
                .map(|(_, v)| v.trim().to_string())
                .unwrap_or_default();
        } else if upper.starts_with("TEST:") && test_cmd.is_empty() {
            test_cmd = trimmed
                .split_once(':')
                .map(|(_, v)| v.trim().to_string())
                .unwrap_or_default();
        }
    }

    let repair_ingots = crucible::parse_ingot_lines(raw);
    let passed = status.unwrap_or(false);

    OutcomeDecision {
        passed,
        comment,
        test_cmd,
        repair_ingots,
    }
}

fn append_repair_ingots(crucible: &mut Crucible, ingots: Vec<Ingot>, cycle: usize) -> usize {
    let mut existing_ids: HashSet<String> = crucible.ingots.iter().map(|i| i.id.clone()).collect();
    let mut added = 0;

    for (idx, mut ingot) in ingots.into_iter().take(MAX_REPAIR_INGOTS).enumerate() {
        let base_id = if ingot.id.trim().is_empty() {
            format!("v{cycle}_{}", idx + 1)
        } else {
            ingot.id.clone()
        };
        ingot.id = unique_ingot_id(&base_id, &mut existing_ids, cycle, idx + 1);
        ingot.status = Status::Ore;
        ingot.solo = false; // outcome repairs are integration fixes; run sequentially
        ingot.heat = 0;
        ingot.smelt = 0;
        if ingot.grade == 0 {
            ingot.grade = 2;
        }
        if ingot.max == 0 {
            ingot.max = 5;
        }
        if ingot.proof.trim().is_empty() {
            ingot.proof = "true".into();
        }
        if ingot.work.trim().is_empty() {
            ingot.work = "Repair outcome validation failure".into();
        }

        crucible.ingots.push(ingot);
        added += 1;
    }

    added
}

fn unique_ingot_id(
    preferred: &str,
    existing_ids: &mut HashSet<String>,
    cycle: usize,
    ordinal: usize,
) -> String {
    let sanitized: String = preferred
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();

    let base = if sanitized.is_empty() {
        format!("v{cycle}_{ordinal}")
    } else {
        sanitized
    };

    if existing_ids.insert(base.clone()) {
        return base;
    }

    let mut n = 2usize;
    loop {
        let candidate = format!("{base}_{n}");
        if existing_ids.insert(candidate.clone()) {
            return candidate;
        }
        n += 1;
    }
}

fn read_tail(path: &str, lines: usize) -> String {
    match std::fs::read_to_string(path) {
        Ok(content) => {
            let all_lines: Vec<&str> = content.lines().collect();
            let start = all_lines.len().saturating_sub(lines);
            all_lines[start..].join("\n")
        }
        Err(_) => "Fresh".into(),
    }
}

fn likely_browser_outcome(ore: &str, blueprint: &str, crucible: &str) -> bool {
    let text = format!(
        "{}\n{}\n{}",
        ore.to_lowercase(),
        blueprint.to_lowercase(),
        crucible.to_lowercase()
    );
    text.contains(":skill web")
        || text.contains("web")
        || text.contains("browser")
        || text.contains("frontend")
        || text.contains("three.js")
        || text.contains("3d")
        || text.contains("simulation")
        || text.contains("game")
        || text.contains("canvas")
}

fn looks_like_browser_test(cmd: &str) -> bool {
    let c = cmd.to_lowercase();
    c.contains("playwright")
        || c.contains("chromium")
        || c.contains("puppeteer")
        || c.contains("cypress")
        || c.contains("selenium")
        || c.contains("page.goto")
        || c.contains("web-test")
        || c.contains("headless")
}

fn log_to_file(label: &str, content: &str) {
    let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let path = format!("{}/{ts}_{label}.log", crate::config::LOG_DIR);
    let _ = std::fs::write(&path, content);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_outcome_pass() {
        let raw = "STATUS: PASS\nCOMMENT: behavior looks correct\nTEST: echo ok\n";
        let decision = parse_outcome_response(raw);
        assert!(decision.passed);
        assert_eq!(decision.comment, "behavior looks correct");
        assert_eq!(decision.test_cmd, "echo ok");
        assert!(decision.repair_ingots.is_empty());
    }

    #[test]
    fn parse_outcome_fail_with_repair() {
        let raw = r#"
STATUS: FAIL
COMMENT: runtime behavior is missing
TEST: npm test
(ingot :id "v1" :status ore :solo nil :grade 2 :skill web :heat 0 :max 5 :smelt 0 :proof "npm test" :work "Add runtime fix")
"#;
        let decision = parse_outcome_response(raw);
        assert!(!decision.passed);
        assert_eq!(decision.test_cmd, "npm test");
        assert_eq!(decision.repair_ingots.len(), 1);
        assert_eq!(decision.repair_ingots[0].id, "v1");
    }

    #[test]
    fn append_repair_ingots_makes_ids_unique() {
        let mut crucible = Crucible::new(
            Path::new("PLAN.md"),
            vec![Ingot {
                id: "v1".into(),
                status: Status::Forged,
                solo: false,
                grade: 1,
                skill: crate::sexp::Skill::Default,
                heat: 0,
                max: 5,
                smelt: 0,
                proof: "true".into(),
                work: "existing".into(),
                extra: vec![],
            }],
        );

        let repairs = vec![Ingot {
            id: "v1".into(),
            status: Status::Ore,
            solo: true,
            grade: 0,
            skill: crate::sexp::Skill::Default,
            heat: 9,
            max: 0,
            smelt: 9,
            proof: "".into(),
            work: "".into(),
            extra: vec![],
        }];

        let added = append_repair_ingots(&mut crucible, repairs, 2);
        assert_eq!(added, 1);
        assert_eq!(crucible.ingots.len(), 2);
        assert_ne!(crucible.ingots[1].id, "v1");
        assert_eq!(crucible.ingots[1].status, Status::Ore);
        assert!(!crucible.ingots[1].solo);
        assert_eq!(crucible.ingots[1].grade, 2);
        assert_eq!(crucible.ingots[1].max, 5);
        assert_eq!(crucible.ingots[1].heat, 0);
        assert_eq!(crucible.ingots[1].smelt, 0);
        assert_eq!(crucible.ingots[1].proof, "true");
    }

    #[test]
    fn browser_outcome_detection() {
        assert!(likely_browser_outcome(
            "Build 3d simulation",
            "Uses Three.js and canvas",
            "(ingot :skill web)"
        ));
        assert!(!likely_browser_outcome(
            "Build CLI tool",
            "No UI",
            "(ingot :skill default)"
        ));
    }

    #[test]
    fn browser_test_shape_detection() {
        assert!(looks_like_browser_test("npx playwright test"));
        assert!(looks_like_browser_test(
            "node -e \"const { chromium } = require('playwright')\""
        ));
        assert!(!looks_like_browser_test("npm test"));
    }
}
