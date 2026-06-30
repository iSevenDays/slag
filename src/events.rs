use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use serde::Serialize;
use serde_json::{json, Value};

use crate::config::{LogFormat, LOG_DIR};

/// Failure-mode taxonomy for smith outputs.
/// In-process source of truth; emitted as a string field in event records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FailureClass {
    /// Response doesn't match required structure
    FormatViolation,
    /// Parser cannot recover the output after max attempts
    ParseUnrecoverable,
    /// CMD: suffix missing from response
    CmdMissing,
    /// Response contains unanswered questions
    QuestionsPresent,
    /// CMD: value is not a usable shell command
    ProofUnexecutable,
    /// Response was truncated mid-output
    Truncation,
    /// Action keyword (REWRITE/SPLIT/IMPOSSIBLE) absent or wrong
    WrongActionKeyword,
    /// HTTP transport error (non-auth, non-busy)
    HttpError,
    /// Authentication failure (401/403) — do not retry on another smith
    AuthError,
    /// Smith busy / rate-limited (429/503)
    ModelBusy,
}

impl FailureClass {
    /// Stable string identifier used as the event field value.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::FormatViolation => "format_violation",
            Self::ParseUnrecoverable => "parse_unrecoverable",
            Self::CmdMissing => "cmd_missing",
            Self::QuestionsPresent => "questions_present",
            Self::ProofUnexecutable => "proof_unexecutable",
            Self::Truncation => "truncation",
            Self::WrongActionKeyword => "wrong_action_keyword",
            Self::HttpError => "http_error",
            Self::AuthError => "auth_error",
            Self::ModelBusy => "model_busy",
        }
    }
}

/// Typed recast failure returned by the recast helper and smith adapters.
/// The `class` field drives escalation logic; `events.rs` emits it as the observability projection.
#[derive(Debug, Clone)]
pub struct RecastFailure {
    pub class: FailureClass,
    pub message: String,
    pub attempt: u32,
}

impl RecastFailure {
    pub fn new(class: FailureClass, message: impl Into<String>, attempt: u32) -> Self {
        Self { class, message: message.into(), attempt }
    }
}

static BUS: OnceLock<EventBus> = OnceLock::new();

struct EventBus {
    run_id: String,
    format: LogFormat,
    verbose: bool,
    file: Mutex<std::fs::File>,
}

pub fn init(format: LogFormat, verbose: bool) -> Result<(), std::io::Error> {
    if BUS.get().is_some() {
        return Ok(());
    }

    let run_id = format!(
        "{}_{}",
        chrono::Local::now().format("%Y%m%d_%H%M%S_%3f"),
        std::process::id()
    );
    let run_dir = Path::new(LOG_DIR).join("runs").join(&run_id);
    std::fs::create_dir_all(&run_dir)?;
    let file_path = run_dir.join("events.jsonl");
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(file_path)?;

    let bus = EventBus {
        run_id,
        format,
        verbose,
        file: Mutex::new(file),
    };
    let _ = BUS.set(bus);

    emit_info(
        "run.start",
        "SLAG run started",
        json!({
            "format": format!("{format:?}"),
            "verbose": verbose
        }),
    );
    Ok(())
}

pub fn run_id() -> Option<String> {
    BUS.get().map(|bus| bus.run_id.clone())
}

pub fn emit_info(kind: &str, message: &str, fields: Value) {
    emit(EventLevel::Info, kind, message, fields);
}

pub fn emit_debug(kind: &str, message: &str, fields: Value) {
    emit(EventLevel::Debug, kind, message, fields);
}

pub fn emit_warn(kind: &str, message: &str, fields: Value) {
    emit(EventLevel::Warn, kind, message, fields);
}

pub fn emit_error(kind: &str, message: &str, fields: Value) {
    emit(EventLevel::Error, kind, message, fields);
}

fn emit(level: EventLevel, kind: &str, message: &str, fields: Value) {
    let Some(bus) = BUS.get() else {
        return;
    };

    let record = EventRecord {
        ts: chrono::Local::now().to_rfc3339(),
        run_id: bus.run_id.clone(),
        level: level.as_str(),
        kind: kind.to_string(),
        message: message.to_string(),
        fields,
    };

    if let Ok(json_line) = serde_json::to_string(&record) {
        if let Ok(mut file) = bus.file.lock() {
            let _ = writeln!(file, "{json_line}");
        }

        match bus.format {
            LogFormat::Json => println!("{json_line}"),
            LogFormat::Text => {
                if matches!(level, EventLevel::Warn | EventLevel::Error)
                    || (matches!(level, EventLevel::Debug) && bus.verbose)
                {
                    println!("  \x1b[90m[{}:{}]\x1b[0m {}", level.as_str(), kind, message);
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EventLevel {
    Info,
    Debug,
    Warn,
    Error,
}

impl EventLevel {
    fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Debug => "debug",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

#[derive(Debug, Serialize)]
struct EventRecord {
    ts: String,
    run_id: String,
    level: &'static str,
    kind: String,
    message: String,
    fields: Value,
}

// Smith event helpers

pub fn emit_smith_invoke_failure(smith_hint: &str, class: &FailureClass, message: &str, attempt: u32) {
    emit_warn(
        "smith.invoke.failure",
        "smith invocation failed",
        json!({
            "smith": smith_hint,
            "failure_class": class.as_str(),
            "message": message,
            "attempt": attempt,
        }),
    );
}

pub fn emit_smith_recast_attempt(smith_hint: &str, class: &FailureClass, attempt: u32, max_attempts: u32) {
    emit_debug(
        "smith.recast.attempt",
        "smith recast attempt",
        json!({
            "smith": smith_hint,
            "failure_class": class.as_str(),
            "attempt": attempt,
            "max_attempts": max_attempts,
        }),
    );
}

pub fn emit_smith_recast_success(smith_hint: &str, attempt: u32) {
    emit_info(
        "smith.recast.success",
        "smith recast succeeded",
        json!({
            "smith": smith_hint,
            "attempt": attempt,
        }),
    );
}

pub fn emit_smith_recast_exhausted(smith_hint: &str, class: &FailureClass, attempts: u32) {
    emit_warn(
        "smith.recast.exhausted",
        "smith recast attempts exhausted",
        json!({
            "smith": smith_hint,
            "failure_class": class.as_str(),
            "attempts": attempts,
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_class_as_str_matches_taxonomy() {
        assert_eq!(FailureClass::FormatViolation.as_str(), "format_violation");
        assert_eq!(FailureClass::ParseUnrecoverable.as_str(), "parse_unrecoverable");
        assert_eq!(FailureClass::CmdMissing.as_str(), "cmd_missing");
        assert_eq!(FailureClass::QuestionsPresent.as_str(), "questions_present");
        assert_eq!(FailureClass::ProofUnexecutable.as_str(), "proof_unexecutable");
        assert_eq!(FailureClass::Truncation.as_str(), "truncation");
        assert_eq!(FailureClass::WrongActionKeyword.as_str(), "wrong_action_keyword");
        assert_eq!(FailureClass::HttpError.as_str(), "http_error");
        assert_eq!(FailureClass::AuthError.as_str(), "auth_error");
        assert_eq!(FailureClass::ModelBusy.as_str(), "model_busy");
    }

    #[test]
    fn recast_failure_carries_class_message_attempt() {
        let f = RecastFailure::new(FailureClass::CmdMissing, "no CMD: line found", 2);
        assert_eq!(f.class, FailureClass::CmdMissing);
        assert_eq!(f.message, "no CMD: line found");
        assert_eq!(f.attempt, 2);
    }

    #[test]
    fn recast_failure_class_as_str_roundtrips() {
        let f = RecastFailure::new(FailureClass::HttpError, "connect failed", 0);
        assert_eq!(f.class.as_str(), "http_error");
    }
}
