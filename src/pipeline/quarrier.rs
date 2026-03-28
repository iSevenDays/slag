use crate::config::{ORE_FILE, PHASES_FILE};
use crate::error::SlagError;
use crate::flux;
use crate::smith::Smith;
use crate::tui;

/// A build phase extracted by the quarrier.
#[derive(Debug, Clone)]
pub struct Phase {
    pub name: String,
    pub description: String,
    pub criteria: String,
}

impl Phase {
    /// Single pass-through phase (no chunking).
    pub fn single_phase() -> Vec<Phase> {
        vec![Phase {
            name: "Full Commission".into(),
            description: "Execute the entire commission in a single pass".into(),
            criteria: String::new(),
        }]
    }
}

/// Run the quarrier: decompose a large commission into ordered build phases.
pub async fn run(smith: &dyn Smith, verbose: bool) -> Result<Vec<Phase>, SlagError> {
    tui::header("QUARRIER \u{00b7} extracting ore veins");

    let ore = std::fs::read_to_string(ORE_FILE).map_err(|_| SlagError::NoOre)?;

    let prompt = flux::quarrier_prompt(&ore);
    log_to_file("QUARRY_PROMPT", &prompt);

    let spinner = tui::spinner("quarrying...");
    let raw = smith.invoke(&prompt).await.map_err(|e| {
        spinner.finish_and_clear();
        SlagError::QuarryFailed(e.to_string())
    })?;
    spinner.finish_and_clear();

    log_to_file("QUARRY_RAW", &raw);

    let phases = parse_phases(&raw);

    if phases.is_empty() {
        if verbose {
            println!("  \x1b[90mquarrier produced no parseable phases, falling back to single pass\x1b[0m");
        }
        return Ok(Phase::single_phase());
    }

    // Write PHASES.md for resume support
    write_phases_manifest(&phases)?;

    tui::status_line(
        "\u{2588}",
        tui::PURE,
        &format!("Quarried {} phase(s)", phases.len()),
    );

    for (i, phase) in phases.iter().enumerate() {
        println!(
            "  \x1b[90m{}. {} \u{2014} {}\x1b[0m",
            i + 1,
            phase.name,
            phase.description
        );
    }

    Ok(phases)
}

/// Parse PHASE:/DESC:/CRITERIA: blocks separated by ---.
fn parse_phases(raw: &str) -> Vec<Phase> {
    let mut phases = Vec::new();
    let mut name = String::new();
    let mut desc = String::new();
    let mut criteria = String::new();

    for line in raw.lines() {
        let trimmed = line.trim();

        if trimmed == "---" || trimmed == "---\n" {
            if !name.is_empty() {
                phases.push(Phase {
                    name: name.clone(),
                    description: desc.clone(),
                    criteria: criteria.clone(),
                });
                name.clear();
                desc.clear();
                criteria.clear();
            }
            continue;
        }

        if let Some(val) = trimmed.strip_prefix("PHASE:") {
            name = val.trim().to_string();
        } else if let Some(val) = trimmed.strip_prefix("DESC:") {
            desc = val.trim().to_string();
        } else if let Some(val) = trimmed.strip_prefix("CRITERIA:") {
            criteria = val.trim().to_string();
        }
    }

    // Capture trailing phase (no final ---)
    if !name.is_empty() {
        phases.push(Phase {
            name,
            description: desc,
            criteria,
        });
    }

    // Sanity: 2-5 phases
    if phases.len() < 2 || phases.len() > 10 {
        return Vec::new();
    }

    phases
}

/// Write PHASES.md manifest for resume support.
fn write_phases_manifest(phases: &[Phase]) -> Result<(), SlagError> {
    let mut content = String::from("# Quarried Phases\n\n");
    for (i, phase) in phases.iter().enumerate() {
        content.push_str(&format!(
            "## Phase {}: {}\n{}\nCriteria: {}\n\n",
            i + 1,
            phase.name,
            phase.description,
            phase.criteria
        ));
    }
    std::fs::write(PHASES_FILE, content)?;
    Ok(())
}

/// Load phases from an existing PHASES.md (resume support).
pub fn load_phases() -> Option<Vec<Phase>> {
    let content = std::fs::read_to_string(PHASES_FILE).ok()?;
    let mut phases = Vec::new();
    let mut name = String::new();
    let mut desc = String::new();
    let mut criteria = String::new();

    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("## Phase ") {
            // Flush previous
            if !name.is_empty() {
                phases.push(Phase {
                    name: name.clone(),
                    description: desc.clone(),
                    criteria: criteria.clone(),
                });
                desc.clear();
                criteria.clear();
            }
            // "1: Foundation Setup" -> "Foundation Setup"
            name = rest
                .split_once(": ")
                .map(|(_, n)| n.to_string())
                .unwrap_or_else(|| rest.to_string());
        } else if let Some(val) = line.strip_prefix("Criteria: ") {
            criteria = val.to_string();
        } else if !line.starts_with('#')
            && !line.is_empty()
            && name.is_empty().not()
            && desc.is_empty()
        {
            desc = line.to_string();
        }
    }

    // Flush last
    if !name.is_empty() {
        phases.push(Phase {
            name,
            description: desc,
            criteria,
        });
    }

    if phases.len() >= 2 {
        Some(phases)
    } else {
        None
    }
}

/// Helper trait for bool negation in expression position.
trait Not {
    fn not(&self) -> bool;
}
impl Not for bool {
    fn not(&self) -> bool {
        !self
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

    #[test]
    fn parse_phases_basic() {
        let input = "\
PHASE: Foundation
DESC: Set up project structure and dependencies
CRITERIA: test -f package.json
---
PHASE: Core Logic
DESC: Implement main business logic
CRITERIA: npm test
---
PHASE: Polish
DESC: Add error handling and documentation
CRITERIA: npm run lint";

        let phases = parse_phases(input);
        assert_eq!(phases.len(), 3);
        assert_eq!(phases[0].name, "Foundation");
        assert_eq!(phases[1].name, "Core Logic");
        assert_eq!(phases[2].name, "Polish");
        assert_eq!(phases[2].criteria, "npm run lint");
    }

    #[test]
    fn parse_phases_rejects_single() {
        let input = "\
PHASE: Everything
DESC: Do it all
CRITERIA: test -f done";

        let phases = parse_phases(input);
        assert!(phases.is_empty(), "single phase should be rejected");
    }

    #[test]
    fn parse_phases_handles_noise() {
        let input = "\
Some preamble text
PHASE: Setup
DESC: Init project
CRITERIA: test -d src
---
extra noise here
PHASE: Build
DESC: Build everything
CRITERIA: npm run build";

        let phases = parse_phases(input);
        assert_eq!(phases.len(), 2);
    }

    #[test]
    fn single_phase_returns_one() {
        let phases = Phase::single_phase();
        assert_eq!(phases.len(), 1);
        assert_eq!(phases[0].name, "Full Commission");
    }
}
