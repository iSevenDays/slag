use crate::config::{SmithCapabilities, SmithConfig};
use crate::error::SlagError;

/// Probe result for one smith in the chain.
pub struct SmithProbeResult {
    pub name: String,
    pub resolved: String,
    pub reachable: bool,
    pub version_or_model: String,
    pub capability_summary: String,
    pub error: Option<String>,
}

pub async fn run(config: &SmithConfig) -> Result<(), SlagError> {
    println!("\n  \x1b[1;37mSlag Smith Doctor\x1b[0m\n");

    let chain = config.base_chain_for_doctor();
    if chain.is_empty() {
        println!(
            "  \x1b[33m⚠\x1b[0m No smiths configured. Set SLAG_SMITH or ensure claude/codex/gemini is in PATH."
        );
        return Ok(());
    }

    let mut results: Vec<SmithProbeResult> = Vec::new();

    for cmd in chain {
        let result = probe_smith(cmd).await;
        results.push(result);
    }

    // Also probe vLLM if SLAG_VLLM_BASE_URL is set
    if let Ok(base_url) = std::env::var("SLAG_VLLM_BASE_URL")
        .or_else(|_| std::env::var("OPENAI_BASE_URL"))
    {
        results.push(probe_vllm(&base_url).await);
    }

    // Print table
    println!(
        "  {:20} {:40} {:10} {:30}",
        "Smith", "Command/URL", "Reachable", "Capabilities"
    );
    println!("  {}", "\u{2500}".repeat(100));
    for r in &results {
        let reach = if r.reachable {
            "\x1b[32m\u{2713} yes\x1b[0m"
        } else {
            "\x1b[31m\u{2717} no\x1b[0m"
        };
        let cmd_col = if r.resolved.len() > 38 {
            format!("{}...", &r.resolved[..35])
        } else {
            r.resolved.clone()
        };
        println!(
            "  {:20} {:40} {:20} {}",
            r.name, cmd_col, reach, r.capability_summary
        );
        if let Some(err) = &r.error {
            println!("       \x1b[31m\u{21b3} {}\x1b[0m", err);
        }
    }
    println!();

    let unreachable = results.iter().filter(|r| !r.reachable).count();
    if unreachable > 0 {
        println!("  \x1b[31m\u{2717}\x1b[0m {unreachable} smith(s) unreachable");
        return Err(SlagError::SmithFailed(format!(
            "{unreachable} smith(s) unreachable"
        )));
    }

    println!("  \x1b[32m\u{2713}\x1b[0m All smiths reachable");
    Ok(())
}

async fn probe_smith(cmd: &str) -> SmithProbeResult {
    let name = detect_smith_name(cmd);
    let caps = capability_profile_for_cmd(cmd);
    let cap_summary = format_cap_summary(&caps);

    // For vllm sentinel, hand off to probe_vllm with a placeholder
    if cmd == "vllm" {
        if let Ok(base_url) = std::env::var("SLAG_VLLM_BASE_URL")
            .or_else(|_| std::env::var("OPENAI_BASE_URL"))
        {
            return probe_vllm(&base_url).await;
        }
        return SmithProbeResult {
            name,
            resolved: cmd.to_string(),
            reachable: false,
            version_or_model: String::new(),
            capability_summary: cap_summary,
            error: Some("SLAG_VLLM_BASE_URL not set".to_string()),
        };
    }

    // Extract the executable name from the command string
    let program = cmd.split_whitespace().next().unwrap_or(cmd);

    // For shell wrapper commands (sh -lc ...), probe the outer shell
    let probe_program = if program == "sh" { "sh" } else { program };

    match std::process::Command::new(probe_program)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
    {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let text = format!("{stdout}{stderr}");
            let first_line = text.lines().next().unwrap_or("ok").trim().to_string();
            let version = if first_line.len() > 40 {
                format!("{}...", &first_line[..40])
            } else {
                first_line
            };
            SmithProbeResult {
                name,
                resolved: cmd.to_string(),
                reachable: true,
                version_or_model: version,
                capability_summary: cap_summary,
                error: None,
            }
        }
        Err(e) => SmithProbeResult {
            name,
            resolved: cmd.to_string(),
            reachable: false,
            version_or_model: String::new(),
            capability_summary: cap_summary,
            error: Some(format!("failed to spawn: {e}")),
        },
    }
}

async fn probe_vllm(base_url: &str) -> SmithProbeResult {
    let base_url = base_url.trim_end_matches('/');
    let models_url = format!("{base_url}/v1/models");

    let api_key = std::env::var("SLAG_VLLM_API_KEY")
        .or_else(|_| std::env::var("OPENAI_API_KEY"))
        .unwrap_or_else(|_| "EMPTY".to_string());
    let api_key = if api_key.trim().is_empty() {
        "EMPTY".to_string()
    } else {
        api_key
    };

    let caps = SmithCapabilities::vllm();
    let cap_summary = format!(
        "vllm, ctx={}K, structured=true",
        caps.context_window / 1000,
    );

    let client = match reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return SmithProbeResult {
                name: "vllm".to_string(),
                resolved: models_url,
                reachable: false,
                version_or_model: String::new(),
                capability_summary: cap_summary,
                error: Some(format!("client build failed: {e}")),
            };
        }
    };

    match client
        .get(&models_url)
        .bearer_auth(&api_key)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            let model = if let Ok(json) = resp.json::<serde_json::Value>().await {
                json["data"][0]["id"]
                    .as_str()
                    .unwrap_or("unknown model")
                    .to_string()
            } else {
                "unknown model".to_string()
            };
            SmithProbeResult {
                name: "vllm".to_string(),
                resolved: models_url,
                reachable: true,
                version_or_model: model,
                capability_summary: cap_summary,
                error: None,
            }
        }
        Ok(resp) => SmithProbeResult {
            name: "vllm".to_string(),
            resolved: models_url,
            reachable: false,
            version_or_model: String::new(),
            capability_summary: cap_summary,
            error: Some(format!("HTTP {} (check SLAG_VLLM_BASE_URL)", resp.status())),
        },
        Err(e) => SmithProbeResult {
            name: "vllm".to_string(),
            resolved: models_url,
            reachable: false,
            version_or_model: String::new(),
            capability_summary: cap_summary,
            error: Some(format!("{e} (check SLAG_VLLM_BASE_URL)")),
        },
    }
}

fn detect_smith_name(cmd: &str) -> String {
    let lower = cmd.to_ascii_lowercase();
    if lower.contains("claude") {
        "claude".to_string()
    } else if lower.contains("codex") {
        "codex".to_string()
    } else if lower.contains("gemini") {
        "gemini".to_string()
    } else if lower.contains("kimi") {
        "kimi".to_string()
    } else if lower.contains("opencode") {
        "opencode".to_string()
    } else if cmd == "vllm" {
        "vllm".to_string()
    } else {
        cmd.split_whitespace()
            .next()
            .unwrap_or(cmd)
            .to_string()
    }
}

fn capability_profile_for_cmd(cmd: &str) -> SmithCapabilities {
    let lower = cmd.to_ascii_lowercase();
    if lower == "vllm" || lower.starts_with("vllm ") {
        SmithCapabilities::vllm()
    } else {
        SmithCapabilities::claude()
    }
}

fn format_cap_summary(caps: &SmithCapabilities) -> String {
    format!(
        "{}, ctx={}K, structured={}",
        caps.name,
        caps.context_window / 1000,
        caps.supports_structured_outputs,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_smith_name_finds_claude() {
        assert_eq!(
            detect_smith_name("claude -p --permission-mode bypassPermissions"),
            "claude"
        );
    }

    #[test]
    fn detect_smith_name_finds_codex() {
        assert_eq!(detect_smith_name("codex -a never exec -"), "codex");
    }

    #[test]
    fn detect_smith_name_falls_back_to_first_word() {
        assert_eq!(detect_smith_name("sh -lc 'something'"), "sh");
    }

    #[test]
    fn detect_smith_name_finds_gemini() {
        assert_eq!(detect_smith_name("gemini -p something"), "gemini");
    }

    #[test]
    fn detect_smith_name_finds_kimi() {
        assert_eq!(
            detect_smith_name("kimi -p --permission-mode bypassPermissions"),
            "kimi"
        );
    }

    #[test]
    fn detect_smith_name_vllm_sentinel() {
        assert_eq!(detect_smith_name("vllm"), "vllm");
    }

    #[test]
    fn format_cap_summary_contains_name_and_context() {
        let caps = SmithCapabilities::claude();
        let summary = format_cap_summary(&caps);
        assert!(summary.contains("claude"));
        assert!(summary.contains("200K"));
    }

    #[test]
    fn format_cap_summary_vllm_shows_structured_true() {
        let caps = SmithCapabilities::vllm();
        let summary = format_cap_summary(&caps);
        assert!(summary.contains("structured=true"));
    }
}
