use std::collections::HashMap;
use std::io::{self, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use crate::anvil::worktree;
use crate::config::{PipelineConfig, PromptPolicy, SmithConfig, CRUCIBLE, LEDGER};
use crate::crucible::Crucible;
use crate::error::SlagError;
use crate::events;
use crate::flux;
use crate::prompt;
use crate::proof;
use crate::sexp::{Ingot, Status};
use crate::smith::claude::ClaudeSmith;
use crate::smith::Smith;
use crate::tui;

use super::resmelt;

const DEFAULT_VERBOSE_HEARTBEAT_SECS: u64 = 15;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StaleMoltenAction {
    Requeue,
    Crack,
    Abort,
}

/// Result of forging an ingot, including branch name if worktree mode
#[derive(Debug, Clone)]
pub struct ForgeResult {
    pub id: String,
    pub branch: Option<String>,
    pub worktree_path: Option<String>,
    pub heat_used: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputMode {
    Quiet,
    Compact,
    Verbose,
}

impl OutputMode {
    fn is_quiet(self) -> bool {
        matches!(self, Self::Quiet)
    }

    fn is_verbose(self) -> bool {
        matches!(self, Self::Verbose)
    }
}

/// Phase 3: Forge loop — parallel anvils then sequential
/// Returns list of forged branches (empty if not using worktree mode)
pub async fn run(
    config: &SmithConfig,
    pipeline_config: &PipelineConfig,
) -> Result<Vec<ForgeResult>, SlagError> {
    let mut forged_results: Vec<ForgeResult> = Vec::new();
    let independent_smith = config
        .independent
        .as_ref()
        .map(|cmd| ClaudeSmith::new(cmd.clone()));
    let use_worktree = pipeline_config.worktree;
    let max_anvils = pipeline_config.max_anvils;
    let sequential_output_mode = if pipeline_config.verbose {
        OutputMode::Verbose
    } else {
        OutputMode::Compact
    };

    loop {
        let mut crucible = Crucible::load(Path::new(CRUCIBLE))?;
        let renamed = normalize_duplicate_ingot_ids(&mut crucible);
        let quarantined = quarantine_invalid_pending_ingots(&mut crucible);
        if !renamed.is_empty() || !quarantined.is_empty() {
            crucible.save()?;
            if !renamed.is_empty() {
                tui::status_line(
                    "↺",
                    tui::WARM,
                    &format!(
                        "normalized {} duplicate ingot id(s) to unique ids",
                        renamed.len()
                    ),
                );
            }
            if !quarantined.is_empty() {
                tui::status_line(
                    "↺",
                    tui::WARM,
                    &format!(
                        "quarantined {} malformed pending ingot(s) as cracked",
                        quarantined.len()
                    ),
                );
                for (id, reason) in quarantined.iter().take(3) {
                    println!("  \x1b[90m[{id}] {reason}\x1b[0m");
                }
                if quarantined.len() > 3 {
                    println!("  \x1b[90m... +{} more\x1b[0m", quarantined.len() - 3);
                }
            }
            events::emit_warn(
                "forge.preflight.quarantine",
                "normalized duplicate IDs and/or quarantined malformed pending ingots",
                serde_json::json!({
                    "renamed": renamed.len(),
                    "quarantined": quarantined.len(),
                }),
            );
        }

        if !crucible.has_pending() {
            // Check for cracked
            let counts = crucible.counts();
            if counts.cracked > 0 {
                return Err(SlagError::ForgeFailed(counts.cracked));
            }
            return Ok(forged_results);
        }

        // --- Parallel anvils for :solo t ---
        let solo_ids: Vec<String> = crucible
            .solo_ore()
            .iter()
            .take(max_anvils)
            .map(|i| i.id.clone())
            .collect();

        if !solo_ids.is_empty() {
            // Mark as molten
            for id in &solo_ids {
                crucible.set_status(id, Status::Molten);
            }
            crucible.save()?;

            // Snapshot ingots before spawning (each task gets its own copy)
            let ingot_snapshots: Vec<Ingot> = solo_ids
                .iter()
                .filter_map(|id| crucible.get(id).cloned())
                .collect();

            println!("\n  \x1b[38;5;208m⚒ ANVILS [{}]\x1b[0m", solo_ids.len());
            let last_idx = ingot_snapshots.len().saturating_sub(1);
            for (i, ingot) in ingot_snapshots.iter().enumerate() {
                let prefix = if i == last_idx { "└─" } else { "├─" };
                println!(
                    "  \x1b[90m{}\x1b[0m \x1b[1;37m{}\x1b[0m  \x1b[38;5;208m◐\x1b[0m forging...  \x1b[90m{}\x1b[0m",
                    prefix,
                    ingot.id,
                    tui::truncate(&ingot.work, 40),
                );
            }

            // Spawn parallel tasks
            let mut set = tokio::task::JoinSet::new();
            for ingot in ingot_snapshots {
                let smith_cmd = config.select(ingot.skill.as_str(), ingot.grade).to_string();
                let worktree_mode = use_worktree;
                set.spawn(async move {
                    let smith = ClaudeSmith::new(smith_cmd);
                    let result =
                        strike_ingot(&ingot, &smith, worktree_mode, OutputMode::Quiet).await;
                    (ingot.id.clone(), result)
                });
            }

            let heartbeat_secs = verbose_heartbeat_secs();
            let mut pending_started: HashMap<String, Instant> = solo_ids
                .iter()
                .map(|id| (id.clone(), Instant::now()))
                .collect();
            let mut last_completion = Instant::now();

            // Collect results and update crucible on main thread
            loop {
                let next = if pipeline_config.verbose {
                    match tokio::time::timeout(Duration::from_secs(heartbeat_secs), set.join_next())
                        .await
                    {
                        Ok(result) => result,
                        Err(_) => {
                            log_parallel_heartbeat(
                                &pending_started,
                                last_completion,
                                heartbeat_secs,
                            );
                            continue;
                        }
                    }
                } else {
                    set.join_next().await
                };

                let Some(result) = next else {
                    break;
                };

                let mut crucible = Crucible::load(Path::new(CRUCIBLE))?;
                match result {
                    Ok((id, Ok(forge_result))) => {
                        pending_started.remove(&id);
                        last_completion = Instant::now();
                        let heat_used = forge_result.heat_used;
                        let max_heat = crucible
                            .get(&id)
                            .map(|ingot| ingot.max)
                            .unwrap_or(heat_used);
                        crucible.set_status(&id, Status::Forged);
                        if let Some(ingot) = crucible.get_mut(&id) {
                            ingot.heat = heat_used;
                        }
                        crucible.save()?;
                        forged_results.push(forge_result);
                        println!(
                            "  \x1b[1;37m✓\x1b[0m [{}] forged (heat {}/{})",
                            id, heat_used, max_heat
                        );
                    }
                    Ok((id, Err(SlagError::IngotCracked(_, heat_used)))) => {
                        pending_started.remove(&id);
                        last_completion = Instant::now();
                        // Try resmelt
                        if let Some(ingot) = crucible.get_mut(&id) {
                            ingot.heat = heat_used;
                        }
                        if let Some(ingot) = crucible.get(&id).cloned() {
                            let smith = ClaudeSmith::new(config.recovery.clone());
                            if resmelt::resmelt_ingot(
                                &mut crucible,
                                &ingot,
                                &smith,
                                independent_smith.as_ref().map(|s| s as &dyn Smith),
                            )
                            .await
                            .is_ok()
                            {
                                crucible.save()?;
                                println!("  \x1b[38;5;220m♻\x1b[0m [{}] re-smelted and queued", id);
                            } else {
                                crucible.set_status(&id, Status::Cracked);
                                crucible.save()?;
                                println!(
                                    "  \x1b[31m✗\x1b[0m [{}] cracked after {} heat(s)",
                                    id, heat_used
                                );
                            }
                        }
                    }
                    Ok((id, Err(e))) => {
                        pending_started.remove(&id);
                        last_completion = Instant::now();
                        eprintln!(
                            "  \x1b[31m✗\x1b[0m [{}] forge infrastructure error: {e}",
                            id
                        );
                        crucible.set_status(&id, Status::Cracked);
                        crucible.save()?;
                    }
                    Err(e) => {
                        last_completion = Instant::now();
                        eprintln!("  \x1b[31m✗\x1b[0m anvil panicked: {e}");
                    }
                }
            }

            // Show status
            let crucible = Crucible::load(Path::new(CRUCIBLE))?;
            print!("\n  ");
            tui::ingot_status_line(&crucible.counts());
            println!();
            continue;
        }

        // --- Sequential for :solo nil ---
        let ingot = match crucible.next_ore() {
            Some(i) => i.clone(),
            None => {
                let stale_ids = stale_molten_ids(&crucible);
                if stale_ids.is_empty() {
                    continue;
                }

                let estimate = estimate_reforge_secs(stale_ids.len(), pipeline_config.max_anvils);
                match choose_stale_molten_action(
                    &stale_ids,
                    estimate,
                    pipeline_config.verbose,
                    pipeline_config.prompt_policy,
                    pipeline_config.prompt_timeout_secs,
                )
                .await?
                {
                    StaleMoltenAction::Requeue => {
                        let recovered = recover_stale_molten_to_ore(&mut crucible);
                        if pipeline_config.verbose {
                            tui::status_line(
                                "↺",
                                tui::COLD,
                                &format!(
                                    "re-queued stale molten ingot(s) to ore: {}",
                                    recovered.join(", ")
                                ),
                            );
                        } else {
                            tui::status_line(
                                "↺",
                                tui::COLD,
                                &format!(
                                    "re-queued {} stale molten ingot(s) to ore",
                                    recovered.len()
                                ),
                            );
                        }
                    }
                    StaleMoltenAction::Crack => {
                        crack_stale_molten(&mut crucible);
                        tui::status_line(
                            "↺",
                            tui::WARM,
                            &format!(
                                "marked {} stale molten ingot(s) as cracked",
                                stale_ids.len()
                            ),
                        );
                    }
                    StaleMoltenAction::Abort => {
                        events::emit_warn(
                            "forge.stale_molten.abort",
                            "operator chose abort during stale molten recovery",
                            serde_json::json!({
                                "stale_count": stale_ids.len()
                            }),
                        );
                        return Err(SlagError::StateRecoveryAbort(
                            "aborted due to stale molten ingot state".into(),
                        ));
                    }
                }
                crucible.save()?;
                continue;
            }
        };

        crucible.set_status(&ingot.id, Status::Molten);
        crucible.save()?;
        let in_fire = crucible.counts().molten;
        tui::status_line(
            "🔥",
            tui::HOT,
            &format!("forging [{}] (in fire: {in_fire})", ingot.id),
        );

        let smith_cmd = config.select(ingot.skill.as_str(), ingot.grade).to_string();
        let smith = ClaudeSmith::new(smith_cmd);

        match strike_ingot(&ingot, &smith, use_worktree, sequential_output_mode).await {
            Ok(forge_result) => {
                let mut crucible = Crucible::load(Path::new(CRUCIBLE))?;
                crucible.set_status(&ingot.id, Status::Forged);
                if let Some(ingot) = crucible.get_mut(&ingot.id) {
                    ingot.heat = forge_result.heat_used;
                }
                crucible.save()?;
                forged_results.push(forge_result);
            }
            Err(SlagError::IngotCracked(_, heat_used)) => {
                let mut crucible = Crucible::load(Path::new(CRUCIBLE))?;
                if let Some(ingot) = crucible.get_mut(&ingot.id) {
                    ingot.heat = heat_used;
                }
                let recovery_smith = ClaudeSmith::new(config.recovery.clone());
                if resmelt::resmelt_ingot(
                    &mut crucible,
                    &ingot,
                    &recovery_smith,
                    independent_smith.as_ref().map(|s| s as &dyn Smith),
                )
                .await
                .is_ok()
                {
                    // Re-smelted: status already updated by resmelt
                    crucible.save()?;
                } else {
                    crucible.set_status(&ingot.id, Status::Cracked);
                    crucible.save()?;
                }
            }
            Err(e) => return Err(e),
        }

        let crucible = Crucible::load(Path::new(CRUCIBLE))?;
        print!("\n  ");
        tui::ingot_status_line(&crucible.counts());
        println!();
    }
}

fn recover_stale_molten_to_ore(crucible: &mut Crucible) -> Vec<String> {
    let mut recovered = Vec::new();
    for ingot in &mut crucible.ingots {
        if ingot.status == Status::Molten {
            ingot.status = Status::Ore;
            recovered.push(ingot.id.clone());
        }
    }
    recovered
}

fn normalize_duplicate_ingot_ids(crucible: &mut Crucible) -> Vec<(String, String)> {
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut assigned: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut renamed = Vec::new();

    for ingot in &mut crucible.ingots {
        let original = ingot.id.clone();
        let seen_count = seen.entry(original.clone()).or_insert(0);
        *seen_count += 1;

        if *seen_count == 1 && assigned.insert(original.clone()) {
            continue;
        }

        let mut n = (*seen_count).max(2);
        let mut candidate = format!("{original}_{n}");
        while assigned.contains(&candidate) {
            n += 1;
            candidate = format!("{original}_{n}");
        }
        ingot.id = candidate.clone();
        assigned.insert(candidate.clone());
        renamed.push((original, candidate));
    }

    renamed
}

fn quarantine_invalid_pending_ingots(crucible: &mut Crucible) -> Vec<(String, String)> {
    let mut quarantined = Vec::new();
    for ingot in &mut crucible.ingots {
        if ingot.status != Status::Ore && ingot.status != Status::Molten {
            continue;
        }
        if is_placeholder_proof(&ingot.proof) {
            ingot.status = Status::Cracked;
            quarantined.push((ingot.id.clone(), "placeholder :proof".to_string()));
            continue;
        }
        if is_placeholder_work(&ingot.work) {
            ingot.status = Status::Cracked;
            quarantined.push((ingot.id.clone(), "placeholder :work".to_string()));
        }
    }
    quarantined
}

fn is_placeholder_proof(proof: &str) -> bool {
    let trimmed = proof.trim();
    if trimmed.is_empty() {
        return true;
    }
    let lowered = trimmed.to_ascii_lowercase();
    matches!(
        lowered.as_str(),
        "true" | "shell" | "proof" | "cmd" | "command" | "n/a"
    ) || lowered.contains("<shell")
}

fn is_placeholder_work(work: &str) -> bool {
    let trimmed = work.trim();
    if trimmed.is_empty() {
        return true;
    }
    matches!(
        trimmed.to_ascii_lowercase().as_str(),
        "task" | "todo" | "tbd" | "sub-task"
    )
}

fn crack_stale_molten(crucible: &mut Crucible) {
    for ingot in &mut crucible.ingots {
        if ingot.status == Status::Molten {
            ingot.status = Status::Cracked;
        }
    }
}

fn stale_molten_ids(crucible: &Crucible) -> Vec<String> {
    crucible
        .ingots
        .iter()
        .filter(|ingot| ingot.status == Status::Molten)
        .map(|ingot| ingot.id.clone())
        .collect()
}

async fn choose_stale_molten_action(
    stale_ids: &[String],
    estimate_secs: Option<u64>,
    verbose: bool,
    policy: PromptPolicy,
    timeout_secs: u64,
) -> Result<StaleMoltenAction, SlagError> {
    match policy {
        PromptPolicy::AutoRequeue => {
            events::emit_info(
                "prompt.stale_molten.auto_requeue",
                "applied prompt policy auto-requeue",
                serde_json::json!({
                    "stale_count": stale_ids.len()
                }),
            );
            return Ok(StaleMoltenAction::Requeue);
        }
        PromptPolicy::AutoCrack => {
            events::emit_info(
                "prompt.stale_molten.auto_crack",
                "applied prompt policy auto-crack",
                serde_json::json!({
                    "stale_count": stale_ids.len()
                }),
            );
            return Ok(StaleMoltenAction::Crack);
        }
        PromptPolicy::AutoAbort => {
            events::emit_warn(
                "prompt.stale_molten.auto_abort",
                "applied prompt policy auto-abort",
                serde_json::json!({
                    "stale_count": stale_ids.len()
                }),
            );
            return Ok(StaleMoltenAction::Abort);
        }
        PromptPolicy::Ask => {}
    }

    if !prompt::stdin_is_interactive() {
        events::emit_debug(
            "prompt.stale_molten.non_interactive",
            "stdin is not interactive, defaulting to requeue",
            serde_json::json!({
                "stale_count": stale_ids.len()
            }),
        );
        return Ok(StaleMoltenAction::Requeue);
    }

    println!();
    tui::status_line(
        "?",
        tui::BRIGHT,
        &format!(
            "Detected {} stale molten ingot(s) from an interrupted forge",
            stale_ids.len()
        ),
    );
    if verbose {
        println!("  \x1b[90mids: {}\x1b[0m", stale_ids.join(", "));
    }
    if let Some(secs) = estimate_secs {
        println!(
            "  \x1b[90mestimated re-forge: ~{} (from recent log samples)\x1b[0m",
            tui::format_elapsed(secs)
        );
    } else {
        println!("  \x1b[90mestimated re-forge: unknown (insufficient log samples)\x1b[0m");
    }

    print!("  \x1b[38;5;220m?\x1b[0m Choose action [R]equeue (default) / [C]rack / [A]bort: ");
    let _ = io::stdout().flush();

    let Some(input) = prompt::read_line_timeout(timeout_secs) else {
        events::emit_warn(
            "prompt.stale_molten.timeout",
            "operator prompt timed out, defaulted to requeue",
            serde_json::json!({
                "timeout_secs": timeout_secs,
                "stale_count": stale_ids.len()
            }),
        );
        tui::status_line(
            "↺",
            tui::COLD,
            &format!(
                "No operator input after {}s, defaulting to requeue",
                timeout_secs
            ),
        );
        return Ok(StaleMoltenAction::Requeue);
    };

    let choice = input.trim().to_ascii_lowercase();
    let action = match choice.as_str() {
        "c" | "crack" => StaleMoltenAction::Crack,
        "a" | "abort" => StaleMoltenAction::Abort,
        _ => StaleMoltenAction::Requeue,
    };
    events::emit_info(
        "prompt.stale_molten.choice",
        "operator selected stale molten action",
        serde_json::json!({
            "choice": choice,
            "resolved_action": format!("{:?}", action),
            "stale_count": stale_ids.len()
        }),
    );
    Ok(action)
}

fn estimate_reforge_secs(stale_count: usize, max_anvils: usize) -> Option<u64> {
    let entries = std::fs::read_dir(crate::config::LOG_DIR).ok()?;
    let mut per_heat: HashMap<String, (Option<i64>, Option<i64>)> = HashMap::new();

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(ts) = parse_log_ts(&name) else {
            continue;
        };
        let Some(label) = log_label(&name) else {
            continue;
        };

        if let Some(key) = label.strip_prefix("FLUX_") {
            let slot = per_heat.entry(key.to_string()).or_insert((None, None));
            if slot.0.map(|v| ts > v).unwrap_or(true) {
                slot.0 = Some(ts);
            }
            continue;
        }
        if let Some(key) = label.strip_prefix("ASSAY_") {
            let slot = per_heat.entry(key.to_string()).or_insert((None, None));
            if slot.1.map(|v| ts > v).unwrap_or(true) {
                slot.1 = Some(ts);
            }
        }
    }

    let mut samples = Vec::new();
    for (_, (flux, assay)) in per_heat {
        let (Some(start), Some(end)) = (flux, assay) else {
            continue;
        };
        if end < start {
            continue;
        }
        let d = (end - start) as u64;
        if (1..=3600).contains(&d) {
            samples.push(d);
        }
    }
    if samples.is_empty() {
        return None;
    }

    samples.sort_unstable();
    let median = samples[samples.len() / 2];
    let anvils = max_anvils.max(1);
    let batches = stale_count.max(1).div_ceil(anvils);
    Some(median.saturating_mul(batches as u64))
}

fn parse_log_ts(filename: &str) -> Option<i64> {
    if filename.len() < 15 {
        return None;
    }
    let prefix = &filename[..15];
    let dt = chrono::NaiveDateTime::parse_from_str(prefix, "%Y%m%d_%H%M%S").ok()?;
    Some(dt.and_utc().timestamp())
}

fn log_label(filename: &str) -> Option<&str> {
    if !filename.ends_with(".log") || filename.len() < 21 {
        return None;
    }
    Some(&filename[16..filename.len() - 4])
}

/// Strike a single ingot: retry with heat, extract CMD, verify proof.
/// If worktree_mode is true, creates an isolated worktree branch for the work.
async fn strike_ingot(
    ingot: &Ingot,
    smith: &dyn Smith,
    worktree_mode: bool,
    output_mode: OutputMode,
) -> Result<ForgeResult, SlagError> {
    let mut slag: Option<String> = None;
    let mut worktree_path: Option<String> = None;
    let mut active_ingot = ingot.clone();
    let branch_name = format!("forge/{}", ingot.id);

    // Create worktree if in worktree mode
    if worktree_mode {
        match worktree::create(&ingot.id).await {
            Ok(path) => {
                worktree_path = Some(path.clone());
                if output_mode.is_verbose() {
                    println!(
                        "    \x1b[90m↳ worktree: {}\x1b[0m",
                        tui::truncate(&path, 40)
                    );
                }
            }
            Err(e) => {
                if !output_mode.is_quiet() {
                    eprintln!("    \x1b[31m✗\x1b[0m worktree create failed: {e}");
                }
                return Err(e);
            }
        }
    }

    if !output_mode.is_quiet() {
        println!(
            "\n  \x1b[38;5;208m▣\x1b[0m \x1b[1;37m[{}]\x1b[0m {}{}{}",
            active_ingot.id,
            tui::truncate(&active_ingot.work, 42),
            if active_ingot.is_complex() {
                " \x1b[38;5;220m◉\x1b[0m"
            } else {
                ""
            },
            if active_ingot.is_web() {
                " \x1b[38;5;208m⚡\x1b[0m"
            } else {
                ""
            },
        );
        if output_mode.is_verbose() {
            println!(
                "    \x1b[90mgr:{} skill:{} proof:{}\x1b[0m",
                active_ingot.grade,
                active_ingot.skill,
                tui::truncate(&active_ingot.proof, 30),
            );
        }
    }

    for heat in 1..=ingot.max {
        // Re-read the ingot before each heat so retries use the latest proof/work.
        // This prevents stale forge orders when PLAN.md was updated in a prior heat.
        refresh_ingot_from_crucible(&mut active_ingot);
        let heat_max = active_ingot.max.max(1);

        if output_mode.is_verbose() {
            let hc = match heat {
                1..=2 => "\x1b[31m",
                3 => "\x1b[38;5;208m",
                4 => "\x1b[38;5;220m",
                _ => "\x1b[1;37m",
            };
            print!(
                "    {hc}{} {heat}/{}\x1b[0m ",
                tui::heat_bar(heat, heat_max),
                heat_max
            );
        }

        let flux_text = flux::prepare_flux(&active_ingot, slag.as_deref());
        log_to_file(&format!("FLUX_{}_{heat}", active_ingot.id), &flux_text);

        let spinner = if output_mode.is_quiet() {
            None
        } else {
            let spinner_msg = if output_mode.is_verbose() && active_ingot.is_complex() {
                "planning..."
            } else if output_mode.is_verbose() && active_ingot.is_web() {
                "web forging..."
            } else {
                "forging..."
            };
            Some(tui::spinner(spinner_msg))
        };

        // In worktree mode, invoke smith in the worktree directory
        let response = if let Some(ref wt_path) = worktree_path {
            invoke_smith_in_worktree(smith, &flux_text, wt_path).await
        } else {
            smith.invoke(&flux_text).await
        };

        let response = match response {
            Ok(r) => {
                if let Some(ref spinner) = spinner {
                    spinner.finish_and_clear();
                }
                r
            }
            Err(e) => {
                if let Some(ref spinner) = spinner {
                    spinner.finish_and_clear();
                }
                slag = Some(format!("Smith error: {e}"));
                if !output_mode.is_quiet() {
                    if output_mode.is_verbose() {
                        println!("\x1b[31m✗\x1b[0m");
                    } else {
                        println!("    \x1b[31m↺\x1b[0m heat {heat}/{} failed", heat_max);
                    }
                }
                continue;
            }
        };

        log_to_file(&format!("STRIKE_{}_{heat}", active_ingot.id), &response);

        // Extract CMD
        let cmd = match proof::extract_cmd(&response) {
            Some(c) => c,
            None => {
                slag = Some("NO CMD: line in response".into());
                if !output_mode.is_quiet() {
                    if output_mode.is_verbose() {
                        println!("\x1b[31m✗\x1b[0m smith output missing \"CMD:\" line");
                    } else {
                        println!("    \x1b[31m↺\x1b[0m heat {heat}/{} failed", heat_max);
                    }
                }
                continue;
            }
        };

        if output_mode.is_verbose() {
            print!("\x1b[90m{}\x1b[0m ", tui::truncate(&cmd, 32));
            tui::flush();
        }

        // Run CMD (in worktree if applicable)
        let (ok, output) = if let Some(ref wt_path) = worktree_path {
            proof::run_shell_in_dir(&cmd, Path::new(wt_path)).await
        } else {
            proof::run_shell(&cmd).await
        };
        log_to_file(
            &format!("ASSAY_{}_{heat}", active_ingot.id),
            &format!("exit={}\n{output}", if ok { 0 } else { 1 }),
        );

        if ok {
            // Verify proof if different from cmd
            if !active_ingot.proof.is_empty()
                && active_ingot.proof != cmd
                && active_ingot.proof != "true"
            {
                let (proof_ok, proof_output) = if let Some(ref wt_path) = worktree_path {
                    proof::run_shell_in_dir(&active_ingot.proof, Path::new(wt_path)).await
                } else {
                    proof::run_shell(&active_ingot.proof).await
                };
                if !proof_ok {
                    slag = Some(format!(
                        "Proof failed [{}]: {proof_output}",
                        active_ingot.proof
                    ));
                    if !output_mode.is_quiet() {
                        if output_mode.is_verbose() {
                            println!(
                                "\x1b[31m✗\x1b[0m proof failed: {} (exit 1)",
                                tui::truncate(&active_ingot.proof, 30)
                            );
                        } else {
                            println!("    \x1b[31m↺\x1b[0m heat {heat}/{} failed", heat_max);
                        }
                    }
                    continue;
                }
            }

            if !output_mode.is_quiet() {
                if output_mode.is_verbose() {
                    println!("\x1b[1;37m█\x1b[0m");
                } else {
                    println!("    \x1b[1;37m✓\x1b[0m forged (heat {heat}/{})", heat_max);
                }
            }

            // Commit in worktree or main repo
            if let Some(ref wt_path) = worktree_path {
                git_commit_in_dir(&active_ingot.id, &active_ingot.work, wt_path).await;
            } else {
                proof::git_commit(&active_ingot.id, &active_ingot.work).await;
            }

            append_ledger(&active_ingot, heat);
            return Ok(ForgeResult {
                id: active_ingot.id.clone(),
                branch: if worktree_mode {
                    Some(branch_name)
                } else {
                    None
                },
                worktree_path,
                heat_used: heat,
            });
        } else {
            slag = Some(format!("CMD failed (exit 1): {output}"));
            if !output_mode.is_quiet() {
                if output_mode.is_verbose() {
                    println!("\x1b[31m✗\x1b[0m");
                } else {
                    println!("    \x1b[31m↺\x1b[0m heat {heat}/{} failed", heat_max);
                }
            }
        }
    }

    // Clean up worktree on failure (preserve branch for debugging)
    if worktree_path.is_some() {
        worktree::cleanup_without_merge(&active_ingot.id).await;
    }

    Err(SlagError::IngotCracked(
        active_ingot.id.clone(),
        active_ingot.max,
    ))
}

fn refresh_ingot_from_crucible(ingot: &mut Ingot) {
    if let Ok(crucible) = Crucible::load(Path::new(CRUCIBLE)) {
        if let Some(latest) = crucible.get(&ingot.id) {
            ingot.work = latest.work.clone();
            ingot.proof = latest.proof.clone();
            ingot.grade = latest.grade;
            ingot.skill = latest.skill.clone();
            ingot.max = latest.max;
            ingot.smelt = latest.smelt;
            ingot.extra = latest.extra.clone();
        }
    }
}

fn verbose_heartbeat_secs() -> u64 {
    std::env::var("SLAG_VERBOSE_HEARTBEAT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_VERBOSE_HEARTBEAT_SECS)
}

fn log_parallel_heartbeat(
    pending_started: &HashMap<String, Instant>,
    last_completion: Instant,
    heartbeat_secs: u64,
) {
    let mut active: Vec<String> = pending_started
        .iter()
        .map(|(id, started)| format!("{id}:{}", tui::format_elapsed(started.elapsed().as_secs())))
        .collect();
    active.sort();
    let active_text = if active.is_empty() {
        "unknown".to_string()
    } else {
        active.join(", ")
    };

    tui::status_line(
        "…",
        tui::COLD,
        &format!(
            "verbose heartbeat ({}): no completions for {}; active anvils: {}",
            tui::format_elapsed(heartbeat_secs),
            tui::format_elapsed(last_completion.elapsed().as_secs()),
            active_text
        ),
    );
}

/// Invoke smith in a specific directory (worktree)
async fn invoke_smith_in_worktree(
    smith: &dyn Smith,
    prompt: &str,
    worktree_path: &str,
) -> Result<String, SlagError> {
    // The smith will work in the current directory, so we need to
    // modify the prompt to include worktree context
    let enhanced_prompt = format!(
        "WORKTREE: You are working in an isolated git worktree at: {worktree_path}\n\
        All file operations should be relative to this directory.\n\n\
        {prompt}"
    );
    smith
        .invoke_in_dir(&enhanced_prompt, Path::new(worktree_path))
        .await
}

/// Git commit in a specific directory (worktree)
async fn git_commit_in_dir(id: &str, work: &str, dir: &str) {
    let msg = format!("forge({id}): {work}");
    let _ = tokio::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(dir)
        .output()
        .await;
    let _ = tokio::process::Command::new("git")
        .args(["commit", "-m", &msg, "--quiet"])
        .current_dir(dir)
        .output()
        .await;
}

fn append_ledger(ingot: &Ingot, heat: u8) {
    let entry = format!(
        "\n## {} [{}] gr:{} skill:{}\n- {}\n- heats:{}\n",
        chrono::Local::now().format("%m-%d %H:%M"),
        ingot.id,
        ingot.grade,
        ingot.skill,
        ingot.work,
        heat,
    );
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(LEDGER)
        .and_then(|mut f| {
            use std::io::Write;
            f.write_all(entry.as_bytes())
        });
}

fn log_to_file(label: &str, content: &str) {
    let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let path = format!("{}/{ts}_{label}.log", crate::config::LOG_DIR);
    let _ = std::fs::write(&path, content);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sexp::{Ingot, Skill};
    use tempfile::NamedTempFile;

    fn mk_ingot(id: &str, status: Status) -> Ingot {
        Ingot {
            id: id.to_string(),
            status,
            solo: true,
            grade: 1,
            skill: Skill::Default,
            heat: 0,
            max: 5,
            smelt: 0,
            proof: "true".to_string(),
            work: "noop".to_string(),
            extra: vec![],
        }
    }

    #[test]
    fn recover_stale_molten_to_ore_requeues_only_molten() {
        let tmp = NamedTempFile::new().expect("tmp file");
        let mut crucible = Crucible::new(
            tmp.path(),
            vec![
                mk_ingot("i1", Status::Molten),
                mk_ingot("i2", Status::Ore),
                mk_ingot("i3", Status::Forged),
                mk_ingot("i4", Status::Molten),
            ],
        );

        let mut recovered = recover_stale_molten_to_ore(&mut crucible);
        recovered.sort();

        assert_eq!(recovered, vec!["i1".to_string(), "i4".to_string()]);
        assert_eq!(crucible.get("i1").expect("i1").status, Status::Ore);
        assert_eq!(crucible.get("i2").expect("i2").status, Status::Ore);
        assert_eq!(crucible.get("i3").expect("i3").status, Status::Forged);
        assert_eq!(crucible.get("i4").expect("i4").status, Status::Ore);
    }

    #[test]
    fn crack_stale_molten_marks_only_molten() {
        let tmp = NamedTempFile::new().expect("tmp file");
        let mut crucible = Crucible::new(
            tmp.path(),
            vec![
                mk_ingot("i1", Status::Molten),
                mk_ingot("i2", Status::Ore),
                mk_ingot("i3", Status::Forged),
            ],
        );

        crack_stale_molten(&mut crucible);

        assert_eq!(crucible.get("i1").expect("i1").status, Status::Cracked);
        assert_eq!(crucible.get("i2").expect("i2").status, Status::Ore);
        assert_eq!(crucible.get("i3").expect("i3").status, Status::Forged);
    }

    #[test]
    fn normalize_duplicate_ingot_ids_renames_following_duplicates() {
        let tmp = NamedTempFile::new().expect("tmp file");
        let mut crucible = Crucible::new(
            tmp.path(),
            vec![
                mk_ingot("r1", Status::Ore),
                mk_ingot("r1", Status::Ore),
                mk_ingot("r1", Status::Cracked),
            ],
        );
        let renamed = normalize_duplicate_ingot_ids(&mut crucible);
        assert_eq!(renamed.len(), 2);
        assert_eq!(crucible.ingots[0].id, "r1");
        assert_eq!(crucible.ingots[1].id, "r1_2");
        assert_eq!(crucible.ingots[2].id, "r1_3");
    }

    #[test]
    fn quarantine_invalid_pending_ingots_marks_only_pending_placeholders() {
        let tmp = NamedTempFile::new().expect("tmp file");
        let mut ok = mk_ingot("ok", Status::Ore);
        ok.proof = "cargo test --all".to_string();
        ok.work = "Run integration tests".to_string();

        let mut bad_pending = mk_ingot("bad_pending", Status::Ore);
        bad_pending.proof = "SHELL".to_string();

        let mut bad_molten = mk_ingot("bad_molten", Status::Molten);
        bad_molten.work = "task".to_string();

        let mut bad_forged = mk_ingot("bad_forged", Status::Forged);
        bad_forged.proof = "SHELL".to_string();

        let mut crucible = Crucible::new(tmp.path(), vec![ok, bad_pending, bad_molten, bad_forged]);
        let quarantined = quarantine_invalid_pending_ingots(&mut crucible);

        assert_eq!(quarantined.len(), 2);
        assert_eq!(crucible.get("ok").expect("ok").status, Status::Ore);
        assert_eq!(
            crucible.get("bad_pending").expect("bad_pending").status,
            Status::Cracked
        );
        assert_eq!(
            crucible.get("bad_molten").expect("bad_molten").status,
            Status::Cracked
        );
        assert_eq!(
            crucible.get("bad_forged").expect("bad_forged").status,
            Status::Forged
        );
    }
}
