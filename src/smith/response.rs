use crate::events::FailureClass;

/// Case-insensitive header extractor. Accepts:
///   `KEY: value`     (plain)
///   `**KEY:** value` (markdown bold)
///   `key: value`     (lowercase)
///   `KEY: value` inside a fenced block (ignores the fence lines themselves)
/// Returns the trimmed value after the colon, or None if key not found.
pub fn extract_header(text: &str, key: &str) -> Option<String> {
    let key_lower = key.to_ascii_lowercase();
    for line in text.lines() {
        let trimmed = line.trim();
        // Skip fenced-block delimiter lines
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            continue;
        }
        // Strip markdown bold markers. Patterns:
        //   **KEY:** value   -> strip leading ** and the ** immediately after the colon
        //   **KEY: value**   -> strip leading ** and trailing **
        // Strategy: strip leading **, then after matching the key: prefix,
        // strip any leading ** from the value (handles "KEY:** value").
        let stripped = trimmed.trim_start_matches("**");
        // Case-insensitive key match against "key:"
        let line_lower = stripped.to_ascii_lowercase();
        let prefix = format!("{}:", key_lower);
        if line_lower.starts_with(&prefix) {
            let colon_pos = stripped.find(':').unwrap_or(0);
            let value = stripped[colon_pos + 1..].trim();
            // Strip markdown bold markers that may wrap the value (**value** or ** value)
            let value = value.trim_start_matches("**").trim();
            let value = value.trim_end_matches("**").trim().to_string();
            return Some(value);
        }
    }
    None
}

/// Convenience wrapper: returns Ok(value) or Err(class) if the header is absent.
pub fn extract_header_as_result(
    text: &str,
    key: &str,
    class: FailureClass,
) -> Result<String, FailureClass> {
    extract_header(text, key).ok_or(class)
}

/// Return the first action keyword from a closed set found in the response.
/// Accepts plain `KEYWORD:` or `**KEYWORD:**` prefixes (case-insensitive match
/// against the uppercase candidate list).
/// Returns the first match (defensive against models that hedge with two keywords).
pub fn extract_action_keyword<'a>(text: &str, candidates: &[&'a str]) -> Option<&'a str> {
    for line in text.lines() {
        let trimmed = line.trim();
        // Strip markdown bold markers
        let stripped = trimmed.trim_start_matches("**");
        let stripped = stripped.trim_end_matches("**");
        let upper = stripped.to_ascii_uppercase();
        for &candidate in candidates {
            let prefix = format!("{}:", candidate);
            if upper.starts_with(&prefix) || upper == candidate {
                return Some(candidate);
            }
        }
    }
    None
}

/// Return the LAST `CMD:` line in the response (matches existing extract_cmd semantics).
/// Strips fenced-block wrappers on both ends (``` CMD: ... ``` or ~~~ CMD: ... ~~~).
/// Returns None if no CMD: line is found or the value is a placeholder.
pub fn extract_trailing_cmd(text: &str) -> Option<String> {
    let cmd = text
        .lines()
        .rev()
        .find(|line| {
            let trimmed = line.trim();
            let upper = trimmed.to_ascii_uppercase();
            upper.starts_with("CMD:")
                || upper.starts_with("```CMD:")
                || upper.starts_with("~~~CMD:")
        })
        .map(|line| {
            let trimmed = line.trim();
            // Strip code-fence prefix and any trailing fence on the same line.
            let stripped = trimmed
                .trim_start_matches("```")
                .trim_start_matches("~~~")
                .trim();
            let value = stripped.split_once(':').map(|x| x.1).unwrap_or("").trim();
            // Strip trailing fence — value may end with ``` or ~~~ when the
            // model put the close fence on the same line as CMD:.
            value
                .trim_end_matches("```")
                .trim_end_matches("~~~")
                .trim()
                .to_string()
        })?;

    if is_protocol_placeholder_cmd(&cmd) {
        return None;
    }
    Some(cmd)
}

/// Detect placeholder / hallucinated CMD values.
/// Relocated from src/pipeline/forge.rs so all parser modules share one definition.
pub fn is_protocol_placeholder_cmd(cmd: &str) -> bool {
    let trimmed = cmd.trim();
    if trimmed.is_empty() {
        return true;
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.contains("<shell command") {
        return true;
    }
    if lower.contains("line in response") {
        return true;
    }
    if lower.contains("missing \"cmd:\"") || lower.contains("no cmd") {
        return true;
    }
    if lower.contains("analyze and fix") {
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_header_plain() {
        assert_eq!(
            extract_header("STATUS: PASS\nother", "STATUS"),
            Some("PASS".to_string())
        );
    }

    #[test]
    fn extract_header_markdown_bold() {
        assert_eq!(
            extract_header("**STATUS:** PASS", "STATUS"),
            Some("PASS".to_string())
        );
    }

    #[test]
    fn extract_header_case_insensitive() {
        assert_eq!(
            extract_header("status: pass", "STATUS"),
            Some("pass".to_string())
        );
    }

    #[test]
    fn extract_header_in_fenced_block() {
        // Fence delimiter lines are skipped; the content line inside is found
        let text = "```\nSTATUS: PASS\n```";
        assert_eq!(extract_header(text, "STATUS"), Some("PASS".to_string()));
    }

    #[test]
    fn extract_header_absent() {
        assert_eq!(extract_header("no header here", "STATUS"), None);
    }

    #[test]
    fn extract_header_value_with_colon() {
        // Values that contain colons should be returned in full
        assert_eq!(
            extract_header("COMMENT: foo: bar", "COMMENT"),
            Some("foo: bar".to_string())
        );
    }

    #[test]
    fn extract_action_keyword_finds_impossible() {
        let text = "**IMPOSSIBLE:** budget exhausted";
        assert_eq!(
            extract_action_keyword(text, &["REWRITE", "SPLIT", "IMPOSSIBLE"]),
            Some("IMPOSSIBLE")
        );
    }

    #[test]
    fn extract_action_keyword_plain() {
        let text = "IMPOSSIBLE: budget";
        assert_eq!(
            extract_action_keyword(text, &["REWRITE", "SPLIT", "IMPOSSIBLE"]),
            Some("IMPOSSIBLE")
        );
    }

    #[test]
    fn extract_action_keyword_returns_first_match() {
        let text = "REWRITE: try again\nIMPOSSIBLE: also true";
        // First line wins
        assert_eq!(
            extract_action_keyword(text, &["REWRITE", "SPLIT", "IMPOSSIBLE"]),
            Some("REWRITE")
        );
    }

    #[test]
    fn extract_action_keyword_absent() {
        assert_eq!(
            extract_action_keyword("some random text", &["REWRITE", "SPLIT", "IMPOSSIBLE"]),
            None
        );
    }

    #[test]
    fn extract_trailing_cmd_returns_last() {
        let text = "CMD: echo first\nsome output\nCMD: echo last";
        assert_eq!(
            extract_trailing_cmd(text),
            Some("echo last".to_string())
        );
    }

    #[test]
    fn extract_trailing_cmd_rejects_placeholder() {
        let text = "CMD: <shell command to verify>";
        assert_eq!(extract_trailing_cmd(text), None);
    }

    #[test]
    fn extract_trailing_cmd_rejects_empty() {
        let text = "CMD: ";
        assert_eq!(extract_trailing_cmd(text), None);
    }

    #[test]
    fn extract_trailing_cmd_strips_both_fences() {
        let text = "```CMD: cargo test```";
        assert_eq!(extract_trailing_cmd(text), Some("cargo test".to_string()));
    }

    #[test]
    fn extract_trailing_cmd_strips_tilde_fence() {
        let text = "~~~CMD: cargo build~~~";
        assert_eq!(extract_trailing_cmd(text), Some("cargo build".to_string()));
    }

    #[test]
    fn is_protocol_placeholder_cmd_detects_shell_template() {
        assert!(is_protocol_placeholder_cmd("<shell command to verify>"));
        assert!(is_protocol_placeholder_cmd(""));
        assert!(is_protocol_placeholder_cmd("   "));
        assert!(!is_protocol_placeholder_cmd("cargo test"));
        assert!(!is_protocol_placeholder_cmd("echo hello"));
    }

    #[test]
    fn is_protocol_placeholder_cmd_detects_no_cmd() {
        assert!(is_protocol_placeholder_cmd("no cmd found"));
        assert!(is_protocol_placeholder_cmd("analyze and fix the issue"));
    }
}
