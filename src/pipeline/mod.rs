pub mod analysis;
pub mod assay;
pub mod forge;
pub mod founder;
pub mod outcome;
pub mod quarrier;
pub mod recast;
pub mod resmelt;
pub mod review;
pub mod surveyor;

use crate::config::{PipelineConfig, SmithConfig};
use crate::crucible::Crucible;
use crate::error::SlagError;
use crate::events;
use crate::tui;

/// Run the full pipeline (4 or 5 phases depending on review mode).
pub async fn run(
    commission: Option<&str>,
    smith_config: &SmithConfig,
    pipeline_config: &PipelineConfig,
) -> Result<(), SlagError> {
    events::emit_info(
        "pipeline.start",
        "pipeline run started",
        serde_json::json!({
            "worktree": pipeline_config.worktree,
            "max_anvils": pipeline_config.max_anvils,
            "max_retry": pipeline_config.max_retry,
            "outcome_gate": pipeline_config.outcome_gate,
            "prompt_policy": format!("{:?}", pipeline_config.prompt_policy),
            "prompt_timeout_secs": pipeline_config.prompt_timeout_secs,
            "log_format": format!("{:?}", pipeline_config.log_format),
        }),
    );
    tui::show_banner();

    // Fire furnace if needed
    fire_furnace(commission)?;

    // Phase 0: Quarrier (decompose large commissions)
    let phases = if should_quarry(pipeline_config) {
        if let Some(existing) = quarrier::load_phases() {
            tui::status_line(
                "\u{2588}",
                tui::PURE,
                &format!("Resuming {} quarried phase(s)", existing.len()),
            );
            existing
        } else {
            let smith = crate::smith::build_smith(&smith_config.base)?;
            quarrier::run(&*smith, pipeline_config.verbose).await?
        }
    } else {
        quarrier::Phase::single_phase()
    };

    let total_phases = phases.len();

    for (phase_idx, phase) in phases.iter().enumerate() {
        if total_phases > 1 {
            tui::header(&format!(
                "PHASE {}/{} \u{00b7} {}",
                phase_idx + 1,
                total_phases,
                phase.name
            ));
        }

        // Scope ore for this phase
        let scoped_ore = scope_ore_for_phase(phase, phase_idx, total_phases);

        // Phase 1: Survey
        if !std::path::Path::new(crate::config::BLUEPRINT).exists() {
            // When multi-phase, write scoped ore as temporary PRD overlay
            if total_phases > 1 {
                let ore_path = std::path::Path::new(crate::config::ORE_FILE);
                let original_ore = std::fs::read_to_string(ore_path).unwrap_or_default();
                std::fs::write(ore_path, &scoped_ore)?;
                let smith = crate::smith::build_smith(&smith_config.surveyor)?;
                let result = surveyor::run(&*smith, pipeline_config.verbose).await;
                // Restore original ore
                std::fs::write(ore_path, original_ore)?;
                result?;
            } else {
                let smith = crate::smith::build_smith(&smith_config.surveyor)?;
                surveyor::run(&*smith, pipeline_config.verbose).await?;
            }
        }

        // Phase 2: Found
        let crucible_path = std::path::Path::new(crate::config::CRUCIBLE);
        let needs_founder = !crucible_path.exists() || {
            let content = std::fs::read_to_string(crucible_path).unwrap_or_default();
            !content.contains("(ingot ")
        };
        if needs_founder {
            let smith = crate::smith::build_smith(&smith_config.founder)?;
            founder::run(
                &*smith,
                pipeline_config.verbose,
                smith_config.founder_confidence_threshold,
            )
            .await?;
        }

        // Phase 3: Forge (with retry loop) — run the existing forge loop for this phase
        run_forge_loop(smith_config, pipeline_config).await?;

        // Archive phase artifacts for multi-phase runs (skip last phase so assay can read it)
        if total_phases > 1 && phase_idx < total_phases - 1 {
            archive_phase_artifacts(phase_idx);
        }
    }

    // Phase 4: Assay (runs once after all phases)
    let crucible_path = std::path::Path::new(crate::config::CRUCIBLE);
    assay::show(None)?;

    // Final check - if any cracked, return error
    let crucible = Crucible::load(crucible_path)?;
    let counts = crucible.counts();
    if counts.cracked > 0 {
        events::emit_error(
            "pipeline.finish.cracked",
            "pipeline ended with cracked ingots",
            serde_json::json!({ "cracked": counts.cracked }),
        );
        return Err(SlagError::ForgeFailed(counts.cracked));
    }
    if crucible.has_pending() {
        events::emit_error(
            "pipeline.finish.pending",
            "pipeline ended with pending ingots",
            serde_json::json!({
                "ore": counts.ore,
                "molten": counts.molten
            }),
        );
        return Err(SlagError::OutcomeFailed(format!(
            "pipeline ended with {} pending ingot(s)",
            counts.ore + counts.molten
        )));
    }

    events::emit_info(
        "pipeline.finish.success",
        "pipeline run completed successfully",
        serde_json::json!({
            "forged": counts.forged,
            "total": counts.total
        }),
    );

    Ok(())
}

/// Check if the quarrier should run.
fn should_quarry(pipeline_config: &PipelineConfig) -> bool {
    if pipeline_config.no_quarry {
        return false;
    }
    // Resume: PHASES.md exists — always honour quarried phases even mid-run
    if std::path::Path::new(crate::config::PHASES_FILE).exists() {
        return true;
    }
    // Already mid-run: blueprint exists (single-phase run)
    if std::path::Path::new(crate::config::BLUEPRINT).exists() {
        return false;
    }
    // Large commission: ore > 300 chars
    let ore = std::fs::read_to_string(crate::config::ORE_FILE).unwrap_or_default();
    ore.len() > 300
}

/// Scope the ore for a specific phase, combining original PRD + phase context + prior work.
fn scope_ore_for_phase(phase: &quarrier::Phase, phase_idx: usize, total: usize) -> String {
    let original_ore = std::fs::read_to_string(crate::config::ORE_FILE).unwrap_or_default();
    if total <= 1 {
        return original_ore;
    }

    let prior_work = std::fs::read_to_string(crate::config::LEDGER)
        .ok()
        .and_then(|content| {
            let lines: Vec<&str> = content.lines().collect();
            let start = lines.len().saturating_sub(15);
            let tail = lines[start..].join("\n");
            if tail.trim().is_empty() {
                None
            } else {
                Some(tail)
            }
        })
        .unwrap_or_else(|| "No prior work yet.".into());

    format!(
        "{original_ore}\n\n\
        === CURRENT PHASE ({}/{total}) ===\n\
        Phase: {}\n\
        Description: {}\n\
        Success criteria: {}\n\n\
        === PRIOR WORK ===\n\
        {prior_work}\n\n\
        Focus ONLY on this phase. Earlier phases are already done.\n",
        phase_idx + 1,
        phase.name,
        phase.description,
        phase.criteria
    )
}

/// Archive phase artifacts between phases (rename BLUEPRINT/PLAN with phase suffix).
fn archive_phase_artifacts(phase_idx: usize) {
    let suffix = format!("_p{}", phase_idx + 1);
    let bp = crate::config::BLUEPRINT;
    let bp_archive = format!("{}{}.md", bp.trim_end_matches(".md"), suffix);
    let _ = std::fs::rename(bp, &bp_archive);

    // Delete PLAN.md so the next phase triggers founder
    let _ = std::fs::remove_file(crate::config::CRUCIBLE);
}

/// Run the forge retry loop (extracted from the original inline code).
async fn run_forge_loop(
    smith_config: &SmithConfig,
    pipeline_config: &PipelineConfig,
) -> Result<(), SlagError> {
    let crucible_path = std::path::Path::new(crate::config::CRUCIBLE);

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
        events::emit_info(
            "forge.cycle.start",
            "forge cycle started",
            serde_json::json!({
                "cycle": cycle,
                "max_cycles": max_cycles,
                "forged": counts.forged,
                "total": counts.total,
                "cracked": counts.cracked
            }),
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
            let smith = crate::smith::build_smith(&smith_config.review)?;
            review::run(&*smith, pipeline_config, &forged_branches).await?;
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

        // Re-run proofs for cracked ingots — earlier forged ingots may have
        // already satisfied the work, so we can promote without a smith call.
        let promoted = forge::post_forge_proof_reeval(crucible_path).await?;
        let (_crucible, counts) = if promoted > 0 {
            println!(
                "  \x1b[1;37m⤴\x1b[0m promoted {} cracked ingot(s) to forged via proof re-eval",
                promoted
            );
            let c = Crucible::load(crucible_path)?;
            let n = c.counts();
            (c, n)
        } else {
            (crucible, counts)
        };

        if counts.cracked == 0 {
            if pipeline_config.outcome_gate {
                let validator = crate::smith::build_smith(&smith_config.outcome)?;
                let outcome_passed = outcome::validate_and_queue(
                    &*validator,
                    cycle,
                    pipeline_config.verbose,
                    smith_config.outcome_confidence_threshold,
                )
                .await?;
                if outcome_passed {
                    events::emit_info(
                        "outcome.pass",
                        "outcome validator passed",
                        serde_json::json!({ "cycle": cycle }),
                    );
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
                events::emit_warn(
                    "outcome.fail",
                    "outcome validator failed, queued repair ingots",
                    serde_json::json!({ "cycle": cycle }),
                );
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
            events::emit_error(
                "forge.retries_exhausted",
                "max retry cycles exhausted with cracked ingots",
                serde_json::json!({
                    "cycle": cycle,
                    "max_cycles": max_cycles,
                    "cracked": counts.cracked
                }),
            );
            break;
        }

        // Analyze failures and prepare for retry
        let smith = crate::smith::build_smith(&smith_config.recovery)?;
        let can_retry =
            analysis::analyze_and_prepare(&*smith, smith_config, pipeline_config, cycle).await?;

        if !can_retry {
            println!("\n  \x1b[31m✗\x1b[0m No recoverable ingots, stopping");
            events::emit_warn(
                "analysis.stop",
                "analysis found no recoverable ingots",
                serde_json::json!({ "cycle": cycle }),
            );
            break;
        }

        println!("\n  \x1b[38;5;220m↺\x1b[0m Retrying forge...\n");
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
