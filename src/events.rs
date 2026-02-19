use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use serde::Serialize;
use serde_json::{json, Value};

use crate::config::{LogFormat, LOG_DIR};

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
