//! `docker logs` lines written by the web process.
//!
//! Every line is one JSON object written under a single lock; the event names and English
//! messages are the table. Fields never carry passwords, password hashes, cookies or session
//! keys. The setup_code_issued event deliberately carries the one-time setup code only in
//! its msg; that bootstrap value becomes unusable after setup, expiry or restart.

use std::io::Write;
use std::sync::{Mutex, OnceLock};

use serde_json::{json, Value};

use crate::codes::Event;
use crate::codes::{REASON_FILE_MISSING, REASON_IO_ERROR, REASON_PERMISSION_DENIED};
use crate::host::Host as _;

type Sink = Box<dyn Write + Send>;

/// The `{key, params}` reason object of a `docker logs` line. The external command raw or the
/// operating-system text of the same failure is carried inside the same object as `raw`,
/// never inside the key.
pub fn reason_object(key: &str, params: Value, raw: Option<&str>) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert("key".into(), json!(key));
    obj.insert("params".into(), params);
    if let Some(raw) = raw {
        if !raw.is_empty() {
            obj.insert("raw".into(), json!(raw));
        }
    }
    Value::Object(obj)
}

/// The general reason key of a host I/O error: the same classification the readings use.
pub fn io_reason_key(e: &std::io::Error) -> &'static str {
    match e.kind() {
        std::io::ErrorKind::NotFound => REASON_FILE_MISSING,
        std::io::ErrorKind::PermissionDenied => REASON_PERMISSION_DENIED,
        _ => REASON_IO_ERROR,
    }
}

fn sink() -> &'static Mutex<Option<Sink>> {
    static SINK: OnceLock<Mutex<Option<Sink>>> = OnceLock::new();
    SINK.get_or_init(|| Mutex::new(None))
}

/// Replace the output destination (used by the fake-host integration tests).
pub fn set_sink(new_sink: Box<dyn Write + Send>) {
    if let Ok(mut guard) = sink().lock() {
        *guard = Some(new_sink);
    }
}

/// Write one line to the destination, defaulting to stdout.
fn emit(level: &str, event: Event, fields: &[(&str, serde_json::Value)]) {
    let mut obj = serde_json::Map::new();
    obj.insert(
        "ts_ms".into(),
        json!(crate::host::RealHost::in_container().wall_ms()),
    );
    obj.insert("level".into(), json!(level));
    obj.insert("event".into(), json!(event.as_str()));
    obj.insert("msg".into(), json!(event.msg()));
    for (k, v) in fields {
        obj.insert((*k).to_string(), v.clone());
    }
    let line = serde_json::Value::Object(obj).to_string();
    // universal 2 / each program log line is an ASCII-only JSON object. Non-ASCII field
    // values keep their value but are escaped.
    let line = ascii_json(&line);
    let mut line = line;
    line.push('\n');
    let mut guard = match sink().lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    match guard.as_mut() {
        Some(s) => {
            let _ = s.write_all(line.as_bytes());
        }
        None => {
            let stdout = std::io::stdout();
            let mut lock = stdout.lock();
            let _ = lock.write_all(line.as_bytes());
        }
    }
}

/// Escape every non-ASCII character as \uXXXX so the line stays ASCII JSON.
fn ascii_json(text: &str) -> String {
    if text.is_ascii() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len() + 16);
    for c in text.chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            let mut buf = [0u16; 2];
            for unit in c.encode_utf16(&mut buf) {
                out.push_str(&format!("\\u{unit:04x}"));
            }
        }
    }
    out
}

/// One INFO line with text fields.
pub fn info(event: Event, fields: &[(&str, &str)]) {
    let owned: Vec<(&str, serde_json::Value)> = fields
        .iter()
        .map(|(k, v)| (*k, serde_json::Value::String((*v).to_string())))
        .collect();
    emit("INFO", event, &owned);
}

/// One ERROR line with text fields.
pub fn error(event: Event, fields: &[(&str, &str)]) {
    let owned: Vec<(&str, serde_json::Value)> = fields
        .iter()
        .map(|(k, v)| (*k, serde_json::Value::String((*v).to_string())))
        .collect();
    emit("ERROR", event, &owned);
}

/// One INFO line with JSON fields (used for `{key, params}` reasons).
pub fn info_json(event: Event, fields: &[(&str, serde_json::Value)]) {
    emit("INFO", event, fields);
}

/// One WARN line with JSON fields.
pub fn warn_json(event: Event, fields: &[(&str, serde_json::Value)]) {
    emit("WARN", event, fields);
}

/// One ERROR line with JSON fields.
pub fn error_json(event: Event, fields: &[(&str, serde_json::Value)]) {
    emit("ERROR", event, fields);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex as StdMutex};

    #[derive(Clone, Default)]
    struct Buf(Arc<StdMutex<Vec<u8>>>);

    impl Write for Buf {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(data);
            Ok(data.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn lines_are_json_with_event_and_msg() {
        let buf = Buf::default();
        set_sink(Box::new(buf.clone()));
        error(Event::SettingUnknown, &[("variable", "NVM_FOO")]);
        info(Event::Listening, &[("address", "0.0.0.0:8199")]);
        let text = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first["level"], "ERROR");
        assert_eq!(first["event"], "setting_unknown");
        assert_eq!(first["msg"], "unrecognized setting");
        assert_eq!(first["variable"], "NVM_FOO");
        let second: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(second["level"], "INFO");
        assert_eq!(second["event"], "listening");
        set_sink(Box::new(std::io::sink()));
    }
}
