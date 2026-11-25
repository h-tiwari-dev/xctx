//! `logs.query`: lines of a logs pane's source (its bounded buffer, or a file) that match a pattern, a level and
//! a time window, and the `logs.error` event when errors in the window cross a threshold.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use regex::{Regex, RegexBuilder};
use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::args::Args;
use crate::error::{ConnectorError, Result};
use crate::{BuiltinContext, BuiltinEvent, BuiltinOutput};

/// How many lines [`LogBuffer::default`] keeps.
pub const DEFAULT_LOG_CAPACITY: usize = 10_000;

/// The event raised when errors cross the threshold.
pub const LOGS_ERROR: &str = "logs.error";

/// At most this many bytes from the end of a log file are read.
const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;

/// Lines returned when the call gives no limit, and the most it may ask for.
const DEFAULT_LIMIT: i64 = 200;
const MAX_LIMIT: i64 = 5_000;

/// The window the error threshold looks at when the call gives no `since_secs`.
const DEFAULT_THRESHOLD_WINDOW_SECS: i64 = 60;

/// A log line's severity, from least to most severe.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    /// `debug` and `trace`.
    Debug,
    /// `info` and `notice`.
    Info,
    /// `warn` and `warning`.
    Warn,
    /// `error`, `fatal`, `critical`, `panic`.
    Error,
}

impl Level {
    /// The level a word names (`error`, `ERR`, `warning`, `fatal`, `trace`, ...), without regard to case.
    pub fn from_word(word: &str) -> Option<Level> {
        match word.trim().to_ascii_lowercase().as_str() {
            "fatal" | "crit" | "critical" | "panic" | "emerg" | "emergency" | "alert" | "error"
            | "err" | "severe" | "e" | "f" => Some(Level::Error),
            "warn" | "warning" | "w" => Some(Level::Warn),
            "info" | "notice" | "information" | "informational" | "i" => Some(Level::Info),
            "debug" | "trace" | "verbose" | "fine" | "d" | "t" => Some(Level::Debug),
            _ => None,
        }
    }

    /// The level's name: `error`, `warn`, `info` or `debug`.
    pub fn name(self) -> &'static str {
        match self {
            Level::Debug => "debug",
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Error => "error",
        }
    }

    /// The level of a numeric (pino/bunyan) level: 10 trace, 20 debug, 30 info, 40 warn, 50 error, 60 fatal.
    fn from_number(n: f64) -> Option<Level> {
        match n {
            n if n >= 50.0 => Some(Level::Error),
            n if n >= 40.0 => Some(Level::Warn),
            n if n >= 30.0 => Some(Level::Info),
            n if n >= 10.0 => Some(Level::Debug),
            _ => None,
        }
    }
}

fn regex(pattern: &str) -> Regex {
    // The patterns are constants in this file, checked by the tests.
    Regex::new(pattern).unwrap_or_else(|e| panic!("bad built-in regex {pattern}: {e}"))
}

static KEY_VALUE: LazyLock<Regex> =
    LazyLock::new(|| regex(r#"(?i)\b(?:level|lvl|severity|loglevel)\s*[=:]\s*"?([a-z]+)"#));
static BRACKETED: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"(?i)[\[<(]\s*(fatal|crit|critical|panic|error|err|warn|warning|info|notice|debug|trace)\s*[\]>)]",
    )
});
static UPPER_WORD: LazyLock<Regex> = LazyLock::new(|| {
    regex(r"\b(FATAL|CRITICAL|CRIT|PANIC|ERROR|ERR|WARNING|WARN|INFO|NOTICE|DEBUG|TRACE)\b")
});
static LINE_START: LazyLock<Regex> = LazyLock::new(|| {
    regex(r"(?i)^\s*(fatal|panic|error|warning|warn|info|debug|trace)(\[[^\]]*\])?:")
});
/// Ruby's Logger: `E, [2024-01-01T10:00:00.000 #1] ERROR -- : ...`.
static RUBY_LOGGER: LazyLock<Regex> = LazyLock::new(|| regex(r"^([FEWID]), \["));
static TIMESTAMP: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"(\d{4})-(\d{2})-(\d{2})[T ](\d{2}):(\d{2}):(\d{2})(?:[.,](\d{1,9}))?\s?(Z|z|[+-]\d{2}:?\d{2})?",
    )
});

/// The level a log line states, in the common formats: JSON (`"level":"error"`, or pino's numbers),
/// `level=error`, `[error]` or `<warn>`, an upper-case word (`ERROR`, `WARN`, `INFO`, `DEBUG`, `FATAL`, ...),
/// `error:` at the start of the line, Ruby's `E, [...]`, and a Rust panic. None when it states none.
pub fn detect_level(line: &str) -> Option<Level> {
    let trimmed = line.trim_start();
    if trimmed.starts_with('{')
        && let Ok(Value::Object(map)) = serde_json::from_str::<Value>(trimmed)
        && let Some(level) = json_level(&map)
    {
        return Some(level);
    }
    if let Some(c) = KEY_VALUE.captures(line)
        && let Some(level) = Level::from_word(&c[1])
    {
        return Some(level);
    }
    if let Some(c) = RUBY_LOGGER.captures(line) {
        return Level::from_word(&c[1]);
    }
    if let Some(c) = LINE_START.captures(line) {
        return Level::from_word(&c[1]);
    }
    if let Some(c) = BRACKETED.captures(line) {
        return Level::from_word(&c[1]);
    }
    if let Some(c) = UPPER_WORD.captures(line) {
        return Level::from_word(&c[1]);
    }
    if line.contains("panicked at") {
        return Some(Level::Error);
    }
    None
}

fn json_level(map: &Map<String, Value>) -> Option<Level> {
    let nested = map.get("log").and_then(|l| l.get("level"));
    [
        "level",
        "lvl",
        "severity",
        "loglevel",
        "log.level",
        "levelname",
    ]
    .iter()
    .filter_map(|k| map.get(*k))
    .chain(nested)
    .find_map(|v| match v {
        Value::String(s) => Level::from_word(s),
        Value::Number(n) => n.as_f64().and_then(Level::from_number),
        _ => None,
    })
}

/// A line of a log, with when it was logged when that is known.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LogLine {
    /// Milliseconds since the Unix epoch: when a buffer received it, or the timestamp a file line carries.
    pub at_ms: Option<u64>,
    /// The line, without its newline.
    pub text: String,
}

/// A bounded buffer of a logs pane's lines, filled by the app from the pane's command and read by
/// `logs.query`. Clones share the same lines, so the app can keep one and put another in the
/// [`BuiltinContext`]. When full, the oldest lines are dropped.
#[derive(Clone, Debug)]
pub struct LogBuffer {
    inner: Arc<Mutex<VecDeque<LogLine>>>,
    capacity: usize,
}

impl Default for LogBuffer {
    /// A buffer of [`DEFAULT_LOG_CAPACITY`] lines.
    fn default() -> Self {
        LogBuffer::new(DEFAULT_LOG_CAPACITY)
    }
}

impl LogBuffer {
    /// An empty buffer keeping at most `capacity` lines (at least one).
    pub fn new(capacity: usize) -> LogBuffer {
        let capacity = capacity.max(1);
        LogBuffer {
            inner: Arc::new(Mutex::new(VecDeque::with_capacity(capacity.min(1024)))),
            capacity,
        }
    }

    /// Add a line received now. A trailing newline is dropped; text with several lines is added as several.
    pub fn push(&self, line: impl AsRef<str>) {
        self.push_at(line, SystemTime::now());
    }

    /// Add a line received at `at`.
    pub fn push_at(&self, line: impl AsRef<str>, at: SystemTime) {
        let at_ms = at
            .duration_since(UNIX_EPOCH)
            .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
            .unwrap_or(0);
        let mut lines = self.lock();
        for text in line.as_ref().lines() {
            if lines.len() >= self.capacity {
                lines.pop_front();
            }
            lines.push_back(LogLine {
                at_ms: Some(at_ms),
                text: text.trim_end_matches('\r').to_owned(),
            });
        }
    }

    /// How many lines it holds.
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// True when it holds no lines.
    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    /// The most lines it keeps.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Drop every line.
    pub fn clear(&self) {
        self.lock().clear();
    }

    /// A copy of its lines, oldest first.
    pub fn snapshot(&self) -> Vec<LogLine> {
        self.lock().iter().cloned().collect()
    }

    fn lock(&self) -> MutexGuard<'_, VecDeque<LogLine>> {
        // A panic while holding the lock leaves the lines usable; keep going with them.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Where `logs.query` reads.
#[derive(Clone, Debug)]
pub enum LogSource {
    /// A logs pane's buffer, filled from its command.
    Buffer(LogBuffer),
    /// A file, relative to the worktree unless absolute. Only its last 8 MiB are read. Lines are timed by the
    /// timestamp they carry (ISO 8601 near the start, or a JSON `time`/`timestamp`/`ts` field; without a zone
    /// it is taken as UTC); a line without one takes the previous line's.
    File(String),
}

pub(crate) fn query(args: &Map<String, Value>, ctx: &BuiltinContext) -> Result<BuiltinOutput> {
    query_at(args, ctx, now_ms())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

pub(crate) fn query_at(
    args: &Map<String, Value>,
    ctx: &BuiltinContext,
    now_ms: u64,
) -> Result<BuiltinOutput> {
    let a = Args::new(
        "logs.query",
        args,
        &["pattern", "level", "since_secs", "limit", "error_threshold"],
    )?;
    let pattern = match a.str("pattern")? {
        Some(p) if !p.is_empty() => Some(
            RegexBuilder::new(p)
                .size_limit(1 << 20)
                .build()
                .map_err(|e| ConnectorError::BadArgs(format!("logs.query: pattern: {e}")))?,
        ),
        _ => None,
    };
    let level = match a.str("level")? {
        Some(l) => Some(Level::from_word(l).ok_or_else(|| {
            ConnectorError::BadArgs(format!(
                "logs.query: level {l:?} is not one of error, warn, info, debug"
            ))
        })?),
        None => None,
    };
    let since = a.int("since_secs", 1, 366 * 24 * 3600)?;
    let limit = a.int("limit", 1, MAX_LIMIT)?.unwrap_or(DEFAULT_LIMIT);
    let threshold = a.int("error_threshold", 0, i64::from(u32::MAX))?;

    let (label, lines) = match &ctx.logs {
        None => {
            return Err(ConnectorError::NotConfigured(
                "no log source is mounted for logs.query".to_owned(),
            ));
        }
        Some(LogSource::Buffer(buffer)) => ("buffer".to_owned(), buffer.snapshot()),
        Some(LogSource::File(path)) => {
            let resolved = ctx.resolve_path(path)?;
            (path.clone(), read_file(&resolved)?)
        }
    };

    let start = |secs: i64| now_ms.saturating_sub(u64::try_from(secs).unwrap_or(0) * 1000);
    let window_start = since.map(start);
    let threshold_secs = since.unwrap_or(DEFAULT_THRESHOLD_WINDOW_SECS);
    let threshold_start = start(threshold_secs);
    let in_since = |at: Option<u64>, from: u64| at.is_some_and(|t| t >= from);

    let mut matched: Vec<(&LogLine, Option<Level>)> = Vec::new();
    let (mut window_lines, mut window_errors) = (0u64, 0u64);
    for line in &lines {
        if pattern.as_ref().is_some_and(|p| !p.is_match(&line.text)) {
            continue;
        }
        let detected = detect_level(&line.text);
        if in_since(line.at_ms, threshold_start) {
            window_lines += 1;
            if detected == Some(Level::Error) {
                window_errors += 1;
            }
        }
        if window_start.is_some_and(|from| !in_since(line.at_ms, from)) {
            continue;
        }
        if level.is_some_and(|want| detected.is_none_or(|d| d < want)) {
            continue;
        }
        matched.push((line, detected));
    }
    let total = matched.len();
    let keep = usize::try_from(limit).unwrap_or(usize::MAX);
    let shown = &matched[total.saturating_sub(keep)..];

    let mut events = Vec::new();
    if let Some(t) = threshold
        && window_errors > u64::try_from(t).unwrap_or(u64::MAX)
    {
        events.push(BuiltinEvent {
            name: LOGS_ERROR.to_owned(),
            detail: format!(
                "{window_errors} error lines in the last {threshold_secs}s of {label} \
                 (threshold {t}, {window_lines} lines in the window)"
            ),
        });
    }

    let json = json!({
        "source": label,
        "lines": shown.iter().map(|(l, lv)| json!({
            "at_ms": l.at_ms,
            "level": lv.map(Level::name),
            "text": l.text,
        })).collect::<Vec<_>>(),
        "matched": total,
        "returned": shown.len(),
        "window_secs": threshold_secs,
        "lines_in_window": window_lines,
        "errors_in_window": window_errors,
        "events": events.iter().map(|e| json!({"event": e.name, "detail": e.detail})).collect::<Vec<_>>(),
    });
    let text = shown
        .iter()
        .map(|(l, _)| l.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    Ok(BuiltinOutput { json, text, events })
}

fn read_file(path: &std::path::Path) -> Result<Vec<LogLine>> {
    let io = |e: std::io::Error| ConnectorError::Io(format!("reading {}: {e}", path.display()));
    let mut file = File::open(path).map_err(io)?;
    let len = file.metadata().map_err(io)?.len();
    let seeked = len > MAX_FILE_BYTES;
    if seeked {
        file.seek(SeekFrom::Start(len - MAX_FILE_BYTES))
            .map_err(io)?;
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES)
        .read_to_end(&mut bytes)
        .map_err(io)?;
    let text = String::from_utf8_lossy(&bytes);
    let mut body: &str = &text;
    if seeked && let Some(i) = body.find('\n') {
        body = &body[i + 1..];
    }
    let mut last = None;
    Ok(body
        .lines()
        .map(|l| {
            let l = l.trim_end_matches('\r');
            if let Some(at) = line_timestamp(l) {
                last = Some(at);
            }
            LogLine {
                at_ms: last,
                text: l.to_owned(),
            }
        })
        .collect())
}

/// The timestamp a line carries, in ms since the epoch.
fn line_timestamp(line: &str) -> Option<u64> {
    let trimmed = line.trim_start();
    if trimmed.starts_with('{')
        && let Ok(Value::Object(map)) = serde_json::from_str::<Value>(trimmed)
    {
        for key in ["time", "timestamp", "ts", "@timestamp", "t", "date"] {
            match map.get(key) {
                Some(Value::String(s)) => {
                    if let Some(at) = parse_timestamp(s) {
                        return Some(at);
                    }
                }
                Some(Value::Number(n)) => {
                    if let Some(f) = n.as_f64().filter(|f| *f > 0.0) {
                        // Seconds unless it is clearly milliseconds.
                        let ms = if f > 1e11 { f } else { f * 1000.0 };
                        return Some(ms as u64);
                    }
                }
                _ => {}
            }
        }
    }
    let mut end = line.len().min(80);
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    parse_timestamp(&line[..end])
}

/// The first ISO 8601-ish timestamp in `text`, in ms since the epoch; UTC when it has no zone.
fn parse_timestamp(text: &str) -> Option<u64> {
    let c = TIMESTAMP.captures(text)?;
    let n = |i: usize| c.get(i).and_then(|m| m.as_str().parse::<i64>().ok());
    let (y, mo, d, h, mi, s) = (n(1)?, n(2)?, n(3)?, n(4)?, n(5)?, n(6)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || s > 60 {
        return None;
    }
    let frac_ms = c
        .get(7)
        .map(|m| {
            let digits: String = m.as_str().chars().chain("000".chars()).take(3).collect();
            digits.parse::<i64>().unwrap_or(0)
        })
        .unwrap_or(0);
    let offset_secs = match c.get(8).map(|m| m.as_str()) {
        None | Some("Z") | Some("z") => 0,
        Some(z) => {
            let sign = if z.starts_with('-') { -1 } else { 1 };
            let digits: String = z.chars().filter(char::is_ascii_digit).collect();
            let hh = digits.get(0..2)?.parse::<i64>().ok()?;
            let mm = digits.get(2..4)?.parse::<i64>().ok()?;
            sign * (hh * 3600 + mm * 60)
        }
    };
    let days = days_from_civil(y, mo, d);
    let secs = days * 86_400 + h * 3600 + mi * 60 + s - offset_secs;
    u64::try_from(secs * 1000 + frac_ms).ok()
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn args(v: Value) -> Map<String, Value> {
        match v {
            Value::Object(m) => m,
            _ => Map::new(),
        }
    }

    #[test]
    fn levels_are_detected_in_common_formats() {
        let cases = [
            (
                "2024-01-01 10:00:00 ERROR something broke",
                Some(Level::Error),
            ),
            ("[error] upstream timed out", Some(Level::Error)),
            ("<warn> disk nearly full", Some(Level::Warn)),
            ("time=x level=error msg=boom", Some(Level::Error)),
            ("level=info msg=\"an error was handled\"", Some(Level::Info)),
            (r#"{"level":"warn","msg":"slow"}"#, Some(Level::Warn)),
            (r#"{"level":50,"msg":"pino"}"#, Some(Level::Error)),
            (r#"{"severity":"DEBUG"}"#, Some(Level::Debug)),
            ("  INFO  server started", Some(Level::Info)),
            ("WARNING: deprecated", Some(Level::Warn)),
            ("error: could not compile", Some(Level::Error)),
            ("warning[E0001]: unused", Some(Level::Warn)),
            (
                "E, [2024-01-01T10:00:00.000 #1] ERROR -- : x",
                Some(Level::Error),
            ),
            (
                "I, [2024-01-01T10:00:00.000 #1]  INFO -- : x",
                Some(Level::Info),
            ),
            (
                "thread 'main' panicked at src/main.rs:1:1",
                Some(Level::Error),
            ),
            ("FATAL: out of memory", Some(Level::Error)),
            ("just some text about errors", None),
            ("GET /users 200", None),
        ];
        for (line, want) in cases {
            assert_eq!(detect_level(line), want, "{line}");
        }
    }

    #[test]
    fn timestamps_parse_with_zones() {
        assert_eq!(parse_timestamp("1970-01-01T00:00:01Z"), Some(1000));
        assert_eq!(parse_timestamp("1970-01-01 00:00:01.5"), Some(1500));
        assert_eq!(parse_timestamp("1970-01-01T01:00:00+01:00"), Some(0));
        assert_eq!(
            parse_timestamp("[2024-03-01T12:00:00.123456Z]"),
            Some(1_709_294_400_123)
        );
        assert_eq!(
            line_timestamp(r#"{"ts":1709294400}"#),
            Some(1_709_294_400_000)
        );
        assert_eq!(
            line_timestamp(r#"{"time":"2024-03-01T12:00:00Z"}"#),
            Some(1_709_294_400_000)
        );
        assert_eq!(parse_timestamp("no time here"), None);
    }

    #[test]
    fn the_buffer_is_bounded_and_shared() {
        let b = LogBuffer::new(3);
        let c = b.clone();
        b.push("a\nb");
        b.push("c");
        c.push("d\n");
        let texts: Vec<_> = b.snapshot().into_iter().map(|l| l.text).collect();
        assert_eq!(texts, ["b", "c", "d"]);
        assert_eq!(c.len(), 3);
        c.clear();
        assert!(b.is_empty());
    }

    fn buffer_ctx() -> (BuiltinContext, u64) {
        let b = LogBuffer::new(100);
        let now = SystemTime::now();
        let old = now - Duration::from_secs(600);
        b.push_at("INFO old start", old);
        b.push_at("ERROR old failure", old);
        b.push_at("INFO GET /users 200", now);
        b.push_at("WARN GET /slow took 3s", now);
        b.push_at("ERROR GET /users 500", now);
        b.push_at("ERROR POST /orders 500", now);
        let mut ctx = BuiltinContext::new("/nonexistent");
        ctx.logs = Some(LogSource::Buffer(b));
        let now_ms = now.duration_since(UNIX_EPOCH).unwrap().as_millis() as u64;
        (ctx, now_ms)
    }

    fn texts(out: &BuiltinOutput) -> Vec<String> {
        out.json["lines"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l["text"].as_str().unwrap().to_owned())
            .collect()
    }

    #[test]
    fn buffer_queries_filter_by_pattern_level_window_and_limit() {
        let (ctx, now) = buffer_ctx();
        let out = query_at(&Map::new(), &ctx, now).unwrap();
        assert_eq!(texts(&out).len(), 6);

        let out = query_at(&args(json!({"level": "error"})), &ctx, now).unwrap();
        assert_eq!(
            texts(&out),
            [
                "ERROR old failure",
                "ERROR GET /users 500",
                "ERROR POST /orders 500"
            ]
        );

        let out = query_at(&args(json!({"level": "warn", "since_secs": 60})), &ctx, now).unwrap();
        assert_eq!(
            texts(&out),
            [
                "WARN GET /slow took 3s",
                "ERROR GET /users 500",
                "ERROR POST /orders 500"
            ]
        );

        let out = query_at(&args(json!({"pattern": r"/users \d+"})), &ctx, now).unwrap();
        assert_eq!(texts(&out), ["INFO GET /users 200", "ERROR GET /users 500"]);
        assert_eq!(out.text, "INFO GET /users 200\nERROR GET /users 500");

        let out = query_at(&args(json!({"limit": 2})), &ctx, now).unwrap();
        assert_eq!(
            texts(&out),
            ["ERROR GET /users 500", "ERROR POST /orders 500"]
        );
        assert_eq!(out.json["matched"], 6);

        assert!(matches!(
            query_at(&args(json!({"pattern": "("})), &ctx, now),
            Err(ConnectorError::BadArgs(_))
        ));
        assert!(matches!(
            query_at(&args(json!({"level": "loud"})), &ctx, now),
            Err(ConnectorError::BadArgs(_))
        ));
        assert!(matches!(
            query_at(&args(json!({"file": "x"})), &ctx, now),
            Err(ConnectorError::BadArgs(_))
        ));
    }

    #[test]
    fn the_error_event_fires_above_the_threshold_in_the_window() {
        let (ctx, now) = buffer_ctx();
        // Two errors in the last 60 s.
        let out = query_at(&args(json!({"error_threshold": 1})), &ctx, now).unwrap();
        assert_eq!(out.events.len(), 1);
        assert_eq!(out.events[0].name, LOGS_ERROR);
        assert!(
            out.events[0].detail.starts_with("2 error lines"),
            "{}",
            out.events[0].detail
        );
        assert_eq!(out.json["events"][0]["event"], LOGS_ERROR);

        let out = query_at(&args(json!({"error_threshold": 2})), &ctx, now).unwrap();
        assert!(out.events.is_empty());
        // A wider window takes in the old error too.
        let out = query_at(
            &args(json!({"error_threshold": 2, "since_secs": 3600})),
            &ctx,
            now,
        )
        .unwrap();
        assert_eq!(out.events.len(), 1);
        // The pattern narrows what counts.
        let out = query_at(
            &args(json!({"error_threshold": 1, "pattern": "orders"})),
            &ctx,
            now,
        )
        .unwrap();
        assert!(out.events.is_empty());
    }

    #[test]
    fn file_queries_use_the_lines_timestamps() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("log")).unwrap();
        std::fs::write(
            dir.path().join("log/app.log"),
            "2024-03-01T11:00:00Z ERROR old\n\
             2024-03-01T11:59:30Z INFO recent\n\
             2024-03-01T11:59:40Z ERROR boom\n\
             \tat a stack frame\n\
             {\"time\":\"2024-03-01T11:59:50Z\",\"level\":\"error\",\"msg\":\"json\"}\n",
        )
        .unwrap();
        let mut ctx = BuiltinContext::new(dir.path());
        ctx.logs = Some(LogSource::File("log/app.log".to_owned()));
        let now = 1_709_294_400_000; // 2024-03-01T12:00:00Z
        let out = query_at(&args(json!({"since_secs": 60})), &ctx, now).unwrap();
        assert_eq!(texts(&out).len(), 4);
        assert_eq!(texts(&out)[2], "\tat a stack frame");
        let out = query_at(
            &args(json!({"level": "error", "error_threshold": 1})),
            &ctx,
            now,
        )
        .unwrap();
        assert_eq!(texts(&out).len(), 3);
        assert_eq!(out.json["lines"][2]["level"], "error");
        assert_eq!(out.events.len(), 1);

        ctx.logs = Some(LogSource::File("../elsewhere.log".to_owned()));
        assert!(matches!(
            query_at(&Map::new(), &ctx, now),
            Err(ConnectorError::Refused(_))
        ));
        ctx.logs = Some(LogSource::File("missing.log".to_owned()));
        assert!(matches!(
            query_at(&Map::new(), &ctx, now),
            Err(ConnectorError::Io(_))
        ));
        ctx.logs = None;
        assert!(matches!(
            query_at(&Map::new(), &ctx, now),
            Err(ConnectorError::NotConfigured(_))
        ));
    }

    #[test]
    fn output_is_masked() {
        let b = LogBuffer::new(10);
        b.push("ERROR token=sekrit-value-123 rejected");
        let mut ctx = BuiltinContext::new("/nonexistent");
        ctx.logs = Some(LogSource::Buffer(b));
        ctx.redactor =
            xctx_core::Redactor::new([("API_TOKEN".to_owned(), "sekrit-value-123".to_owned())]);
        let out = crate::run_builtin("logs.query", &Map::new(), &ctx).unwrap();
        assert!(!out.text.contains("sekrit") && out.text.contains("[REDACTED:API_TOKEN]"));
        assert!(!out.json.to_string().contains("sekrit"));
    }
}
