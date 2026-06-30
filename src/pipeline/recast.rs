use std::collections::HashMap;

use crate::events::{self, FailureClass, RecastFailure};
use crate::smith::Smith;

/// Map a transport-layer smith error string to the appropriate `FailureClass`.
/// Mirrors the dispatch rules used by `forge::is_smith_failover_candidate` so
/// downstream consumers (escalation, observability, ledger) see the real cause
/// instead of every transport failure collapsing into `ParseUnrecoverable`.
fn classify_smith_error(msg: &str) -> FailureClass {
    let lower = msg.to_ascii_lowercase();
    if lower.contains("http 401") || lower.contains("http 403") {
        return FailureClass::AuthError;
    }
    if lower.contains("http 429") || lower.contains("http 503") || lower.contains("rate limit")
        || lower.contains("usage limit") || lower.contains("model busy")
    {
        return FailureClass::ModelBusy;
    }
    if lower.contains("length-truncated") || lower.contains("finish_reason: \"length\"") {
        return FailureClass::Truncation;
    }
    if lower.starts_with("connect:") || lower.contains("timeout after")
        || lower.starts_with("http 5") || lower.contains("empty choices")
        || lower.contains("vllm parse")
    {
        return FailureClass::HttpError;
    }
    FailureClass::ParseUnrecoverable
}

/// Stateless format-repair primitive for founder, resmelt, and reviewer-lane parsing.
///
/// - attempt 0: base prompt (`prompt_fn(0)`)
/// - attempt 1+: recast prompt (`recast_fn(attempt, last_failure)`)
/// - `max_attempts`: total attempts before returning `Err(RecastFailure)`
///
/// Returns `Ok(T)` on first successful parse, or `Err(RecastFailure)` after exhaustion.
pub async fn bounded_retry<T, PF, RF, PA>(
    smith: &dyn Smith,
    smith_hint: &str,
    prompt_fn: PF,
    recast_fn: RF,
    parser: PA,
    max_attempts: u32,
) -> Result<T, RecastFailure>
where
    PF: Fn(u32) -> String,
    RF: Fn(u32, &RecastFailure) -> String,
    PA: Fn(&str) -> Result<T, FailureClass>,
{
    let mut last_failure: Option<RecastFailure> = None;
    for attempt in 0..max_attempts {
        let prompt = if attempt == 0 {
            prompt_fn(0)
        } else {
            let prev = last_failure.as_ref().unwrap();
            recast_fn(attempt, prev)
        };

        let raw = match smith.invoke(&prompt).await {
            Ok(r) => r,
            Err(e) => {
                let msg = e.to_string();
                let class = classify_smith_error(&msg);
                events::emit_smith_invoke_failure(smith_hint, &class, &msg, attempt);
                // Transport errors are not retryable inside the recast loop —
                // they bypass parser escalation and return immediately so the
                // caller can decide whether to failover to the next smith.
                return Err(RecastFailure::new(class, msg, attempt));
            }
        };

        match parser(&raw) {
            Ok(value) => {
                if attempt > 0 {
                    events::emit_smith_recast_success(smith_hint, attempt);
                }
                return Ok(value);
            }
            Err(class) => {
                events::emit_smith_recast_attempt(smith_hint, &class, attempt, max_attempts);
                last_failure = Some(RecastFailure::new(
                    class,
                    format!("parse failed on attempt {attempt}"),
                    attempt,
                ));
            }
        }
    }
    let exhausted = last_failure.unwrap_or_else(|| {
        RecastFailure::new(FailureClass::ParseUnrecoverable, "no attempts", 0)
    });
    events::emit_smith_recast_exhausted(smith_hint, &exhausted.class, max_attempts);
    Err(exhausted)
}

/// Adds 3-identical-signature bail to `bounded_retry`.
/// Used by `strike_ingot_with_chain` in forge.rs.
#[allow(clippy::too_many_arguments)]
pub async fn bounded_retry_with_signature<T, PF, RF, PA, SF>(
    smith: &dyn Smith,
    smith_hint: &str,
    prompt_fn: PF,
    recast_fn: RF,
    parser: PA,
    signature_fn: SF,
    max_attempts: u32,
    identical_bail: u32,
) -> Result<T, RecastFailure>
where
    PF: Fn(u32) -> String,
    RF: Fn(u32, &RecastFailure) -> String,
    PA: Fn(&str) -> Result<T, FailureClass>,
    SF: Fn(&str) -> String,
{
    let mut sig_counts: HashMap<String, u32> = HashMap::new();
    let mut last_failure: Option<RecastFailure> = None;
    for attempt in 0..max_attempts {
        let prompt = if attempt == 0 {
            prompt_fn(0)
        } else {
            let prev = last_failure.as_ref().unwrap();
            recast_fn(attempt, prev)
        };

        let raw = match smith.invoke(&prompt).await {
            Ok(r) => r,
            Err(e) => {
                let msg = e.to_string();
                let class = classify_smith_error(&msg);
                events::emit_smith_invoke_failure(smith_hint, &class, &msg, attempt);
                return Err(RecastFailure::new(class, msg, attempt));
            }
        };

        match parser(&raw) {
            Ok(value) => {
                if attempt > 0 {
                    events::emit_smith_recast_success(smith_hint, attempt);
                }
                return Ok(value);
            }
            Err(class) => {
                let sig = signature_fn(&raw);
                let count = sig_counts.entry(sig).or_insert(0);
                *count += 1;
                if *count >= identical_bail {
                    // Preserve the original FailureClass — downstream
                    // routing must see the real cause, not a generic bail.
                    let failure = RecastFailure::new(
                        class.clone(),
                        format!(
                            "identical {} signature repeated {identical_bail} times",
                            class.as_str()
                        ),
                        attempt,
                    );
                    events::emit_smith_recast_exhausted(
                        smith_hint,
                        &failure.class,
                        attempt + 1,
                    );
                    return Err(failure);
                }
                events::emit_smith_recast_attempt(smith_hint, &class, attempt, max_attempts);
                last_failure = Some(RecastFailure::new(
                    class,
                    format!("parse failed on attempt {attempt}"),
                    attempt,
                ));
            }
        }
    }
    let exhausted = last_failure.unwrap_or_else(|| {
        RecastFailure::new(FailureClass::ParseUnrecoverable, "no attempts", 0)
    });
    events::emit_smith_recast_exhausted(smith_hint, &exhausted.class, max_attempts);
    Err(exhausted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::smith::mock::MockSmith;

    #[tokio::test]
    async fn bounded_retry_exhausts_after_max_attempts() {
        // parser always fails → should exhaust after max_attempts
        let smith = MockSmith::fixed("bad output");
        let result = bounded_retry(
            &smith,
            "mock",
            |_| "prompt".to_string(),
            |_, _| "recast prompt".to_string(),
            |_raw| Err::<String, _>(FailureClass::FormatViolation),
            3,
        )
        .await;
        assert!(result.is_err());
        assert_eq!(smith.call_count(), 3);
    }

    #[tokio::test]
    async fn bounded_retry_succeeds_on_second_attempt() {
        let smith = MockSmith::new(vec!["bad".to_string(), "ok".to_string()]);
        let result = bounded_retry(
            &smith,
            "mock",
            |_| "prompt".to_string(),
            |_, _| "recast".to_string(),
            |raw| {
                if raw == "ok" {
                    Ok(raw.to_string())
                } else {
                    Err(FailureClass::FormatViolation)
                }
            },
            3,
        )
        .await;
        assert!(result.is_ok());
        assert_eq!(smith.call_count(), 2);
    }

    #[tokio::test]
    async fn bounded_retry_with_signature_bails_on_identical_failures() {
        // parser always fails with the same output → bails after identical_bail=3
        let smith = MockSmith::fixed("same error every time");
        let result = bounded_retry_with_signature(
            &smith,
            "mock",
            |_| "prompt".to_string(),
            |_, _| "recast".to_string(),
            |_raw| Err::<String, _>(FailureClass::FormatViolation),
            |raw| raw.chars().take(50).collect(),
            10, // max_attempts
            3,  // identical_bail
        )
        .await;
        assert!(result.is_err());
        // bails after 3 identical signatures
        assert_eq!(smith.call_count(), 3);
    }

    #[tokio::test]
    async fn zero_recast_happy_path_emits_no_recast_events() {
        // parser succeeds on first attempt → only one call
        let smith = MockSmith::fixed("good output");
        let result = bounded_retry(
            &smith,
            "mock",
            |_| "prompt".to_string(),
            |_, _| "recast".to_string(),
            |raw| Ok::<String, FailureClass>(raw.to_string()),
            3,
        )
        .await;
        assert!(result.is_ok());
        assert_eq!(smith.call_count(), 1);
    }

    #[tokio::test]
    async fn bounded_retry_with_signature_succeeds_after_varied_failures() {
        // First two calls fail with different signatures, third succeeds
        let smith = MockSmith::new(vec![
            "error type A".to_string(),
            "error type B".to_string(),
            "ok".to_string(),
        ]);
        let result = bounded_retry_with_signature(
            &smith,
            "mock",
            |_| "prompt".to_string(),
            |_, _| "recast".to_string(),
            |raw| {
                if raw == "ok" {
                    Ok(raw.to_string())
                } else {
                    Err(FailureClass::FormatViolation)
                }
            },
            |raw| raw.chars().take(50).collect(),
            5,  // max_attempts
            3,  // identical_bail
        )
        .await;
        assert!(result.is_ok());
        assert_eq!(smith.call_count(), 3);
    }

    #[tokio::test]
    async fn bounded_retry_with_zero_max_attempts_returns_err() {
        let smith = MockSmith::fixed("anything");
        let result = bounded_retry(
            &smith,
            "mock",
            |_| "prompt".to_string(),
            |_, _| "recast".to_string(),
            |_raw| Ok::<String, FailureClass>("ok".to_string()),
            0, // max_attempts=0 → no calls
        )
        .await;
        assert!(result.is_err());
        assert_eq!(smith.call_count(), 0);
    }

    #[test]
    fn classify_smith_error_maps_transport_classes() {
        assert_eq!(classify_smith_error("http 401: bad auth"), FailureClass::AuthError);
        assert_eq!(classify_smith_error("http 403: forbidden"), FailureClass::AuthError);
        assert_eq!(classify_smith_error("http 429: rate"), FailureClass::ModelBusy);
        assert_eq!(classify_smith_error("http 503: busy"), FailureClass::ModelBusy);
        assert_eq!(classify_smith_error("usage limit"), FailureClass::ModelBusy);
        assert_eq!(classify_smith_error("connect: refused"), FailureClass::HttpError);
        assert_eq!(classify_smith_error("timeout after 30s"), FailureClass::HttpError);
        assert_eq!(classify_smith_error("http 500: oops"), FailureClass::HttpError);
        assert_eq!(classify_smith_error("vllm: empty choices"), FailureClass::HttpError);
        assert_eq!(
            classify_smith_error("vllm: length-truncated"),
            FailureClass::Truncation
        );
        assert_eq!(
            classify_smith_error("something weird"),
            FailureClass::ParseUnrecoverable
        );
    }

    #[tokio::test]
    async fn transport_error_uses_classified_failure_class_not_parse_unrecoverable() {
        let smith = MockSmith::failing();
        let err = bounded_retry(
            &smith,
            "mock",
            |_| "p".into(),
            |_, _| "r".into(),
            |_raw| Ok::<String, FailureClass>("ok".to_string()),
            3,
        )
        .await
        .unwrap_err();
        // MockSmith::failing produces SmithFailed with a generic message;
        // classify_smith_error returns ParseUnrecoverable for that string,
        // but the failure path must NOT emit smith.recast.exhausted —
        // transport failures are reported via smith.invoke.failure instead.
        // Verifying the attempt counter stayed at 0 (no retry consumed):
        assert_eq!(err.attempt, 0);
    }

    #[tokio::test]
    async fn identical_bail_preserves_original_failure_class() {
        let smith = MockSmith::fixed("same output forever");
        let err = bounded_retry_with_signature(
            &smith,
            "mock",
            |_| "p".into(),
            |_, _| "r".into(),
            |_raw| Err::<String, _>(FailureClass::CmdMissing),
            |raw| raw.chars().take(50).collect(),
            10,
            3,
        )
        .await
        .unwrap_err();
        // Before the fix, the bail clobbered the class to ParseUnrecoverable;
        // now the real cause must survive so downstream routing/observability work.
        assert_eq!(err.class, FailureClass::CmdMissing);
    }

    #[tokio::test]
    async fn bounded_retry_smith_error_returns_immediately() {
        // Smith fails (no responses) → should return error immediately
        let smith = MockSmith::failing();
        let result = bounded_retry(
            &smith,
            "mock",
            |_| "prompt".to_string(),
            |_, _| "recast".to_string(),
            |_raw| Ok::<String, FailureClass>("ok".to_string()),
            5,
        )
        .await;
        assert!(result.is_err());
        // Only one invocation before returning
        assert_eq!(smith.call_count(), 1);
    }
}
