pub mod analysis;
pub mod assay;
pub mod forge;
pub mod founder;
pub mod outcome;
pub mod resmelt;
pub mod review;
pub mod surveyor;

use crate::config::{PipelineConfig, SmithConfig};
use crate::crucible::Crucible;
use crate::error::SlagError;
use crate::smith::claude::ClaudeSmith;
use crate::tui;

/// Run the full pipeline (4 or 5 phases depending on review mode).
pub async fn run(
    commission: Option<&str>,
    smith_config: &SmithConfig,
    pipeline_config: &PipelineConfig,
) -> Result<(), SlagError> {
    tui::show_banner();

    // Fire furnace if needed
    fire_furnace(commission)?;

    // Phase 1: Survey
    if !std::path::Path::new(crate::config::BLUEPRINT).exists() {
        let smith = ClaudeSmith::new(smith_config.surveyor.clone());
        surveyor::run(&smith, pipeline_config.verbose).await?;
    }

    // Phase 2: Found
    let crucible_path = std::path::Path::new(crate::config::CRUCIBLE);
    let needs_founder = !crucible_path.exists() || {
        let content = std::fs::read_to_string(crucible_path).unwrap_or_default();
        !content.contains("(ingot ")
    };
    if needs_founder {
        let smith = ClaudeSmith::new(smith_config.founder.clone());
        founder::run(
            &smith,
            pipeline_config.verbose,
            smith_config.founder_confidence_threshold,
        )
        .await?;
    }

    // Phase 3: Forge (with retry loop)
    let forge_start = std::time::Instant::now();
    let mut cycle = 0;
    let max_cycles = pipeline_config.max_retry + 1; // +1 for initial attempt

    loop {
        cycle += 1;

        if cycle > 1 {
            tui::header(&format!("FORGE · retry {}/{}", cycle - 1, max_cycles - 1));
        } else {
            tui::header("FORGE");
        }
        tui::show_legend();

        let crucible = Crucible::load(crucible_path)?;
        let counts = crucible.counts();
        print!("  ");
        tui::ingot_status_line(&counts);
        println!();
        println!(
            "  \x1b[90mcycle {}/{} · done {}/{} · cracked {} · elapsed {}\x1b[0m",
            cycle,
            max_cycles,
            counts.forged,
            counts.total,
            counts.cracked,
            tui::format_elapsed(forge_start.elapsed().as_secs())
        );

        // Run forge (ignore ForgeFailed error - we handle it with analysis)
        let forge_result = forge::run(smith_config, pipeline_config).await;

        let forged_branches = match forge_result {
            Ok(branches) => branches,
            Err(SlagError::ForgeFailed(_)) => Vec::new(),
            Err(e) => return Err(e),
        };
        let forged_branches = if pipeline_config.worktree {
            collect_forged_worktree_results(crucible_path).await?
        } else {
            forged_branches
        };

        // Phase 3.5: Review (if worktree mode enabled)
        if pipeline_config.should_review() && !forged_branches.is_empty() {
            let smith = ClaudeSmith::new(smith_config.review.clone());
            review::run(&smith, pipeline_config, &forged_branches).await?;
        } else if pipeline_config.worktree
            && pipeline_config.skip_review
            && !forged_branches.is_empty()
        {
            tui::header("MERGE · skip-review mode");
            for result in &forged_branches {
                println!(
                    "  \x1b[38;5;220m◐\x1b[0m merging [\x1b[1;37m{}\x1b[0m]",
                    result.id
                );
                crate::anvil::worktree::merge_and_cleanup(&result.id).await?;
            }
        }

        // Check if we're done (all forged, none cracked)
        let crucible = Crucible::load(crucible_path)?;
        let counts = crucible.counts();

        if counts.cracked == 0 {
            if pipeline_config.outcome_gate {
                let validator = ClaudeSmith::new(smith_config.outcome.clone());
                let outcome_passed = outcome::validate_and_queue(
                    &validator,
                    cycle,
                    pipeline_config.verbose,
                    smith_config.outcome_confidence_threshold,
                )
                .await?;
                if outcome_passed {
                    break;
                }
                if cycle >= max_cycles {
                    println!(
                        "\n  \x1b[31m✗\x1b[0m Max retries ({}) exhausted with unresolved outcome failures",
                        max_cycles - 1
                    );
                    break;
                }
                println!("\n  \x1b[38;5;220m↺\x1b[0m Outcome failed, forging repair ingots...\n");
                continue;
            }

            break;
        }

        // Check if we've exhausted retries
        if cycle >= max_cycles {
            println!(
                "\n  \x1b[31m✗\x1b[0m Max retries ({}) exhausted, {} ingots still cracked",
                max_cycles - 1,
                counts.cracked
            );
            break;
        }

        // Analyze failures and prepare for retry
        let smith = ClaudeSmith::new(smith_config.recovery.clone());
        let can_retry = analysis::analyze_and_prepare(&smith, smith_config, cycle).await?;

        if !can_retry {
            println!("\n  \x1b[31m✗\x1b[0m No recoverable ingots, stopping");
            break;
        }

        println!("\n  \x1b[38;5;220m↺\x1b[0m Retrying forge...\n");
    }

    // Phase 4: Assay
    let elapsed_secs = forge_start.elapsed().as_secs();
    assay::show(Some(elapsed_secs))?;

    // Final check - if any cracked, return error
    let crucible = Crucible::load(crucible_path)?;
    let counts = crucible.counts();
    if counts.cracked > 0 {
        return Err(SlagError::ForgeFailed(counts.cracked));
    }
    if crucible.has_pending() {
        return Err(SlagError::OutcomeFailed(format!(
            "pipeline ended with {} pending ingot(s)",
            counts.ore + counts.molten
        )));
    }

    Ok(())
}

async fn collect_forged_worktree_results(
    crucible_path: &std::path::Path,
) -> Result<Vec<forge::ForgeResult>, SlagError> {
    let crucible = Crucible::load(crucible_path)?;
    let existing_branches = review::list_forge_branches().await;
    let mut results = Vec::new();

    for branch in existing_branches {
        let Some(id) = branch.strip_prefix("forge/") else {
            continue;
        };
        let Some(ingot) = crucible.get(id) else {
            continue;
        };
        if ingot.status != crate::sexp::Status::Forged {
            continue;
        }
        let worktree_path = format!("../slag-anvil-{id}");
        results.push(forge::ForgeResult {
            id: id.to_string(),
            branch: Some(branch),
            worktree_path: if std::path::Path::new(&worktree_path).exists() {
                Some(worktree_path)
            } else {
                None
            },
            heat_used: ingot.heat,
        });
    }

    Ok(results)
}

/// Initialize project structure (fire the furnace)
fn fire_furnace(commission: Option<&str>) -> Result<(), SlagError> {
    let ore_path = std::path::Path::new(crate::config::ORE_FILE);

    if ore_path.exists() {
        return Ok(());
    }

    let commission = commission.ok_or(SlagError::NoOre)?;

    tui::header("FIRING FURNACE");

    // git init
    let _ = std::process::Command::new("git")
        .args(["init", "-b", "main"])
        .output();

    // .gitignore
    let gitignore = std::path::Path::new(".gitignore");
    let content = std::fs::read_to_string(gitignore).unwrap_or_default();
    if !content.contains("logs/") {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(gitignore)?;
        use std::io::Write;
        writeln!(f, "logs/")?;
    }

    // Create PRD.md
    std::fs::write(ore_path, format!("# Commission\n\n{commission}\n"))?;
    tui::status_line("░", tui::COLD, "Ore loaded");

    // Create AGENTS.md
    let alloy_path = std::path::Path::new(crate::config::ALLOY_FILE);
    if !alloy_path.exists() {
        std::fs::write(alloy_path, "## Alloy Recipes\n")?;
        tui::status_line("+", tui::COLD, "Recipes ready");
    }

    // Create PROGRESS.md
    let ledger_path = std::path::Path::new(crate::config::LEDGER);
    if !ledger_path.exists() {
        std::fs::write(
            ledger_path,
            format!(
                "# Smithy Ledger\nFired: {}\n",
                chrono::Local::now().format("%Y-%m-%d %H:%M")
            ),
        )?;
        tui::status_line("+", tui::COLD, "Ledger open");
    }

    // Create logs dir
    std::fs::create_dir_all(crate::config::LOG_DIR)?;

    // Initial commit
    let _ = std::process::Command::new("git")
        .args(["add", "-A"])
        .output();
    let _ = std::process::Command::new("git")
        .args(["commit", "-m", "fire: furnace lit", "--quiet"])
        .output();

    tui::status_line("█", tui::HOT, "Furnace hot");
    Ok(())
}
