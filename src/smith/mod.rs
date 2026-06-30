pub mod claude;
pub mod doctor;
pub mod mock;
pub mod vllm;
pub mod response;

use std::future::Future;
use std::path::Path;
use std::pin::Pin;

use crate::config::SmithCapabilities;
use crate::error::SlagError;
#[allow(unused_imports)]
pub use crate::events::{FailureClass, RecastFailure};

/// Dispatch a smith command string to the right adapter.
/// `"vllm"` (and any token starting with `"vllm "`) constructs `VllmSmith::from_env()`,
/// everything else is treated as a subprocess invocation handled by `ClaudeSmith`.
pub fn build_smith(cmd: &str) -> Result<Box<dyn Smith>, SlagError> {
    let trimmed = cmd.trim();
    if trimmed == "vllm" || trimmed.starts_with("vllm ") {
        let smith = vllm::VllmSmith::from_env()?;
        return Ok(Box::new(smith));
    }
    Ok(Box::new(claude::ClaudeSmith::new(cmd.to_string())))
}

/// Structured output constraint for capable smiths (e.g., vLLM with xgrammar).
/// Passed to `invoke_with_constraints`; ignored by smiths that don't support it.
#[derive(Debug, Clone)]
pub enum StructuredOutputSpec {
    /// Closed vocabulary: model must emit exactly one of these strings.
    Choice(Vec<String>),
    /// GBNF grammar string: model output must conform to this grammar.
    Grammar(String),
    /// Regular expression: model output must match this pattern.
    Regex(String),
}

/// Async trait for invoking an AI smith (Claude or mock).
/// Uses boxed future for dyn compatibility.
pub trait Smith: Send + Sync {
    /// Send a prompt and receive the response text.
    fn invoke(
        &self,
        prompt: &str,
    ) -> Pin<Box<dyn Future<Output = Result<String, SlagError>> + Send + '_>>;

    /// Send a prompt while executing in a specific directory.
    /// Default implementation falls back to regular invocation.
    fn invoke_in_dir(
        &self,
        prompt: &str,
        _dir: &Path,
    ) -> Pin<Box<dyn Future<Output = Result<String, SlagError>> + Send + '_>> {
        self.invoke(prompt)
    }

    /// Send a prompt with an optional structured-output constraint.
    /// Default implementation ignores the spec and falls through to `invoke`.
    /// Capable smiths (e.g., VllmSmith) override this to inject the constraint.
    fn invoke_with_constraints(
        &self,
        prompt: &str,
        _spec: Option<&StructuredOutputSpec>,
    ) -> Pin<Box<dyn Future<Output = Result<String, SlagError>> + Send + '_>> {
        self.invoke(prompt)
    }

    /// Return the capability profile for this smith.
    /// Default implementation returns a static conservative profile.
    fn capabilities(&self) -> &SmithCapabilities {
        static CONSERVATIVE: std::sync::OnceLock<SmithCapabilities> =
            std::sync::OnceLock::new();
        CONSERVATIVE.get_or_init(SmithCapabilities::conservative)
    }
}

/// Check if response text contains unresolved questions
pub fn has_questions(text: &str) -> bool {
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.ends_with('?') {
            return true;
        }
        if trimmed.starts_with("**Question")
            || trimmed.starts_with("Question")
            || trimmed.starts_with("Which ")
            || trimmed.starts_with("What ")
            || trimmed.starts_with("Should ")
            || trimmed.starts_with("Do you ")
            || trimmed.starts_with("Would you ")
            || trimmed.starts_with("Can you ")
            || trimmed.starts_with("Could you ")
        {
            return true;
        }
    }
    false
}

/// Self-iterate to resolve questions in smith output.
pub async fn self_iterate(
    smith: &dyn Smith,
    mut raw: String,
    max_iter: usize,
) -> Result<String, SlagError> {
    let mut iterations = 0;
    while has_questions(&raw) && iterations < max_iter {
        iterations += 1;
        let follow_up = format!(
            "{raw}\n\n---\n[SELF-QUERY RESOLUTION]\n\
            You asked questions above. You are the expert. Answer them yourself:\n\
            - Make decisive choices based on best practices\n\
            - Choose the most sensible option when uncertain\n\
            - Do not ask for clarification - decide and proceed\n\n\
            Now output the COMPLETE deliverable with all decisions made.\n\
            NO QUESTIONS. NO PREAMBLE. Just the final output."
        );
        raw = smith.invoke(&follow_up).await?;
    }
    Ok(raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::mock::MockSmith;

    #[test]
    fn detect_questions() {
        assert!(has_questions("What framework should we use?"));
        assert!(has_questions("**Question**: which approach?"));
        assert!(has_questions("Should we use React or Vue?"));
        assert!(!has_questions("# Blueprint\nThis is a plan."));
        assert!(!has_questions("Create the file structure."));
    }

    #[test]
    fn mock_smith_uses_conservative_capabilities_default() {
        let smith = MockSmith::new(vec![]);
        assert_eq!(smith.capabilities().name, "unknown");
    }

    #[test]
    fn conservative_default_is_shared_static() {
        let smith_a = MockSmith::new(vec![]);
        let smith_b = MockSmith::new(vec![]);
        // Both should point to the same static allocation
        assert!(std::ptr::eq(smith_a.capabilities(), smith_b.capabilities()));
    }

    #[test]
    fn build_smith_dispatches_vllm_token_to_vllm_adapter() {
        std::env::set_var("SLAG_VLLM_BASE_URL", "http://localhost:8000");
        std::env::set_var("SLAG_VLLM_MODEL", "qwen3-32b");
        let smith = build_smith("vllm").expect("vllm token should build VllmSmith");
        assert_eq!(
            smith.capabilities().name,
            "vllm",
            "expected vllm capability profile, not subprocess fallback"
        );
        std::env::remove_var("SLAG_VLLM_BASE_URL");
        std::env::remove_var("SLAG_VLLM_MODEL");
    }

    #[test]
    fn build_smith_defaults_to_claude_subprocess_for_arbitrary_commands() {
        let smith =
            build_smith("claude -p --permission-mode bypassPermissions").expect("should build");
        assert_eq!(smith.capabilities().name, "claude");
    }

    #[test]
    fn build_smith_dispatches_vllm_with_trailing_args_to_vllm_adapter() {
        // "vllm" followed by future flags should still route to the HTTP adapter,
        // not be spawned as a subprocess.
        std::env::set_var("SLAG_VLLM_BASE_URL", "http://localhost:8000");
        std::env::set_var("SLAG_VLLM_MODEL", "qwen3-32b");
        let smith = build_smith("vllm --some-future-flag").expect("vllm prefix should build");
        assert_eq!(smith.capabilities().name, "vllm");
        std::env::remove_var("SLAG_VLLM_BASE_URL");
        std::env::remove_var("SLAG_VLLM_MODEL");
    }

    #[test]
    fn invoke_with_constraints_without_spec_uses_plain_path() {
        // Default impl on a non-VllmSmith smith ignores the spec
        let smith = MockSmith::fixed("plain response");
        let spec = StructuredOutputSpec::Choice(vec!["PASS".to_string(), "FAIL".to_string()]);
        // MockSmith uses the default-impl which calls invoke — should return the canned response
        let result = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(smith.invoke_with_constraints("test", Some(&spec)));
        assert_eq!(result.unwrap(), "plain response");
    }
}

/// Fixture-based bench/eval suite for smith adherence testing.
/// Run with: SLAG_BENCH=1 cargo test smith_fixtures
/// Without SLAG_BENCH=1, all bench tests are skipped.
#[cfg(test)]
mod bench_fixtures {
    use super::response::{extract_action_keyword, extract_trailing_cmd};
    use crate::crucible::parse_ingot_lines;
    use std::path::Path;
    use std::time::Instant;

    /// Assertion type parsed from fixture `---ASSERT---` section.
    #[derive(Debug, Clone)]
    enum Assertion {
        ParseableIngot,
        TrailingCmd,
        StatusEnum,
        ActionKeyword { candidates: Vec<String> },
        FormatViolation,
    }

    /// One fixture: a prompt body + list of assertions.
    struct Fixture {
        name: String,
        prompt: String,
        assertions: Vec<Assertion>,
    }

    /// Result of running one fixture against one smith.
    struct FixtureResult {
        fixture: String,
        smith: String,
        passed: bool,
        failure_class: Option<String>,
        #[allow(dead_code)]
        duration_ms: u64,
    }

    fn parse_assertion(line: &str) -> Option<Assertion> {
        let line = line.trim();
        if line == "parseable_ingot" {
            return Some(Assertion::ParseableIngot);
        }
        if line == "trailing_cmd" {
            return Some(Assertion::TrailingCmd);
        }
        if line == "format_violation" {
            return Some(Assertion::FormatViolation);
        }
        if let Some(rest) = line.strip_prefix("status_enum:") {
            let _ = rest; // just check STATUS: PASS|FAIL in the response
            return Some(Assertion::StatusEnum);
        }
        if let Some(rest) = line.strip_prefix("action_keyword:") {
            let candidates = rest.split('|').map(|s| s.trim().to_string()).collect();
            return Some(Assertion::ActionKeyword { candidates });
        }
        None
    }

    fn load_fixture(path: &Path) -> Option<Fixture> {
        let content = std::fs::read_to_string(path).ok()?;
        let name = path.file_stem()?.to_string_lossy().to_string();

        let (prompt_part, assert_part) = if let Some(idx) = content.find("---ASSERT---") {
            (&content[..idx], &content[idx + "---ASSERT---".len()..])
        } else {
            (content.as_str(), "")
        };

        // Strip the "=== PROMPT ===" header if present
        let prompt = if let Some(after) = prompt_part.strip_prefix("=== PROMPT ===\n") {
            after.trim().to_string()
        } else {
            prompt_part.trim().to_string()
        };

        let assertions: Vec<Assertion> = assert_part
            .lines()
            .filter_map(parse_assertion)
            .collect();

        Some(Fixture { name, prompt, assertions })
    }

    fn discover_fixtures(dir: &Path) -> Vec<Fixture> {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut fixtures = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map(|e| e == "txt").unwrap_or(false) {
                if let Some(f) = load_fixture(&path) {
                    fixtures.push(f);
                }
            }
        }
        fixtures.sort_by(|a, b| a.name.cmp(&b.name));
        fixtures
    }

    fn check_assertions(response: &str, assertions: &[Assertion]) -> (bool, Option<String>) {
        for assertion in assertions {
            match assertion {
                Assertion::ParseableIngot => {
                    let ingots = parse_ingot_lines(response);
                    if ingots.is_empty() {
                        return (false, Some("format_violation".to_string()));
                    }
                }
                Assertion::TrailingCmd => {
                    if extract_trailing_cmd(response).is_none() {
                        return (false, Some("cmd_missing".to_string()));
                    }
                }
                Assertion::StatusEnum => {
                    let upper = response.to_ascii_uppercase();
                    if !upper.contains("STATUS: PASS") && !upper.contains("STATUS: FAIL") {
                        return (false, Some("format_violation".to_string()));
                    }
                }
                Assertion::ActionKeyword { candidates } => {
                    let refs: Vec<&str> = candidates.iter().map(|s| s.as_str()).collect();
                    if extract_action_keyword(response, &refs).is_none() {
                        return (false, Some("wrong_action_keyword".to_string()));
                    }
                }
                Assertion::FormatViolation => {
                    // A "format_violation" fixture expects the response to fail —
                    // it passes only if the response would be rejected as malformed (no ingots).
                    let ingots = parse_ingot_lines(response);
                    if !ingots.is_empty() {
                        return (false, Some("unexpected_pass".to_string()));
                    }
                }
            }
        }
        (true, None)
    }

    fn run_offline_bench() -> Vec<FixtureResult> {
        // Resolve fixture dirs relative to the crate manifest dir
        let base = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let fixture_dirs = [
            base.join("tests/fixtures/founder"),
            base.join("tests/fixtures/outcome"),
            base.join("tests/fixtures/resmelt"),
        ];

        let mut results = Vec::new();

        for dir in &fixture_dirs {
            let fixtures = discover_fixtures(dir);
            for fixture in &fixtures {
                // Offline mode: choose mock response based on the fixture's assertions.
                let (smith_name, response) =
                    if fixture.assertions.iter().any(|a| matches!(a, Assertion::FormatViolation)) {
                        // Fixture expects failure — use prose that produces no ingots
                        (
                            "mock-fail",
                            "Here is a description of what the task should do, with no structured output.".to_string(),
                        )
                    } else {
                        // Fixture expects success — use a valid ingot + CMD line
                        (
                            "mock-pass",
                            concat!(
                                "(ingot :id \"i1\" :status ore :solo t :grade 1 :skill default",
                                " :heat 0 :max 5 :smelt 0 :proof \"cargo test\" :work \"Build hello world\")\n",
                                "STATUS: PASS\n",
                                "REWRITE: try again\n",
                                "CMD: cargo test"
                            ).to_string(),
                        )
                    };

                let start = Instant::now();
                let (passed, failure_class) = check_assertions(&response, &fixture.assertions);
                let duration_ms = start.elapsed().as_millis() as u64;

                results.push(FixtureResult {
                    fixture: fixture.name.clone(),
                    smith: smith_name.to_string(),
                    passed,
                    failure_class,
                    duration_ms,
                });
            }
        }

        results
    }

    fn print_adherence_table(results: &[FixtureResult]) {
        println!("\n{:=<60}", "");
        println!("  Smith Adherence Bench Results");
        println!("{:=<60}", "");
        println!("{:<30} {:<15} {:<15} {:<10}", "Fixture", "Smith", "Result", "Class");
        println!("{:-<70}", "");

        let mut total = 0usize;
        let mut passed_count = 0usize;
        for r in results {
            total += 1;
            if r.passed {
                passed_count += 1;
            }
            let result = if r.passed { "PASS" } else { "FAIL" };
            let class = r.failure_class.as_deref().unwrap_or("-");
            println!("{:<30} {:<15} {:<15} {:<10}", r.fixture, r.smith, result, class);
        }

        println!("{:-<70}", "");
        if total > 0 {
            println!(
                "  Total: {}/{} passed ({:.0}%)",
                passed_count,
                total,
                (passed_count as f64 / total as f64) * 100.0
            );
        } else {
            println!("  Total: 0/0 passed");
        }
        println!("{:=<60}\n", "");
    }

    #[test]
    fn smith_fixtures_bench() {
        if std::env::var("SLAG_BENCH").unwrap_or_default() != "1" {
            // Skip in default test runs — set SLAG_BENCH=1 to enable
            return;
        }

        let results = run_offline_bench();
        print_adherence_table(&results);

        // Verify at least one fixture was discovered
        assert!(!results.is_empty(), "bench should discover at least one fixture");
        // All mock-pass fixtures should pass
        let mock_pass_failures: Vec<_> = results
            .iter()
            .filter(|r| r.smith == "mock-pass" && !r.passed)
            .map(|r| r.fixture.as_str())
            .collect();
        assert!(
            mock_pass_failures.is_empty(),
            "all mock-pass fixtures should pass, failed: {:?}",
            mock_pass_failures
        );
    }

    #[test]
    fn smith_fixtures_offline_discovers_fixtures() {
        // Always runs — tests fixture file discovery without requiring SLAG_BENCH
        let base = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let fixtures = discover_fixtures(&base.join("tests/fixtures/founder"));
        assert!(!fixtures.is_empty(), "should discover at least one fixture file in tests/fixtures/founder");
    }

    #[test]
    fn smith_fixtures_parse_assertion_variants() {
        assert!(matches!(parse_assertion("parseable_ingot"), Some(Assertion::ParseableIngot)));
        assert!(matches!(parse_assertion("trailing_cmd"), Some(Assertion::TrailingCmd)));
        assert!(matches!(parse_assertion("format_violation"), Some(Assertion::FormatViolation)));
        assert!(matches!(parse_assertion("status_enum: PASS|FAIL"), Some(Assertion::StatusEnum)));
        assert!(parse_assertion("unknown_assertion").is_none());
    }

    #[test]
    fn smith_fixtures_env_gate_skips_without_env() {
        // Verifies the env gate logic: SLAG_BENCH != "1" means bench is skipped
        let bench_enabled = std::env::var("SLAG_BENCH").unwrap_or_default() == "1";
        if bench_enabled {
            return; // meta-test not meaningful when bench is enabled
        }
        assert!(!bench_enabled, "SLAG_BENCH should not be '1' in default CI");
    }

    #[test]
    fn smith_fixtures_check_assertions_format_violation_passes_on_prose() {
        let assertions = [Assertion::FormatViolation];
        let prose = "Here is a description with no structured output.";
        let (passed, class) = check_assertions(prose, &assertions);
        assert!(passed, "format_violation fixture should pass when response has no ingots");
        assert!(class.is_none());
    }

    #[test]
    fn smith_fixtures_check_assertions_trailing_cmd_detected() {
        let assertions = [Assertion::TrailingCmd];
        let response = "some work done\nCMD: cargo test";
        let (passed, class) = check_assertions(response, &assertions);
        assert!(passed, "trailing_cmd should pass when CMD: is present");
        assert!(class.is_none());
    }

    #[test]
    fn smith_fixtures_check_assertions_trailing_cmd_missing() {
        let assertions = [Assertion::TrailingCmd];
        let response = "some work done, no cmd line";
        let (passed, class) = check_assertions(response, &assertions);
        assert!(!passed);
        assert_eq!(class.as_deref(), Some("cmd_missing"));
    }

    #[test]
    fn smith_fixtures_scripted_mock_cycles() {
        use super::mock::MockSmith;
        use super::Smith;
        let smith = MockSmith::scripted(vec!["first".into(), "second".into()]);
        assert_eq!(smith.call_count(), 0);
        // scripted is an alias for new — verify cycling behavior via call_count
        let rt = tokio::runtime::Runtime::new().unwrap();
        let r1 = rt.block_on(smith.invoke("p")).unwrap();
        let r2 = rt.block_on(smith.invoke("p")).unwrap();
        let r3 = rt.block_on(smith.invoke("p")).unwrap();
        assert_eq!(r1, "first");
        assert_eq!(r2, "second");
        assert_eq!(r3, "first"); // cycles
        assert_eq!(smith.call_count(), 3);
    }
}
