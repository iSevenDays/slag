use std::time::Duration;

use crate::config::{BLUEPRINT, CRUCIBLE, MAX_ITERATE, ORE_FILE};
use crate::crucible::{self, Crucible};
use crate::error::SlagError;
use crate::flux;
use crate::smith::claude::ClaudeSmith;
use crate::smith::{self, Smith};
use crate::tui;

/// Phase 2: Read blueprint and produce S-expression ingots in PLAN.md
pub async fn run(smith: &dyn Smith, verbose: bool) -> Result<(), SlagError> {
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
    let mut ingots = crucible::parse_ingot_lines(&raw);

    // Recovery path: some models return prose/XML despite strict format instructions.
    for attempt in 1..=MAX_ITERATE {
        if !ingots.is_empty() {
            break;
        }
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
        ingots = crucible::parse_ingot_lines(&raw);
    }

    if ingots.is_empty() {
        tui::status_line(
            "↺",
            tui::COLD,
            "Founder produced no ingots; escalating to subagent",
        );
        if let Some((_handoff_raw, handoff_ingots)) =
            try_subagent_founder(&ore, &blueprint, &raw).await
        {
            ingots = handoff_ingots;
        }
    }

    if ingots.is_empty() {
        return Err(SlagError::NoIngots);
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

fn subagent_command() -> String {
    std::env::var("SLAG_SMITH_SUBAGENT")
        .unwrap_or_else(|_| "npx -y @anthropic-ai/claude-code -p".to_string())
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
    let subagent = ClaudeSmith::new(subagent_command());
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

    let raw = match smith::self_iterate(&subagent, raw, MAX_ITERATE).await {
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
