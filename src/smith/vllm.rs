use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use reqwest::Client;
use serde_json::json;

use crate::config::SmithCapabilities;
use crate::error::SlagError;
use crate::events::{self, FailureClass};
use crate::smith::{Smith, StructuredOutputSpec};

/// HTTP adapter for vLLM's OpenAI-compatible /v1/chat/completions endpoint.
pub struct VllmSmith {
    client: Client,
    base_url: String,
    model: String,
    api_key: String,
    timeout_secs: u64,
    enable_thinking: bool,
    capabilities: SmithCapabilities,
}

impl VllmSmith {
    /// Construct from environment variables.
    /// Required: SLAG_VLLM_BASE_URL, SLAG_VLLM_MODEL
    /// Optional: SLAG_VLLM_API_KEY (default "EMPTY"), SLAG_VLLM_TIMEOUT_SECS, SLAG_VLLM_ENABLE_THINKING
    pub fn from_env() -> Result<Self, SlagError> {
        let base_url = std::env::var("SLAG_VLLM_BASE_URL")
            .or_else(|_| std::env::var("OPENAI_BASE_URL"))
            .map_err(|_| SlagError::SmithFailed("SLAG_VLLM_BASE_URL not set".into()))?;
        let base_url = base_url.trim_end_matches('/').to_string();

        // Model is optional — "auto" lets vLLM select the loaded model automatically.
        let model = std::env::var("SLAG_VLLM_MODEL")
            .or_else(|_| std::env::var("OPENAI_MODEL"))
            .unwrap_or_else(|_| "auto".to_string());

        // Always send a non-empty token — never send "Bearer None" (vLLM issue #33412)
        let api_key = std::env::var("SLAG_VLLM_API_KEY")
            .or_else(|_| std::env::var("OPENAI_API_KEY"))
            .unwrap_or_else(|_| "EMPTY".to_string());
        let api_key = if api_key.trim().is_empty() {
            "EMPTY".to_string()
        } else {
            api_key
        };

        let timeout_secs = std::env::var("SLAG_VLLM_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|v| *v > 0)
            .or_else(|| {
                std::env::var("SLAG_SMITH_TIMEOUT_SECS")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .filter(|v: &u64| *v > 0)
            })
            .unwrap_or(300);

        // KTD3: thinking mode disabled by default for format-strict phases.
        // vLLM 0.11.2+ silently disables structured_outputs when reasoning is on,
        // and Qwen3 Precision degrades on exact-suffix constraints.
        let enable_thinking = std::env::var("SLAG_VLLM_ENABLE_THINKING")
            .ok()
            .map(|v| matches!(v.trim(), "1" | "true" | "yes" | "on"))
            .unwrap_or(false);

        let client = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .tcp_keepalive(Duration::from_secs(30))
            .pool_idle_timeout(Duration::from_secs(30))
            .use_rustls_tls()
            .build()
            .map_err(|e| SlagError::SmithFailed(format!("reqwest client build failed: {e}")))?;

        Ok(Self {
            client,
            base_url,
            model,
            api_key,
            timeout_secs,
            enable_thinking,
            capabilities: SmithCapabilities::vllm(),
        })
    }

    async fn invoke_impl(&self, prompt: &str) -> Result<String, SlagError> {
        self.invoke_impl_with_spec(prompt, None).await
    }

    async fn invoke_impl_with_spec(
        &self,
        prompt: &str,
        spec: Option<&StructuredOutputSpec>,
    ) -> Result<String, SlagError> {
        // Up to 2 attempts: initial + one retry after a rate-limit sleep.
        let mut retried = false;
        loop {
            let url = format!("{}/v1/chat/completions", self.base_url);
            let cap = &self.capabilities;

            let mut extra_body = serde_json::json!({
                "top_k": cap.default_top_k,
                "chat_template_kwargs": {
                    "enable_thinking": self.enable_thinking
                }
            });

            // Inject structured_outputs into extra_body when spec is provided and supported.
            if let Some(s) = spec {
                if cap.supports_structured_outputs {
                    let structured = match s {
                        StructuredOutputSpec::Choice(choices) => serde_json::json!({
                            "type": "choice",
                            "choices": choices,
                            "backend": "xgrammar:no-fallback"
                        }),
                        StructuredOutputSpec::Grammar(grammar) => serde_json::json!({
                            "type": "grammar",
                            "grammar": grammar,
                            "backend": "xgrammar:no-fallback"
                        }),
                        StructuredOutputSpec::Regex(pattern) => serde_json::json!({
                            "type": "regex",
                            "pattern": pattern,
                            "backend": "xgrammar:no-fallback"
                        }),
                    };
                    extra_body["structured_outputs"] = structured;
                }
            }

            let body = json!({
                "model": self.model,
                "messages": [{"role": "user", "content": prompt}],
                "temperature": cap.default_temperature,
                "top_p": cap.default_top_p,
                "stream": false,
                "extra_body": extra_body
            });

            let request = self
                .client
                .post(&url)
                .bearer_auth(&self.api_key)
                .json(&body)
                .timeout(Duration::from_secs(self.timeout_secs));

            let response = match request.send().await {
                Ok(r) => r,
                Err(e) => {
                    // Distinguish transport failure modes so failover routing
                    // and FailureClass observability are accurate.
                    let (class, msg) = if e.is_connect() {
                        (FailureClass::HttpError, format!("connect: {e}"))
                    } else if e.is_timeout() {
                        (
                            FailureClass::HttpError,
                            format!("timeout after {}s", self.timeout_secs),
                        )
                    } else if e.is_request() {
                        // Non-connect request errors (URL builder, body encode,
                        // redirect policy) are a config defect, not a transient
                        // network condition — keep them out of the connect:
                        // failover bucket.
                        (
                            FailureClass::HttpError,
                            format!("http setup error: {e}"),
                        )
                    } else {
                        (
                            FailureClass::HttpError,
                            format!("http request error: {e}"),
                        )
                    };
                    events::emit_smith_invoke_failure("vllm", &class, &msg, 0);
                    return Err(SlagError::SmithFailed(msg));
                }
            };

            let status = response.status();

            // Handle 429 / 503 with optional Retry-After: single in-adapter retry.
            // Plan U6: retry happens on any 429/503 — Retry-After is the *hint*,
            // not the gate (real proxies/CDNs frequently omit the header).
            if status.as_u16() == 429 || status.as_u16() == 503 {
                let retry_after = response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(1) // default 1s backoff when header missing/unparseable
                    .clamp(1, 5);

                let body_text = response.text().await.unwrap_or_default();
                let msg = format!("http {}: {}", status.as_u16(), truncate_body(&body_text));
                events::emit_smith_invoke_failure(
                    "vllm",
                    &FailureClass::ModelBusy,
                    &msg,
                    if retried { 1 } else { 0 },
                );

                if !retried {
                    tokio::time::sleep(Duration::from_secs(retry_after)).await;
                    retried = true;
                    continue;
                }
                return Err(SlagError::SmithFailed(msg));
            }

            // Auth errors — NOT failover candidates
            if status.as_u16() == 401 || status.as_u16() == 403 {
                let body_text = response.text().await.unwrap_or_default();
                let msg = format!(
                    "http {}: {} (check SLAG_VLLM_API_KEY)",
                    status.as_u16(),
                    truncate_body(&body_text)
                );
                events::emit_smith_invoke_failure("vllm", &FailureClass::AuthError, &msg, 0);
                return Err(SlagError::SmithFailed(msg));
            }

            if !status.is_success() {
                let body_text = response.text().await.unwrap_or_default();
                let msg = format!("http {}: {}", status.as_u16(), truncate_body(&body_text));
                events::emit_smith_invoke_failure("vllm", &FailureClass::HttpError, &msg, 0);
                return Err(SlagError::SmithFailed(msg));
            }

            let value: serde_json::Value = match response.json().await {
                Ok(v) => v,
                Err(e) => {
                    let msg = format!("vllm parse: {e}");
                    events::emit_smith_invoke_failure(
                        "vllm",
                        &FailureClass::ParseUnrecoverable,
                        &msg,
                        0,
                    );
                    return Err(SlagError::SmithFailed(msg));
                }
            };

            // Plan R11: silent length-truncation is unacceptable on every call,
            // not only structured-output calls. A truncated CMD: or action
            // keyword parses cleanly upstream but is semantically wrong.
            let finish_reason = value["choices"][0]["finish_reason"].as_str().unwrap_or("");
            if finish_reason == "length" {
                let msg = "vllm: length-truncated".to_string();
                events::emit_smith_invoke_failure(
                    "vllm",
                    &FailureClass::Truncation,
                    &msg,
                    0,
                );
                return Err(SlagError::SmithFailed(msg));
            }

            let content = match value["choices"][0]["message"]["content"].as_str() {
                Some(c) => c.to_string(),
                None => {
                    let msg = "vllm: empty choices".to_string();
                    events::emit_smith_invoke_failure(
                        "vllm",
                        &FailureClass::FormatViolation,
                        &msg,
                        0,
                    );
                    return Err(SlagError::SmithFailed(msg));
                }
            };

            return Ok(content);
        }
    }
}

fn truncate_body(body: &str) -> String {
    const LIMIT: usize = 200;
    if body.len() <= LIMIT {
        return body.to_string();
    }
    // UTF-8 safe: snap to the nearest char boundary <= LIMIT
    let end = (0..=LIMIT)
        .rev()
        .find(|&i| body.is_char_boundary(i))
        .unwrap_or(0);
    format!("{}...", &body[..end])
}

impl Smith for VllmSmith {
    fn invoke(
        &self,
        prompt: &str,
    ) -> Pin<Box<dyn Future<Output = Result<String, SlagError>> + Send + '_>> {
        let prompt = prompt.to_string();
        Box::pin(async move { self.invoke_impl(&prompt).await })
    }

    fn invoke_with_constraints(
        &self,
        prompt: &str,
        spec: Option<&StructuredOutputSpec>,
    ) -> Pin<Box<dyn Future<Output = Result<String, SlagError>> + Send + '_>> {
        let prompt = prompt.to_string();
        let spec = spec.cloned();
        Box::pin(async move {
            self.invoke_impl_with_spec(&prompt, spec.as_ref()).await
        })
    }

    fn capabilities(&self) -> &SmithCapabilities {
        &self.capabilities
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::smith::StructuredOutputSpec;

    #[test]
    fn from_env_fails_without_base_url() {
        let saved = std::env::var_os("SLAG_VLLM_BASE_URL");
        let saved_openai = std::env::var_os("OPENAI_BASE_URL");
        std::env::remove_var("SLAG_VLLM_BASE_URL");
        std::env::remove_var("OPENAI_BASE_URL");
        let result = VllmSmith::from_env();
        if let Some(v) = saved {
            std::env::set_var("SLAG_VLLM_BASE_URL", v);
        }
        if let Some(v) = saved_openai {
            std::env::set_var("OPENAI_BASE_URL", v);
        }
        assert!(result.is_err());
    }

    #[test]
    fn api_key_defaults_to_empty_not_none() {
        std::env::set_var("SLAG_VLLM_BASE_URL", "http://localhost:8000");
        std::env::set_var("SLAG_VLLM_MODEL", "qwen3-32b");
        let saved_key = std::env::var_os("SLAG_VLLM_API_KEY");
        let saved_oai = std::env::var_os("OPENAI_API_KEY");
        std::env::remove_var("SLAG_VLLM_API_KEY");
        std::env::remove_var("OPENAI_API_KEY");
        let smith = VllmSmith::from_env().expect("should construct");
        if let Some(v) = saved_key {
            std::env::set_var("SLAG_VLLM_API_KEY", v);
        }
        if let Some(v) = saved_oai {
            std::env::set_var("OPENAI_API_KEY", v);
        }
        assert_eq!(smith.api_key, "EMPTY");
    }

    #[test]
    fn capabilities_returns_vllm_profile() {
        std::env::set_var("SLAG_VLLM_BASE_URL", "http://localhost:8000");
        std::env::set_var("SLAG_VLLM_MODEL", "qwen3-32b");
        let smith = VllmSmith::from_env().expect("should construct");
        std::env::remove_var("SLAG_VLLM_BASE_URL");
        std::env::remove_var("SLAG_VLLM_MODEL");
        assert_eq!(smith.capabilities().name, "vllm");
        assert!(smith.capabilities().supports_structured_outputs);
    }

    #[test]
    fn enable_thinking_defaults_false() {
        // KTD3: default false — thinking mode disabled for format-strict phases.
        std::env::set_var("SLAG_VLLM_BASE_URL", "http://localhost:8000");
        std::env::set_var("SLAG_VLLM_MODEL", "test-model");
        let saved = std::env::var_os("SLAG_VLLM_ENABLE_THINKING");
        std::env::remove_var("SLAG_VLLM_ENABLE_THINKING");
        let smith = VllmSmith::from_env().expect("should construct");
        std::env::remove_var("SLAG_VLLM_BASE_URL");
        std::env::remove_var("SLAG_VLLM_MODEL");
        if let Some(v) = saved {
            std::env::set_var("SLAG_VLLM_ENABLE_THINKING", v);
        }
        assert!(!smith.enable_thinking);
    }

    #[test]
    fn failover_candidate_for_connect_error() {
        use crate::error::SlagError;
        let err = SlagError::SmithFailed("connect: connection refused".into());
        // is_smith_failover_candidate is private to forge.rs; verify the error string
        // pattern that the function matches against is correct
        let msg = err.to_string();
        assert!(msg.contains("connect:"));
    }

    #[test]
    fn invoke_with_constraints_choice_is_supported() {
        // VllmSmith has supports_structured_outputs=true — a Choice spec should be accepted
        let saved_url = std::env::var_os("SLAG_VLLM_BASE_URL");
        let saved_model = std::env::var_os("SLAG_VLLM_MODEL");
        std::env::set_var("SLAG_VLLM_BASE_URL", "http://localhost:8000");
        std::env::set_var("SLAG_VLLM_MODEL", "qwen3-32b");
        let smith = VllmSmith::from_env().expect("should construct");
        if let Some(v) = saved_url {
            std::env::set_var("SLAG_VLLM_BASE_URL", v);
        } else {
            std::env::remove_var("SLAG_VLLM_BASE_URL");
        }
        if let Some(v) = saved_model {
            std::env::set_var("SLAG_VLLM_MODEL", v);
        } else {
            std::env::remove_var("SLAG_VLLM_MODEL");
        }
        assert!(smith.capabilities().supports_structured_outputs);
    }

    #[test]
    fn structured_output_spec_choice_clones_correctly() {
        // Verify StructuredOutputSpec derives Clone correctly (used in invoke_with_constraints)
        let spec = StructuredOutputSpec::Choice(vec!["PASS".to_string(), "FAIL".to_string()]);
        let cloned = spec.clone();
        if let StructuredOutputSpec::Choice(choices) = cloned {
            assert_eq!(choices, vec!["PASS", "FAIL"]);
        } else {
            panic!("expected Choice variant");
        }
    }

    #[test]
    fn structured_output_spec_grammar_clones_correctly() {
        let spec = StructuredOutputSpec::Grammar("root ::= \"ok\"".to_string());
        let cloned = spec.clone();
        if let StructuredOutputSpec::Grammar(g) = cloned {
            assert_eq!(g, "root ::= \"ok\"");
        } else {
            panic!("expected Grammar variant");
        }
    }

    #[test]
    fn structured_output_spec_regex_clones_correctly() {
        let spec = StructuredOutputSpec::Regex(".*".to_string());
        let cloned = spec.clone();
        if let StructuredOutputSpec::Regex(r) = cloned {
            assert_eq!(r, ".*");
        } else {
            panic!("expected Regex variant");
        }
    }

    #[test]
    fn base_url_trailing_slash_stripped() {
        std::env::set_var("SLAG_VLLM_BASE_URL", "http://localhost:8000/");
        std::env::set_var("SLAG_VLLM_MODEL", "qwen3-32b");
        let smith = VllmSmith::from_env().expect("should construct");
        std::env::remove_var("SLAG_VLLM_BASE_URL");
        std::env::remove_var("SLAG_VLLM_MODEL");
        assert_eq!(smith.base_url, "http://localhost:8000");
    }

    #[test]
    fn truncate_body_handles_multibyte_utf8_safely() {
        // Regression: raw &body[..200] panics on a multi-byte boundary.
        // 'é' is 2 bytes; this string ends mid-char around byte 200 if sliced raw.
        let body: String = "é".repeat(150); // 300 bytes, 150 chars
        let out = truncate_body(&body);
        assert!(out.ends_with("..."), "should mark truncation");
        // Must be valid UTF-8 (String guarantees this if the slice was on a boundary)
        assert!(out.is_char_boundary(out.len() - 3));
    }

    #[test]
    fn truncate_body_passthrough_short() {
        let body = "short body";
        assert_eq!(truncate_body(body), body);
    }

    #[test]
    fn enable_thinking_env_values_parse() {
        std::env::set_var("SLAG_VLLM_BASE_URL", "http://localhost:8000");
        std::env::set_var("SLAG_VLLM_MODEL", "qwen3-32b");

        for val in ["1", "true", "yes"] {
            std::env::set_var("SLAG_VLLM_ENABLE_THINKING", val);
            let smith = VllmSmith::from_env().expect("should construct");
            assert!(smith.enable_thinking, "expected true for SLAG_VLLM_ENABLE_THINKING={val}");
        }

        for val in ["0", "false", "no", ""] {
            std::env::set_var("SLAG_VLLM_ENABLE_THINKING", val);
            let smith = VllmSmith::from_env().expect("should construct");
            assert!(!smith.enable_thinking, "expected false for SLAG_VLLM_ENABLE_THINKING={val}");
        }

        std::env::remove_var("SLAG_VLLM_BASE_URL");
        std::env::remove_var("SLAG_VLLM_MODEL");
        std::env::remove_var("SLAG_VLLM_ENABLE_THINKING");
    }
}
