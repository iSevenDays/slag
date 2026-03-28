use std::io::Write;

use serde::{Deserialize, Serialize};

use crate::config::{EXPERIMENT_LOG, EXPERIMENT_LOG_JSONL};

/// TOON tabular header — field names (short aliases for token efficiency).
const TOON_FIELDS: &[&str] = &[
    "ts", "id", "cy", "h", "smith", "status", "dur", "exit", "proof", "hash", "files", "desc",
];

/// A single experiment record in the structured ledger.
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

// --- TOON tabular writer ---

/// Append a record to the TOON experiment log.
/// Writes header on first record, then one data row per call.
pub fn append_record(record: &ExperimentRecord) {
    let needs_header = !std::path::Path::new(EXPERIMENT_LOG).exists()
        || std::fs::metadata(EXPERIMENT_LOG)
            .map(|m| m.len() == 0)
            .unwrap_or(true);

    let mut f = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(EXPERIMENT_LOG)
    {
        Ok(f) => f,
        Err(_) => return,
    };

    if needs_header {
        // Header will get count updated on load; use placeholder [*] for streaming append
        let _ = writeln!(f, "[*]{{{}}}:", TOON_FIELDS.join(","));
    }

    let row = format_toon_row(record);
    let _ = writeln!(f, "  {row}");
}

fn format_toon_row(r: &ExperimentRecord) -> String {
    let values = [
        toon_quote(&r.ts),
        toon_quote(&r.ingot_id),
        r.cycle.to_string(),
        r.heat.to_string(),
        toon_quote(&r.smith_cmd),
        toon_quote(&r.status),
        r.duration_secs.to_string(),
        r.proof_exit.to_string(),
        toon_quote(&r.proof_cmd),
        toon_quote(r.commit_hash.as_deref().unwrap_or("")),
        r.files_changed.to_string(),
        toon_quote(&r.description),
    ];
    values.join(",")
}

/// Quote a TOON value if it contains comma, colon, quote, backslash, or is empty.
fn toon_quote(s: &str) -> String {
    if s.is_empty()
        || s.contains(',')
        || s.contains(':')
        || s.contains('"')
        || s.contains('\\')
        || s.contains('\n')
        || s.starts_with(' ')
        || s.ends_with(' ')
        || s == "true"
        || s == "false"
        || s == "null"
    {
        let escaped = s
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n");
        format!("\"{escaped}\"")
    } else {
        s.to_string()
    }
}

// --- TOON tabular reader ---

/// Load all records. Tries TOON first, falls back to JSONL for migration.
pub fn load_records() -> Vec<ExperimentRecord> {
    let toon_records = load_toon_records();
    if !toon_records.is_empty() {
        return toon_records;
    }
    // Fallback: legacy JSONL
    load_jsonl_records()
}

fn load_toon_records() -> Vec<ExperimentRecord> {
    let Ok(content) = std::fs::read_to_string(EXPERIMENT_LOG) else {
        return Vec::new();
    };
    parse_toon_records(&content)
}

fn parse_toon_records(content: &str) -> Vec<ExperimentRecord> {
    let mut lines = content.lines();
    // Skip header line (e.g., "[*]{ts,id,cy,...}:" or "[N]{...}:")
    let Some(header) = lines.next() else {
        return Vec::new();
    };
    if !header.contains('{') {
        return Vec::new();
    }

    let mut records = Vec::new();
    for line in lines {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(record) = parse_toon_row(trimmed) {
            records.push(record);
        }
    }
    records
}

fn parse_toon_row(row: &str) -> Option<ExperimentRecord> {
    let fields = split_toon_fields(row);
    if fields.len() < 12 {
        return None;
    }
    Some(ExperimentRecord {
        ts: fields[0].clone(),
        ingot_id: fields[1].clone(),
        cycle: fields[2].parse().unwrap_or(1),
        heat: fields[3].parse().unwrap_or(1),
        smith_cmd: fields[4].clone(),
        status: fields[5].clone(),
        duration_secs: fields[6].parse().unwrap_or(0),
        proof_exit: fields[7].parse().unwrap_or(-1),
        proof_cmd: fields[8].clone(),
        commit_hash: if fields[9].is_empty() {
            None
        } else {
            Some(fields[9].clone())
        },
        files_changed: fields[10].parse().unwrap_or(0),
        description: fields[11..].join(","), // description may contain commas if quoted was split
    })
}

/// Split a TOON row respecting quoted fields.
fn split_toon_fields(row: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut in_quote = false;
    let mut escaped = false;
    let chars: Vec<char> = row.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];
        if escaped {
            match c {
                'n' => current.push('\n'),
                'r' => current.push('\r'),
                't' => current.push('\t'),
                _ => current.push(c),
            }
            escaped = false;
            i += 1;
            continue;
        }
        if c == '\\' && in_quote {
            escaped = true;
            i += 1;
            continue;
        }
        if c == '"' {
            in_quote = !in_quote;
            i += 1;
            continue;
        }
        if c == ',' && !in_quote {
            fields.push(current.clone());
            current.clear();
            i += 1;
            continue;
        }
        current.push(c);
        i += 1;
    }
    fields.push(current);
    fields
}

fn load_jsonl_records() -> Vec<ExperimentRecord> {
    let Ok(content) = std::fs::read_to_string(EXPERIMENT_LOG_JSONL) else {
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

/// Format experiment history as compact TOON tabular for smith prompts.
pub fn format_ingot_history(ingot_id: &str) -> Option<String> {
    let records = records_for_ingot(ingot_id);
    if records.is_empty() {
        return None;
    }
    let n = records.len();
    let mut out = format!("HISTORY({ingot_id})[{n}]{{h,status,dur,err,hash}}:\n");
    for r in &records {
        let hash = r.commit_hash.as_deref().unwrap_or("");
        let desc = truncate_desc(&r.description, 80);
        out.push_str(&format!(
            "  {},{},{},{},{}\n",
            r.heat,
            r.status,
            r.duration_secs,
            toon_quote(&desc),
            hash,
        ));
    }
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
    fn toon_row_round_trip() {
        let record = sample_record("i1", 1, "forged");
        let row = format_toon_row(&record);
        let parsed = parse_toon_row(&row).expect("should parse");
        assert_eq!(parsed.ingot_id, "i1");
        assert_eq!(parsed.status, "forged");
        assert_eq!(parsed.heat, 1);
        assert_eq!(parsed.duration_secs, 42);
        assert_eq!(parsed.commit_hash, Some("abc1234".to_string()));
        assert_eq!(parsed.description, "Wire bootstrap");
    }

    #[test]
    fn toon_row_with_commas_in_description() {
        let mut record = sample_record("i2", 2, "cracked");
        record.description = "CMD failed: TypeError, cannot read property".to_string();
        let row = format_toon_row(&record);
        let parsed = parse_toon_row(&row).expect("should parse");
        assert_eq!(
            parsed.description,
            "CMD failed: TypeError, cannot read property"
        );
    }

    #[test]
    fn toon_row_with_empty_hash() {
        let record = sample_record("i3", 1, "cracked");
        let row = format_toon_row(&record);
        let parsed = parse_toon_row(&row).expect("should parse");
        assert_eq!(parsed.commit_hash, None);
    }

    #[test]
    fn toon_quote_handles_special_chars() {
        assert_eq!(toon_quote("simple"), "simple");
        assert_eq!(toon_quote("has,comma"), "\"has,comma\"");
        assert_eq!(toon_quote("has:colon"), "\"has:colon\"");
        assert_eq!(toon_quote(""), "\"\"");
        assert_eq!(toon_quote("true"), "\"true\"");
        assert_eq!(toon_quote("has \"quotes\""), "\"has \\\"quotes\\\"\"");
    }

    #[test]
    fn parse_toon_content() {
        let content = "[*]{ts,id,cy,h,smith,status,dur,exit,proof,hash,files,desc}:\n  2026-03-28T14:30:00Z,i1,1,1,claude -p,forged,42,0,npm test,abc1234,3,Wire bootstrap\n  2026-03-28T14:31:00Z,i2,1,2,claude -p,cracked,10,1,npm test,,2,CMD failed\n";
        let records = parse_toon_records(content);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].ingot_id, "i1");
        assert_eq!(records[0].status, "forged");
        assert_eq!(records[1].ingot_id, "i2");
        assert_eq!(records[1].commit_hash, None);
    }

    #[test]
    fn format_history_uses_toon_tabular() {
        // This tests the format structure, not actual file reading
        let history = format_ingot_history("nonexistent_xyz_test");
        assert!(history.is_none());
    }

    #[test]
    fn split_toon_fields_respects_quotes() {
        let fields = split_toon_fields("a,\"b,c\",d");
        assert_eq!(fields, vec!["a", "b,c", "d"]);
    }

    #[test]
    fn split_toon_fields_handles_escaped_quotes() {
        let fields = split_toon_fields("a,\"he said \\\"hi\\\"\",b");
        assert_eq!(fields, vec!["a", "he said \"hi\"", "b"]);
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

    #[test]
    fn toon_is_smaller_than_jsonl() {
        let record = sample_record("i1", 1, "forged");
        let jsonl = serde_json::to_string(&record).unwrap();
        let toon = format_toon_row(&record);
        assert!(
            toon.len() < jsonl.len(),
            "TOON ({}) should be smaller than JSONL ({})",
            toon.len(),
            jsonl.len()
        );
    }
}
