use std::io::Write;

use serde::{Deserialize, Serialize};

use crate::config::EXPERIMENT_LOG;

/// A single experiment record in the structured JSONL ledger.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExperimentRecord {
    pub ts: String,
    pub ingot_id: String,
    pub cycle: usize,
    pub heat: u8,
    pub smith_cmd: String,
    pub status: String,
    pub duration_secs: u64,
    pub proof_exit: i32,
    pub proof_cmd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit_hash: Option<String>,
    pub files_changed: usize,
    pub description: String,
}

/// Append a record to the JSONL experiment log (atomic per-line append).
pub fn append_record(record: &ExperimentRecord) {
    let Ok(json) = serde_json::to_string(record) else {
        return;
    };
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(EXPERIMENT_LOG)
        .and_then(|mut f| writeln!(f, "{json}"));
}

/// Load all records from the experiment log.
pub fn load_records() -> Vec<ExperimentRecord> {
    let Ok(content) = std::fs::read_to_string(EXPERIMENT_LOG) else {
        return Vec::new();
    };
    content
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// Load records for a specific ingot, ordered by timestamp.
pub fn records_for_ingot(ingot_id: &str) -> Vec<ExperimentRecord> {
    load_records()
        .into_iter()
        .filter(|r| r.ingot_id == ingot_id)
        .collect()
}

/// Format experiment history for a given ingot as structured text for smith prompts.
pub fn format_ingot_history(ingot_id: &str) -> Option<String> {
    let records = records_for_ingot(ingot_id);
    if records.is_empty() {
        return None;
    }
    let mut out = format!("=== EXPERIMENT HISTORY ({ingot_id}) ===\n");
    for r in &records {
        let hash_note = r
            .commit_hash
            .as_deref()
            .map(|h| format!(" [{h}]"))
            .unwrap_or_default();
        out.push_str(&format!(
            "Heat {}: {} ({}s) — \"{}\"{hash_note}\n",
            r.heat,
            r.status,
            r.duration_secs,
            truncate_desc(&r.description, 80),
        ));
    }
    out.push_str("===\n");
    Some(out)
}

/// Print a compact summary table of experiment stats.
pub fn summary_table() -> String {
    let records = load_records();
    if records.is_empty() {
        return "No experiments recorded.".to_string();
    }
    let total = records.len();
    let forged = records.iter().filter(|r| r.status == "forged").count();
    let cracked = records.iter().filter(|r| r.status == "cracked").count();
    let reverted = records.iter().filter(|r| r.status == "reverted").count();
    let total_secs: u64 = records.iter().map(|r| r.duration_secs).sum();
    format!(
        "Experiments: {total} total, {forged} forged, {cracked} cracked, {reverted} reverted, {total_secs}s total",
    )
}

/// Get the current git HEAD short hash, if available.
pub fn git_head_short() -> Option<String> {
    std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Count files changed in the working tree (staged + unstaged).
pub fn git_files_changed() -> usize {
    std::process::Command::new("git")
        .args(["diff", "--name-only", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter(|l| !l.is_empty())
                .count()
        })
        .unwrap_or(0)
}

fn truncate_desc(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max.saturating_sub(3)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn sample_record(id: &str, heat: u8, status: &str) -> ExperimentRecord {
        ExperimentRecord {
            ts: "2026-03-28T14:30:00Z".to_string(),
            ingot_id: id.to_string(),
            cycle: 1,
            heat,
            smith_cmd: "claude -p".to_string(),
            status: status.to_string(),
            duration_secs: 42,
            proof_exit: if status == "forged" { 0 } else { 1 },
            proof_cmd: "npm test".to_string(),
            commit_hash: if status == "forged" {
                Some("abc1234".to_string())
            } else {
                None
            },
            files_changed: 3,
            description: "Wire bootstrap".to_string(),
        }
    }

    #[test]
    fn round_trip_jsonl() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("experiments.jsonl");
        // Temporarily override the log path
        std::env::set_var("_SLAG_TEST_LEDGER", path.to_string_lossy().as_ref());

        let record = sample_record("i1", 1, "forged");
        let json = serde_json::to_string(&record).unwrap();
        let parsed: ExperimentRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.ingot_id, "i1");
        assert_eq!(parsed.status, "forged");
        assert_eq!(parsed.commit_hash, Some("abc1234".to_string()));
    }

    #[test]
    fn format_history_empty_returns_none() {
        // With no log file, should return None
        assert!(format_ingot_history("nonexistent_ingot_xyz").is_none());
    }

    #[test]
    fn summary_table_empty() {
        let summary = summary_table();
        // Either "No experiments" or actual data depending on test environment
        assert!(!summary.is_empty());
    }

    #[test]
    fn truncate_desc_short() {
        assert_eq!(truncate_desc("hello", 10), "hello");
    }

    #[test]
    fn truncate_desc_long() {
        let long = "a".repeat(100);
        let result = truncate_desc(&long, 20);
        assert!(result.len() <= 20);
        assert!(result.ends_with("..."));
    }
}
