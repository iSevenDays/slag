use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::config::{BLUEPRINT, CRUCIBLE, LEDGER, MAX_ITERATE, ORE_FILE};
use crate::crucible::{self, Crucible};
use crate::error::SlagError;
use crate::flux;
use crate::proof;
use crate::sexp::{Ingot, Skill, Status};
use crate::smith::claude::ClaudeSmith;
use crate::smith::Smith;
use crate::tui;

const MAX_REPAIR_INGOTS: usize = 4;
const DEFAULT_OUTCOME_VALIDATOR_TIMEOUT_SECS: u64 = 180;
const DEFAULT_SUBAGENT_TIMEOUT_SECS: u64 = 90;
const OUTCOME_SCREENSHOT_PATH: &str = "logs/outcome-smoke.png";
const DETERMINISTIC_WEB_SMOKE_SCRIPT: &str = "scripts/outcome_web_smoke.js";

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
    let validator_timeout_secs = std::env::var("SLAG_OUTCOME_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_OUTCOME_VALIDATOR_TIMEOUT_SECS);

    let (spinner, spinner_progress) =
        start_timeout_spinner("validating outcome", validator_timeout_secs);
    let mut recast_allowed = true;
    let response = match tokio::time::timeout(
        Duration::from_secs(validator_timeout_secs),
        smith.invoke(&prompt),
    )
    .await
    {
        Ok(Ok(response)) => response,
        Ok(Err(e)) => {
            recast_allowed = false;
            tui::status_line(
                "↺",
                tui::COLD,
                &format!("Outcome validator unavailable ({e}); using fallback fail path"),
            );
            "STATUS: FAIL\nCOMMENT: outcome validator invocation failed\nTEST:\n".into()
        }
        Err(_) => {
            recast_allowed = false;
            tui::status_line(
                "↺",
                tui::COLD,
                &format!(
                    "Outcome validator timed out after {validator_timeout_secs}s; using fallback fail path"
                ),
            );
            "STATUS: FAIL\nCOMMENT: outcome validator timed out\nTEST:\n".into()
        }
    };
    stop_timeout_spinner(&spinner, spinner_progress);

    log_to_file("OUTCOME_RAW", &response);
    let mut response = response;
    let mut decision = parse_outcome_response(&response);
    let mut format_retries = 0usize;
    for attempt in 1..=MAX_ITERATE {
        if !recast_allowed {
            break;
        }
        if decision.status_known && !decision.test_cmd.trim().is_empty() {
            break;
        }
        format_retries += 1;
        tui::status_line(
            "↺",
            tui::COLD,
            &format!("Outcome format retry {attempt}/{MAX_ITERATE}"),
        );
        let recast_prompt =
            flux::prepare_outcome_recast_flux(&ore, &blueprint, &crucible, &ledger_tail, &response);
        log_to_file(&format!("OUTCOME_RECAST_PROMPT_{attempt}"), &recast_prompt);
        let (retry_spinner, retry_progress) =
            start_timeout_spinner("re-validating", validator_timeout_secs);
        let retry_raw = match tokio::time::timeout(
            Duration::from_secs(validator_timeout_secs),
            smith.invoke(&recast_prompt),
        )
        .await
        {
            Ok(Ok(raw)) => raw,
            Ok(Err(e)) => {
                stop_timeout_spinner(&retry_spinner, retry_progress);
                tui::status_line(
                    "↺",
                    tui::COLD,
                    &format!("Outcome format retry failed ({e}); continuing with fallback TEST"),
                );
                break;
            }
            Err(_) => {
                stop_timeout_spinner(&retry_spinner, retry_progress);
                tui::status_line(
                    "↺",
                    tui::COLD,
                    &format!(
                        "Outcome format retry timed out after {validator_timeout_secs}s; continuing with fallback TEST"
                    ),
                );
                break;
            }
        };
        stop_timeout_spinner(&retry_spinner, retry_progress);
        log_to_file(&format!("OUTCOME_RECAST_RAW_{attempt}"), &retry_raw);
        response = retry_raw;
        decision = parse_outcome_response(&response);
    }

    let malformed_twice = format_retries >= 2;
    let mut comment = resolve_comment(&decision);
    let (mut test_cmd, mut used_fallback_test) = resolve_test_cmd(&decision, requires_browser_test);
    let mut weak_pass_conflict =
        decision.passed && looks_like_weak_test(&test_cmd, requires_browser_test);

    if malformed_twice || weak_pass_conflict {
        let reason = if malformed_twice && weak_pass_conflict {
            "malformed outcome response retries and weak PASS test"
        } else if malformed_twice {
            "malformed outcome response retries"
        } else {
            "PASS text conflicts with weak TEST"
        };
        if let Some(subagent_raw) = try_outcome_subagent(
            &ore,
            &blueprint,
            &crucible,
            &ledger_tail,
            &response,
            reason,
            verbose,
        )
        .await
        {
            response = subagent_raw;
            decision = parse_outcome_response(&response);
            comment = resolve_comment(&decision);
            let (candidate_cmd, candidate_used_fallback) =
                resolve_test_cmd(&decision, requires_browser_test);
            test_cmd = candidate_cmd;
            used_fallback_test = candidate_used_fallback;
            weak_pass_conflict =
                decision.passed && looks_like_weak_test(&test_cmd, requires_browser_test);
        }
    }

    if used_fallback_test {
        println!("  \x1b[38;5;220m↺\x1b[0m validator did not provide TEST; using fallback command");
    }

    let mut used_deterministic_smoke = false;
    if requires_browser_test
        && (used_fallback_test
            || !looks_like_browser_test(&test_cmd)
            || weak_pass_conflict
            || malformed_twice)
    {
        if let Some(smoke_cmd) = deterministic_web_smoke_cmd(&test_cmd) {
            used_deterministic_smoke = true;
            test_cmd = smoke_cmd;
            tui::status_line(
                "↺",
                tui::COLD,
                "Using deterministic web smoke fallback TEST",
            );
        }
    }

    println!(
        "  \x1b[90mTEST:\x1b[0m {}",
        if verbose {
            test_cmd.to_string()
        } else {
            tui::truncate(&test_cmd, 80)
        }
    );
    let browser_shape_ok = !requires_browser_test || looks_like_browser_test(&test_cmd);
    if requires_browser_test && !browser_shape_ok {
        println!(
            "  \x1b[31m✗\x1b[0m outcome TEST must be browser/runtime-aware for web/simulation outcomes"
        );
    }
    if requires_browser_test {
        let _ = std::fs::remove_file(OUTCOME_SCREENSHOT_PATH);
    }

    let test_cmd_to_run = if requires_browser_test {
        format!("SLAG_OUTCOME_SCREENSHOT=\"{OUTCOME_SCREENSHOT_PATH}\" {test_cmd}")
    } else {
        test_cmd.clone()
    };
    let (test_ok, test_output) = proof::run_shell(&test_cmd_to_run).await;
    let screenshot_ok =
        !requires_browser_test || screenshot_artifact_ok(Path::new(OUTCOME_SCREENSHOT_PATH));
    let evidence = extract_outcome_evidence(&test_output, OUTCOME_SCREENSHOT_PATH, screenshot_ok);
    log_to_file(
        "OUTCOME_TEST",
        &format!(
            "cmd={}\nexec_cmd={}\nexit={}\n{}",
            test_cmd,
            test_cmd_to_run,
            if test_ok { 0 } else { 1 },
            test_output
        ),
    );

    if decision.passed && test_ok && browser_shape_ok && screenshot_ok {
        println!(
            "  \x1b[1;37m✓\x1b[0m outcome PASS: {}",
            if verbose {
                comment
            } else {
                tui::truncate(&comment, 90)
            }
        );
        if requires_browser_test {
            println!(
                "  \x1b[90mevidence:\x1b[0m screenshot={} metric={} console_errors={}{}",
                evidence.screenshot,
                evidence.metric_display(),
                evidence.console_errors_display(),
                if used_deterministic_smoke {
                    " (deterministic smoke)"
                } else {
                    ""
                }
            );
        }
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
    if requires_browser_test && !screenshot_ok {
        println!(
            "  \x1b[31m✗\x1b[0m outcome screenshot missing at {}",
            OUTCOME_SCREENSHOT_PATH
        );
    }

    let mut repair_ingots = decision.repair_ingots;
    if repair_ingots.is_empty() {
        println!(
            "  \x1b[38;5;220m↺\x1b[0m validator returned FAIL without repair ingots; queuing synthetic repair"
        );
        repair_ingots.push(synthetic_repair_ingot(
            &test_cmd,
            &comment,
            requires_browser_test,
        ));
    }

    let mut crucible = Crucible::load(Path::new(CRUCIBLE))?;
    let added = append_repair_ingots(&mut crucible, repair_ingots, cycle);
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
    status_known: bool,
    comment: String,
    test_cmd: String,
    repair_ingots: Vec<Ingot>,
}

#[derive(Debug, Clone)]
struct OutcomeEvidence {
    screenshot: String,
    metric_label: Option<String>,
    metric_value: Option<i64>,
    console_errors: Option<usize>,
}

impl OutcomeEvidence {
    fn metric_display(&self) -> String {
        match (&self.metric_label, self.metric_value) {
            (Some(label), Some(value)) => format!("{label}:{value}"),
            _ => "unknown".to_string(),
        }
    }

    fn console_errors_display(&self) -> String {
        self.console_errors
            .map(|v| v.to_string())
            .unwrap_or_else(|| "unknown".to_string())
    }
}

fn resolve_comment(decision: &OutcomeDecision) -> String {
    if decision.comment.is_empty() {
        "no comment".to_string()
    } else {
        decision.comment.clone()
    }
}

fn resolve_test_cmd(decision: &OutcomeDecision, requires_browser_test: bool) -> (String, bool) {
    let mut test_cmd = decision.test_cmd.trim().to_string();
    let mut used_fallback_test = false;
    if test_cmd.is_empty() {
        if let Ok(current_crucible) = Crucible::load(Path::new(CRUCIBLE)) {
            if let Some(fallback) = fallback_outcome_test(&current_crucible, requires_browser_test)
            {
                test_cmd = fallback;
                used_fallback_test = true;
            }
        }
    }
    if test_cmd.is_empty() {
        test_cmd = "false".into();
        used_fallback_test = true;
    }
    (test_cmd, used_fallback_test)
}

fn start_timeout_spinner(
    label: &str,
    timeout_secs: u64,
) -> (indicatif::ProgressBar, tokio::task::JoinHandle<()>) {
    let spinner = tui::spinner(&format!("{label} (0/{timeout_secs}s)..."));
    let spinner_clone = spinner.clone();
    let label = label.to_string();
    let started = Instant::now();
    let task = tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let elapsed = started.elapsed().as_secs().min(timeout_secs);
            spinner_clone.set_message(format!("{label} ({elapsed}/{timeout_secs}s)..."));
            if elapsed >= timeout_secs {
                break;
            }
        }
    });
    (spinner, task)
}

fn stop_timeout_spinner(spinner: &indicatif::ProgressBar, task: tokio::task::JoinHandle<()>) {
    task.abort();
    spinner.finish_and_clear();
}

fn subagent_command() -> String {
    std::env::var("SLAG_SMITH_SUBAGENT")
        .unwrap_or_else(|_| "npx -y @anthropic-ai/claude-code -p".to_string())
}

fn subagent_timeout_secs() -> u64 {
    std::env::var("SLAG_SUBAGENT_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_SUBAGENT_TIMEOUT_SECS)
}

async fn try_outcome_subagent(
    ore: &str,
    blueprint: &str,
    crucible: &str,
    ledger_tail: &str,
    previous_raw: &str,
    reason: &str,
    verbose: bool,
) -> Option<String> {
    let subagent = ClaudeSmith::new(subagent_command());
    let prompt = format!(
        "{}\n\n\
        [SUBAGENT ESCALATION]\n\
        Reason: {reason}\n\
        Previous validator output:\n{previous_raw}\n\n\
        Return ONLY STATUS/COMMENT/TEST (+ optional repair ingots) in exact required format.\n\
        Do not ask questions.",
        flux::prepare_outcome_recast_flux(ore, blueprint, crucible, ledger_tail, previous_raw)
    );
    log_to_file("OUTCOME_SUBAGENT_PROMPT", &prompt);
    tui::status_line("↺", tui::COLD, "Escalating outcome validation to subagent");

    let (spinner, progress) = start_timeout_spinner("subagent validating", subagent_timeout_secs());
    let raw = match tokio::time::timeout(
        Duration::from_secs(subagent_timeout_secs()),
        subagent.invoke(&prompt),
    )
    .await
    {
        Ok(Ok(raw)) => raw,
        Ok(Err(e)) => {
            stop_timeout_spinner(&spinner, progress);
            tui::status_line("↺", tui::COLD, &format!("Subagent validation failed: {e}"));
            return None;
        }
        Err(_) => {
            stop_timeout_spinner(&spinner, progress);
            tui::status_line("↺", tui::COLD, "Subagent validation timed out");
            return None;
        }
    };
    stop_timeout_spinner(&spinner, progress);
    log_to_file("OUTCOME_SUBAGENT_RAW", &raw);
    if verbose {
        tui::status_line(
            "↺",
            tui::COLD,
            &format!("Subagent validator output size: {} bytes", raw.len()),
        );
    }
    Some(raw)
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

    if comment.is_empty() {
        comment = raw
            .lines()
            .map(str::trim)
            .find(|line| {
                !line.is_empty()
                    && !line.to_ascii_uppercase().starts_with("ASK:")
                    && !line.starts_with('🟩')
            })
            .unwrap_or_default()
            .to_string();
    }
    if status.is_none() {
        status = infer_status_from_text(raw);
    }
    if test_cmd.is_empty() {
        test_cmd = infer_test_from_text(raw);
    }

    let repair_ingots = crucible::parse_ingot_lines(raw);
    let status_known = status.is_some();
    let passed = status.unwrap_or(false);

    OutcomeDecision {
        passed,
        status_known,
        comment,
        test_cmd,
        repair_ingots,
    }
}

fn infer_status_from_text(raw: &str) -> Option<bool> {
    for line in raw.lines() {
        let upper = line.trim().to_ascii_uppercase();
        if upper.contains("OUTCOME") && upper.contains("PASS") && !upper.contains("FAIL") {
            return Some(true);
        }
        if upper.contains("OUTCOME") && upper.contains("FAIL") {
            return Some(false);
        }
        if upper == "PASS" || upper.starts_with("PASS:") {
            return Some(true);
        }
        if upper == "FAIL" || upper.starts_with("FAIL:") {
            return Some(false);
        }
    }
    None
}

fn infer_test_from_text(raw: &str) -> String {
    for line in raw.lines() {
        let trimmed = line.trim().trim_start_matches("- ").trim();
        if let Some(cmd) = trimmed
            .strip_prefix('`')
            .and_then(|s| s.strip_suffix('`'))
            .map(str::trim)
        {
            if looks_like_shell_cmd(cmd) {
                return cmd.to_string();
            }
        }
        if looks_like_shell_cmd(trimmed) {
            return trimmed.to_string();
        }
    }
    String::new()
}

fn looks_like_shell_cmd(line: &str) -> bool {
    let l = line.trim();
    if l.is_empty() {
        return false;
    }
    let lowered = l.to_lowercase();
    lowered.starts_with("npm ")
        || lowered.starts_with("npx ")
        || lowered.starts_with("node ")
        || lowered.starts_with("cargo ")
        || lowered.starts_with("bash ")
        || lowered.starts_with("sh ")
        || lowered.starts_with("python ")
        || lowered.starts_with("pytest ")
        || lowered.starts_with("pnpm ")
        || lowered.starts_with("yarn ")
        || lowered.starts_with("playwright ")
        || lowered.starts_with("curl ")
        || lowered.starts_with("test ")
}

fn fallback_outcome_test(crucible: &Crucible, requires_browser_test: bool) -> Option<String> {
    let mut generic_fallback: Option<String> = None;
    for ingot in crucible.ingots.iter().rev() {
        if ingot.status != Status::Forged {
            continue;
        }
        let proof = ingot.proof.trim();
        if proof.is_empty() || proof == "true" {
            continue;
        }
        if requires_browser_test && looks_like_browser_test(proof) {
            return Some(proof.to_string());
        }
        if generic_fallback.is_none() {
            generic_fallback = Some(proof.to_string());
        }
    }
    if requires_browser_test {
        return deterministic_web_smoke_cmd(generic_fallback.as_deref().unwrap_or_default());
    }
    generic_fallback
}

fn synthetic_repair_ingot(test_cmd: &str, comment: &str, requires_browser_test: bool) -> Ingot {
    let summary = if comment.trim().is_empty() {
        "Outcome validation failed".to_string()
    } else {
        tui::truncate(comment.trim(), 120)
    };
    Ingot {
        id: "v_auto".into(),
        status: Status::Ore,
        solo: false,
        grade: if requires_browser_test { 3 } else { 2 },
        skill: if requires_browser_test {
            Skill::Web
        } else {
            Skill::Default
        },
        heat: 0,
        max: 5,
        smelt: 0,
        proof: if test_cmd.trim().is_empty() {
            "true".into()
        } else {
            test_cmd.to_string()
        },
        work: format!(
            "Fix outcome validation failure and make TEST pass: {}",
            summary
        ),
        extra: vec![],
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
        || c.contains("outcome_web_smoke.js")
}

fn screenshot_artifact_ok(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.len() > 0)
        .unwrap_or(false)
}

fn looks_like_weak_test(cmd: &str, requires_browser_test: bool) -> bool {
    let lowered = cmd.trim().to_ascii_lowercase();
    if lowered.is_empty() || lowered == "true" || lowered == "echo ok" {
        return true;
    }
    if requires_browser_test {
        return !looks_like_browser_test(&lowered);
    }
    if lowered.contains("cargo test")
        || lowered.contains("npm test")
        || lowered.contains("pnpm test")
        || lowered.contains("yarn test")
        || lowered.contains("pytest")
        || lowered.contains("playwright")
        || lowered.contains("outcome_web_smoke.js")
    {
        return false;
    }
    lowered.contains("test -f")
        || lowered.contains("grep -q")
        || lowered.contains("node --check")
        || lowered.contains("cargo fmt")
        || lowered.contains("npm run build")
}

fn deterministic_web_smoke_cmd(reference_cmd: &str) -> Option<String> {
    if !Path::new(DETERMINISTIC_WEB_SMOKE_SCRIPT).exists() {
        return None;
    }
    let mut cmd = format!(
        "node {}",
        shell_single_quote(DETERMINISTIC_WEB_SMOKE_SCRIPT)
    );
    if let Some(cwd) = infer_cwd_from_command(reference_cmd) {
        cmd.push_str(&format!(" --cwd {}", shell_single_quote(&cwd)));
    }
    Some(cmd)
}

fn infer_cwd_from_command(cmd: &str) -> Option<String> {
    let trimmed = cmd.trim();
    let rest = trimmed.strip_prefix("cd ")?;
    let end = rest
        .find("&&")
        .or_else(|| rest.find(';'))
        .unwrap_or(rest.len());
    let candidate = rest[..end].trim().trim_matches('"').trim_matches('\'');
    if candidate.is_empty() {
        None
    } else {
        Some(candidate.to_string())
    }
}

fn shell_single_quote(v: &str) -> String {
    format!("'{}'", v.replace('\'', "'\"'\"'"))
}

fn extract_outcome_evidence(
    output: &str,
    screenshot: &str,
    screenshot_ok: bool,
) -> OutcomeEvidence {
    let mut evidence = OutcomeEvidence {
        screenshot: if screenshot_ok {
            screenshot.to_string()
        } else {
            "missing".to_string()
        },
        metric_label: None,
        metric_value: None,
        console_errors: None,
    };

    if let (Some(start), Some(end)) = (output.find('{'), output.rfind('}')) {
        if start < end {
            let maybe_json = &output[start..=end];
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(maybe_json) {
                evidence.metric_label = value
                    .get("metricLabel")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                evidence.metric_value = value.get("metricValue").and_then(|v| v.as_i64());
                evidence.console_errors = value
                    .get("consoleErrors")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as usize);
                if let Some(path) = value.get("screenshot").and_then(|v| v.as_str()) {
                    evidence.screenshot = path.to_string();
                }
            }
        }
    }

    evidence
}

fn log_to_file(label: &str, content: &str) {
    let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let path = format!("{}/{ts}_{label}.log", crate::config::LOG_DIR);
    let _ = std::fs::write(&path, content);
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn parse_outcome_pass() {
        let raw = "STATUS: PASS\nCOMMENT: behavior looks correct\nTEST: echo ok\n";
        let decision = parse_outcome_response(raw);
        assert!(decision.passed);
        assert!(decision.status_known);
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
        assert!(decision.status_known);
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

    #[test]
    fn parse_outcome_infers_pass_status_from_narrative() {
        let raw = "The outcome is **PASS**: simulation works.\nAll acceptance criteria confirmed via Playwright.";
        let decision = parse_outcome_response(raw);
        assert!(decision.passed);
        assert!(decision.status_known);
    }

    #[test]
    fn parse_outcome_infers_test_from_inline_command() {
        let raw = "Use this command:\n`npx playwright test`";
        let decision = parse_outcome_response(raw);
        assert_eq!(decision.test_cmd, "npx playwright test");
    }

    #[test]
    fn synthetic_repair_is_web_for_browser_outcomes() {
        let repair = synthetic_repair_ingot("npx playwright test", "No snakes visible", true);
        assert_eq!(repair.skill, Skill::Web);
        assert_eq!(repair.grade, 3);
        assert_eq!(repair.status, Status::Ore);
    }

    #[test]
    fn screenshot_artifact_checking() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("outcome-smoke.png");
        assert!(!screenshot_artifact_ok(&path));

        std::fs::write(&path, b"png").expect("write");
        assert!(screenshot_artifact_ok(&path));
    }

    #[test]
    fn weak_test_detection() {
        assert!(looks_like_weak_test("test -f src/main.js", false));
        assert!(!looks_like_weak_test("cargo test --all", false));
        assert!(looks_like_weak_test("npm test && npm run build", true));
        assert!(!looks_like_weak_test(
            "node scripts/outcome_web_smoke.js --cwd snake-3d",
            true
        ));
    }

    #[test]
    fn infer_cwd_from_cd_command() {
        assert_eq!(
            infer_cwd_from_command("cd snake-3d && npm test"),
            Some("snake-3d".to_string())
        );
        assert_eq!(infer_cwd_from_command("npm test"), None);
    }

    #[test]
    fn parse_outcome_evidence_json() {
        let output = r#"{
          "ok": true,
          "metricLabel": "snakes",
          "metricValue": 9,
          "consoleErrors": 0,
          "screenshot": "logs/outcome-smoke.png"
        }"#;
        let evidence = extract_outcome_evidence(output, OUTCOME_SCREENSHOT_PATH, true);
        assert_eq!(evidence.metric_label.as_deref(), Some("snakes"));
        assert_eq!(evidence.metric_value, Some(9));
        assert_eq!(evidence.console_errors, Some(0));
        assert_eq!(evidence.screenshot, "logs/outcome-smoke.png");
    }
}
