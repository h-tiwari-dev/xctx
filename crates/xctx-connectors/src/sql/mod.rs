//! `sql.read` and `sql.write` through the engines' command-line tools, without a shell.
//!
//! - SQLite: `sqlite3 -init /dev/null -batch -bail -safe [-readonly] -json <db> <statement>`. `-readonly`
//!   opens the file read-only, so a write fails even if the text guard were bypassed; `-safe` turns off
//!   `ATTACH`, dot-commands that reach the system, and functions such as `writefile` and `load_extension`; and
//!   `-init /dev/null` keeps the user's `~/.sqliterc` out.
//! - PostgreSQL: `psql -X --no-password -v ON_ERROR_STOP=1 -P pager=off --csv -d <url> -c <statement>`, with
//!   `PGOPTIONS='-c default_transaction_read_only=on -c statement_timeout=<ms>'` for reads. The URL comes from
//!   the database setting or `DATABASE_URL`; its password is passed in `PGPASSWORD`, never on the command line.
//!   `-X` keeps the user's `~/.psqlrc` out.
//!
//! [`check_read_statement`] and [`check_write_statement`] are the second guard, on the text.

mod guard;

use std::collections::BTreeMap;
use std::path::Path;

use serde::de::{Deserialize, Deserializer, MapAccess, Visitor};
use serde_json::{Map, Value, json};
use xctx_core::Redactor;
use xctx_plugin::{CallPlan, Effect, Invocation, SqlEngine, ToolOutput, run_plan};

pub use guard::{SqlDialect, check_read_statement, check_write_statement};

use crate::args::Args;
use crate::error::{ConnectorError, Result};
use crate::{BuiltinContext, BuiltinOutput};

/// Rows returned when the call gives no limit, and the most it may ask for.
const DEFAULT_ROWS: i64 = 100;
const MAX_ROWS: i64 = 5_000;

/// The sqlite and psql programs, found on the `PATH`.
const SQLITE3: &str = "sqlite3";
const PSQL: &str = "psql";

/// The database a space's sql pane is mounted on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Database {
    /// The engine, which picks the command-line tool.
    pub engine: SqlEngine,
    /// For SQLite, the database file: relative to the worktree unless absolute; a relative path may not leave
    /// the worktree. For PostgreSQL, a `postgres://` URL, or empty to use `DATABASE_URL` from the space's
    /// secrets or env.
    pub location: String,
}

impl Database {
    /// A SQLite database file.
    pub fn sqlite(path: impl Into<String>) -> Database {
        Database {
            engine: SqlEngine::Sqlite,
            location: path.into(),
        }
    }

    /// A PostgreSQL database; an empty `url` means `DATABASE_URL`.
    pub fn postgres(url: impl Into<String>) -> Database {
        Database {
            engine: SqlEngine::Postgres,
            location: url.into(),
        }
    }

    fn dialect(&self) -> SqlDialect {
        match self.engine {
            SqlEngine::Sqlite => SqlDialect::Sqlite,
            SqlEngine::Postgres => SqlDialect::Postgres,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Read,
    Write,
}

/// A command to run: argv and the env vars on top of xctx's.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Command {
    argv: Vec<String>,
    env: BTreeMap<String, String>,
}

fn database(ctx: &BuiltinContext) -> Result<&Database> {
    ctx.database.as_ref().ok_or_else(|| {
        ConnectorError::NotConfigured("no database is mounted for this space".to_owned())
    })
}

pub(crate) fn read(
    args: &Map<String, Value>,
    ctx: &BuiltinContext,
    masker: &mut Redactor,
) -> Result<BuiltinOutput> {
    let a = Args::new("sql.read", args, &["query", "limit"])?;
    let query = a.required_str("query")?;
    let limit = a.int("limit", 1, MAX_ROWS)?.unwrap_or(DEFAULT_ROWS);
    let db = database(ctx)?;
    let statement = check_read_statement(query, db.dialect())?;
    let command = command(db, &statement, Mode::Read, ctx, masker)?;
    let output = run(&command, "sql.read", ctx, masker)?;
    let (columns, mut rows) = match db.engine {
        SqlEngine::Sqlite => parse_sqlite_json(&output.stdout, output.truncated)?,
        SqlEngine::Postgres => parse_csv_table(&output.stdout, output.truncated),
    };
    let keep = usize::try_from(limit).unwrap_or(usize::MAX);
    let truncated = output.truncated || rows.len() > keep;
    rows.truncate(keep);
    let text = table_text(&columns, &rows, truncated);
    Ok(BuiltinOutput {
        json: json!({
            "engine": engine_name(db.engine),
            "columns": columns,
            "row_count": rows.len(),
            "rows": rows,
            "truncated": truncated,
        }),
        text,
        events: Vec::new(),
    })
}

pub(crate) fn write(
    args: &Map<String, Value>,
    ctx: &BuiltinContext,
    masker: &mut Redactor,
) -> Result<BuiltinOutput> {
    let a = Args::new("sql.write", args, &["statement", "reason"])?;
    let statement = a.required_str("statement")?;
    let reason = a.str("reason")?.unwrap_or("");
    let db = database(ctx)?;
    let statement = check_write_statement(statement, db.dialect())?;
    let command = command(db, &statement, Mode::Write, ctx, masker)?;
    let output = run(&command, "sql.write", ctx, masker)?;
    let (changes, columns, rows, text) = match db.engine {
        SqlEngine::Sqlite => {
            let (changes, (columns, rows)) = parse_sqlite_write(&output.stdout)?;
            let mut text = match changes {
                Some(n) => format!("{n} row(s) changed"),
                None => "Done".to_owned(),
            };
            if !rows.is_empty() {
                text.push_str("\n\n");
                text.push_str(&table_text(&columns, &rows, false));
            }
            (changes, columns, rows, text)
        }
        SqlEngine::Postgres => {
            let text = output.stdout.trim().to_owned();
            let changes = text
                .lines()
                .last()
                .and_then(|l| l.split_whitespace().last())
                .and_then(|n| n.parse::<u64>().ok());
            (changes, Vec::new(), Vec::new(), text)
        }
    };
    Ok(BuiltinOutput {
        json: json!({
            "engine": engine_name(db.engine),
            "statement": statement,
            "reason": reason,
            "changes": changes,
            "columns": columns,
            "rows": rows,
        }),
        text,
        events: Vec::new(),
    })
}

fn engine_name(engine: SqlEngine) -> &'static str {
    match engine {
        SqlEngine::Sqlite => "sqlite",
        SqlEngine::Postgres => "postgres",
    }
}

fn command(
    db: &Database,
    statement: &str,
    mode: Mode,
    ctx: &BuiltinContext,
    masker: &mut Redactor,
) -> Result<Command> {
    match db.engine {
        SqlEngine::Sqlite => {
            let path = ctx.resolve_path(&db.location)?;
            if !path.is_file() {
                return Err(ConnectorError::NotConfigured(format!(
                    "the sqlite database {} does not exist",
                    path.display()
                )));
            }
            Ok(sqlite_command(&path, statement, mode))
        }
        SqlEngine::Postgres => {
            let url = if db.location.trim().is_empty() {
                ctx.setting("DATABASE_URL").ok_or_else(|| {
                    ConnectorError::NotConfigured(
                        "no postgres URL: set the database or DATABASE_URL".to_owned(),
                    )
                })?
            } else {
                db.location.trim()
            };
            masker.extend([("DATABASE_URL".to_owned(), url.to_owned())]);
            let (command, password) = psql_command(url, statement, mode, ctx.timeout)?;
            if let Some(p) = password {
                masker.extend([("PGPASSWORD".to_owned(), p)]);
            }
            Ok(command)
        }
    }
}

fn sqlite_command(path: &Path, statement: &str, mode: Mode) -> Command {
    let mut argv: Vec<String> = [SQLITE3, "-init", "/dev/null", "-batch", "-bail", "-safe"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    if mode == Mode::Read {
        argv.push("-readonly".to_owned());
    }
    argv.push("-json".to_owned());
    let path = path.display().to_string();
    // Never let the path look like an option.
    argv.push(if path.starts_with('-') {
        format!("./{path}")
    } else {
        path
    });
    argv.push(statement.to_owned());
    if mode == Mode::Write {
        argv.push("SELECT changes() AS changes".to_owned());
    }
    Command {
        argv,
        env: BTreeMap::new(),
    }
}

/// The psql command for `url`, and the password taken out of the URL.
fn psql_command(
    url: &str,
    statement: &str,
    mode: Mode,
    timeout: std::time::Duration,
) -> Result<(Command, Option<String>)> {
    let bad = |why: &str| {
        ConnectorError::NotConfigured(format!(
            "the postgres URL is not usable: {why} (expected postgres://user:password@host:port/db)"
        ))
    };
    let mut parsed = url::Url::parse(url).map_err(|e| bad(&e.to_string()))?;
    if !matches!(parsed.scheme(), "postgres" | "postgresql") {
        return Err(bad("it is not a postgres:// URL"));
    }
    let password = parsed.password().map(percent_decode);
    parsed
        .set_password(None)
        .map_err(|_| bad("its password cannot be separated"))?;
    let mut argv: Vec<String> = [
        PSQL,
        "-X",
        "--no-password",
        "-v",
        "ON_ERROR_STOP=1",
        "-P",
        "pager=off",
        "--csv",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    if mode == Mode::Read {
        argv.push("-q".to_owned());
    }
    argv.push("-d".to_owned());
    argv.push(parsed.to_string());
    argv.push("-c".to_owned());
    argv.push(statement.to_owned());

    let timeout_ms = timeout.as_millis().max(1);
    let mut options = format!("-c statement_timeout={timeout_ms}");
    if mode == Mode::Read {
        options = format!("-c default_transaction_read_only=on {options}");
    }
    let mut env = BTreeMap::from([
        ("PGOPTIONS".to_owned(), options),
        ("PGCONNECT_TIMEOUT".to_owned(), "10".to_owned()),
        ("PGAPPNAME".to_owned(), "xctx".to_owned()),
    ]);
    if let Some(p) = &password {
        env.insert("PGPASSWORD".to_owned(), p.clone());
    }
    Ok((Command { argv, env }, password))
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(hex) = s.get(i + 1..i + 3)
            && let Ok(v) = u8::from_str_radix(hex, 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn run(
    command: &Command,
    tool: &str,
    ctx: &BuiltinContext,
    masker: &Redactor,
) -> Result<ToolOutput> {
    let plan = CallPlan {
        plugin: "database".to_owned(),
        tool: tool.to_owned(),
        effect: if tool == "sql.write" {
            Effect::Write
        } else {
            Effect::Read
        },
        invocation: Invocation::Run {
            argv: command.argv.clone(),
        },
        env: command.env.clone(),
        cwd: ctx.worktree.clone(),
        redactor: masker.clone(),
    };
    let program = command.argv.first().cloned().unwrap_or_default();
    let output = run_plan(&plan, ctx.timeout, ctx.max_output_bytes.max(1024))
        .map_err(|e| ConnectorError::Tool(format!("{program} could not be run: {e}")))?;
    if output.timed_out {
        return Err(ConnectorError::Tool(format!(
            "{program} took longer than {}s and was stopped",
            ctx.timeout.as_secs_f32()
        )));
    }
    if !output.success() {
        let why = output.stderr.trim();
        let why = if why.is_empty() {
            output.stdout.trim()
        } else {
            why
        };
        return Err(ConnectorError::Tool(format!(
            "{program} failed ({}): {why}",
            output
                .status
                .map_or_else(|| "killed".to_owned(), |s| format!("exit {s}"))
        )));
    }
    Ok(output)
}

/// A JSON object with its keys in their order (sqlite prints columns in select order).
struct OrderedRow(Vec<(String, Value)>);

impl<'de> Deserialize<'de> for OrderedRow {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = OrderedRow;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a row object")
            }
            fn visit_map<A: MapAccess<'de>>(
                self,
                mut m: A,
            ) -> std::result::Result<OrderedRow, A::Error> {
                let mut fields = Vec::new();
                while let Some((k, v)) = m.next_entry::<String, Value>()? {
                    fields.push((k, v));
                }
                Ok(OrderedRow(fields))
            }
        }
        d.deserialize_map(V)
    }
}

type Table = (Vec<String>, Vec<Vec<Value>>);

fn table_from_rows(rows: Vec<OrderedRow>) -> Table {
    let columns: Vec<String> = rows
        .first()
        .map(|r| r.0.iter().map(|(k, _)| k.clone()).collect())
        .unwrap_or_default();
    let rows = rows
        .into_iter()
        .map(|r| r.0.into_iter().map(|(_, v)| v).collect())
        .collect();
    (columns, rows)
}

/// sqlite3 `-json` output: `[{...},\n{...}]`, one row per line, or nothing for no rows. When the output was cut,
/// the rows that arrived whole are kept.
fn parse_sqlite_json(out: &str, truncated: bool) -> Result<Table> {
    if out.trim().is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    match serde_json::from_str::<Vec<OrderedRow>>(out.trim()) {
        Ok(rows) => Ok(table_from_rows(rows)),
        Err(e) if !truncated => Err(ConnectorError::Tool(format!(
            "sqlite3 printed something that is not JSON: {e}"
        ))),
        Err(_) => {
            let rows = out
                .lines()
                .map(|l| {
                    l.trim()
                        .trim_start_matches('[')
                        .trim_end_matches([',', ']'])
                })
                .map_while(|l| serde_json::from_str::<OrderedRow>(l).ok())
                .collect();
            Ok(table_from_rows(rows))
        }
    }
}

/// The output of a write: any `RETURNING` rows, then `[{"changes":n}]`.
fn parse_sqlite_write(out: &str) -> Result<(Option<u64>, Table)> {
    let mut arrays = Vec::new();
    for item in serde_json::Deserializer::from_str(out).into_iter::<Vec<OrderedRow>>() {
        arrays.push(item.map_err(|e| {
            ConnectorError::Tool(format!("sqlite3 printed something that is not JSON: {e}"))
        })?);
    }
    let changes = arrays
        .pop()
        .and_then(|a| a.into_iter().next())
        .and_then(|r| r.0.into_iter().find(|(k, _)| k == "changes"))
        .and_then(|(_, v)| v.as_u64());
    Ok((
        changes,
        arrays.pop().map(table_from_rows).unwrap_or_default(),
    ))
}

/// psql `--csv` output: a header, then rows. An unquoted empty field is NULL; a quoted one is an empty string.
/// When the output was cut, the last (maybe partial) record is dropped.
fn parse_csv_table(out: &str, truncated: bool) -> Table {
    let mut records = parse_csv(out);
    if truncated {
        records.pop();
    }
    let mut it = records.into_iter();
    let columns = it
        .next()
        .map(|h| h.into_iter().map(|f| f.unwrap_or_default()).collect())
        .unwrap_or_default();
    let rows = it
        .map(|r| {
            r.into_iter()
                .map(|f| f.map_or(Value::Null, Value::String))
                .collect()
        })
        .collect();
    (columns, rows)
}

/// RFC 4180 records; `None` for an unquoted empty field.
fn parse_csv(text: &str) -> Vec<Vec<Option<String>>> {
    let mut records = Vec::new();
    let mut record = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut in_quotes = false;
    let mut chars = text.chars().peekable();
    let mut any = false;
    let finish = |field: &mut String, quoted: &mut bool| {
        let f = std::mem::take(field);
        let was = std::mem::replace(quoted, false);
        if f.is_empty() && !was { None } else { Some(f) }
    };
    while let Some(c) = chars.next() {
        any = true;
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    in_quotes = false;
                }
            } else {
                field.push(c);
            }
            continue;
        }
        match c {
            '"' => {
                in_quotes = true;
                quoted = true;
            }
            ',' => record.push(finish(&mut field, &mut quoted)),
            '\r' => {}
            '\n' => {
                record.push(finish(&mut field, &mut quoted));
                records.push(std::mem::take(&mut record));
                any = false;
            }
            c => field.push(c),
        }
    }
    if any {
        record.push(finish(&mut field, &mut quoted));
        records.push(record);
    }
    records
}

fn cell(v: &Value) -> String {
    match v {
        Value::Null => "NULL".to_owned(),
        Value::String(s) => s.replace(['\t', '\n'], " "),
        other => other.to_string(),
    }
}

fn table_text(columns: &[String], rows: &[Vec<Value>], truncated: bool) -> String {
    let mut text = columns.join("\t");
    for r in rows {
        text.push('\n');
        text.push_str(&r.iter().map(cell).collect::<Vec<_>>().join("\t"));
    }
    text.push_str(&format!(
        "\n({} row{}{})",
        rows.len(),
        if rows.len() == 1 { "" } else { "s" },
        if truncated { ", more not shown" } else { "" }
    ));
    text.trim_start_matches('\n').to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_builtin;
    use std::time::Duration;

    fn have_sqlite() -> bool {
        let ok = std::process::Command::new(SQLITE3)
            .arg("-version")
            .output()
            .is_ok_and(|o| o.status.success());
        if !ok {
            eprintln!("skipping: sqlite3 is not installed");
        }
        ok
    }

    fn args(v: Value) -> Map<String, Value> {
        v.as_object().cloned().unwrap_or_default()
    }

    /// A worktree with `db/dev.sqlite3` holding a `users` table.
    fn sqlite_space() -> (tempfile::TempDir, BuiltinContext) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("db")).unwrap();
        let db = dir.path().join("db/dev.sqlite3");
        let status = std::process::Command::new(SQLITE3)
            .arg(&db)
            .arg(
                "CREATE TABLE users(id INTEGER PRIMARY KEY, name TEXT, note TEXT); \
                  INSERT INTO users(name, note) VALUES ('ana', 'x;y'), ('bo', NULL), ('cy', '');",
            )
            .status()
            .unwrap();
        assert!(status.success());
        let mut ctx = BuiltinContext::new(dir.path());
        ctx.database = Some(Database::sqlite("db/dev.sqlite3"));
        ctx.timeout = Duration::from_secs(10);
        (dir, ctx)
    }

    fn count(ctx: &BuiltinContext) -> i64 {
        let out = run_builtin(
            "sql.read",
            &args(json!({"query": "SELECT count(*) AS n FROM users"})),
            ctx,
        )
        .unwrap();
        out.json["rows"][0][0].as_i64().unwrap()
    }

    #[test]
    fn sqlite_reads_return_columns_in_order_and_rows() {
        if !have_sqlite() {
            return;
        }
        let (_dir, ctx) = sqlite_space();
        let out = run_builtin(
            "sql.read",
            &args(json!({"query": "SELECT name, id, note FROM users ORDER BY id;"})),
            &ctx,
        )
        .unwrap();
        assert_eq!(out.json["columns"], json!(["name", "id", "note"]));
        assert_eq!(
            out.json["rows"],
            json!([["ana", 1, "x;y"], ["bo", 2, null], ["cy", 3, ""]])
        );
        assert_eq!(out.json["truncated"], false);
        assert_eq!(
            out.text,
            "name\tid\tnote\nana\t1\tx;y\nbo\t2\tNULL\ncy\t3\t\n(3 rows)"
        );

        let out = run_builtin(
            "sql.read",
            &args(json!({"query": "SELECT * FROM users", "limit": 2})),
            &ctx,
        )
        .unwrap();
        assert_eq!(out.json["row_count"], 2);
        assert_eq!(out.json["truncated"], true);

        let out = run_builtin(
            "sql.read",
            &args(json!({"query": "SELECT * FROM users WHERE 0"})),
            &ctx,
        )
        .unwrap();
        assert_eq!(out.json["rows"], json!([]));

        let out = run_builtin(
            "sql.read",
            &args(json!({"query": "PRAGMA table_info(users)"})),
            &ctx,
        )
        .unwrap();
        assert_eq!(out.json["row_count"], 3);

        let err = run_builtin(
            "sql.read",
            &args(json!({"query": "SELECT * FROM nope"})),
            &ctx,
        )
        .unwrap_err();
        assert!(
            matches!(&err, ConnectorError::Tool(m) if m.contains("no such table")),
            "{err:?}"
        );
    }

    #[test]
    fn sqlite_refuses_writes_in_reads_and_runs_approved_writes() {
        if !have_sqlite() {
            return;
        }
        let (_dir, mut ctx) = sqlite_space();
        for q in [
            "DELETE FROM users",
            "SELECT 1; DELETE FROM users",
            "ATTACH DATABASE 'other.db' AS o",
        ] {
            let err = run_builtin("sql.read", &args(json!({"query": q})), &ctx).unwrap_err();
            assert!(matches!(err, ConnectorError::Refused(_)), "{q}: {err:?}");
        }
        let write = args(
            json!({"statement": "UPDATE users SET note = 'n' WHERE id < 3", "reason": "test"}),
        );
        assert!(matches!(
            run_builtin("sql.write", &write, &ctx),
            Err(ConnectorError::Refused(_))
        ));
        assert_eq!(count(&ctx), 3);

        ctx.approved = true;
        let out = run_builtin("sql.write", &write, &ctx).unwrap();
        assert_eq!(out.json["changes"], 2);
        assert_eq!(out.text, "2 row(s) changed");
        let out = run_builtin(
            "sql.write",
            &args(json!({"statement": "INSERT INTO users(name) VALUES ('di') RETURNING id, name", "reason": "r"})),
            &ctx,
        )
        .unwrap();
        assert_eq!(out.json["changes"], 1);
        assert_eq!(out.json["rows"], json!([[4, "di"]]));
        assert_eq!(count(&ctx), 4);

        let err = run_builtin(
            "sql.write",
            &args(json!({"statement": "DELETE FROM users; DROP TABLE users", "reason": "r"})),
            &ctx,
        )
        .unwrap_err();
        assert!(matches!(err, ConnectorError::Refused(_)));
        assert_eq!(count(&ctx), 4);
    }

    #[test]
    fn readonly_blocks_writes_even_past_the_guard() {
        if !have_sqlite() {
            return;
        }
        let (dir, ctx) = sqlite_space();
        let db = dir.path().join("db/dev.sqlite3");
        let masker = Redactor::default();
        // The read command, run on a write the guard would have refused.
        let cmd = sqlite_command(&db, "DELETE FROM users", Mode::Read);
        assert!(cmd.argv.contains(&"-readonly".to_owned()));
        let err = run(&cmd, "sql.read", &ctx, &masker).unwrap_err();
        assert!(
            matches!(&err, ConnectorError::Tool(m) if m.contains("readonly")),
            "{err:?}"
        );
        // -safe blocks files outside the database, even in write mode.
        let cmd = sqlite_command(&db, "SELECT writefile('pwned', 'x')", Mode::Write);
        assert!(run(&cmd, "sql.write", &ctx, &masker).is_err());
        assert!(!dir.path().join("pwned").exists());
        let cmd = sqlite_command(&db, "ATTACH DATABASE 'other.db' AS o", Mode::Write);
        assert!(run(&cmd, "sql.write", &ctx, &masker).is_err());
        assert!(!dir.path().join("other.db").exists());
        assert_eq!(count(&ctx), 3);
    }

    #[test]
    fn sqlite_paths_stay_in_the_worktree_and_must_exist() {
        let (_dir, mut ctx) = sqlite_space();
        ctx.database = Some(Database::sqlite("../outside.db"));
        let q = args(json!({"query": "SELECT 1"}));
        assert!(matches!(
            run_builtin("sql.read", &q, &ctx),
            Err(ConnectorError::Refused(_))
        ));
        ctx.database = Some(Database::sqlite("db/missing.sqlite3"));
        assert!(matches!(
            run_builtin("sql.read", &q, &ctx),
            Err(ConnectorError::NotConfigured(_))
        ));
        ctx.database = None;
        assert!(matches!(
            run_builtin("sql.read", &q, &ctx),
            Err(ConnectorError::NotConfigured(_))
        ));
    }

    #[test]
    fn psql_commands_are_read_only_and_keep_the_password_off_the_command_line() {
        let (cmd, password) = psql_command(
            "postgres://app:s%40cret-pw@db.local:5433/app_dev?sslmode=disable",
            "SELECT 1",
            Mode::Read,
            Duration::from_secs(30),
        )
        .unwrap();
        assert_eq!(password.as_deref(), Some("s@cret-pw"));
        assert_eq!(
            cmd.argv,
            [
                "psql",
                "-X",
                "--no-password",
                "-v",
                "ON_ERROR_STOP=1",
                "-P",
                "pager=off",
                "--csv",
                "-q",
                "-d",
                "postgres://app@db.local:5433/app_dev?sslmode=disable",
                "-c",
                "SELECT 1"
            ]
        );
        assert!(!cmd.argv.iter().any(|a| a.contains("cret")));
        assert_eq!(cmd.env["PGPASSWORD"], "s@cret-pw");
        assert_eq!(
            cmd.env["PGOPTIONS"],
            "-c default_transaction_read_only=on -c statement_timeout=30000"
        );

        let (cmd, _) = psql_command(
            "postgresql://localhost/app",
            "UPDATE t SET a = 1",
            Mode::Write,
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(cmd.env["PGOPTIONS"], "-c statement_timeout=5000");
        assert!(!cmd.argv.contains(&"-q".to_owned()));
        assert!(!cmd.env.contains_key("PGPASSWORD"));

        for bad in ["mysql://x/y", "host=x password=y", ""] {
            assert!(
                psql_command(bad, "SELECT 1", Mode::Read, Duration::from_secs(1)).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn postgres_url_comes_from_database_url_and_is_masked() {
        let mut ctx = BuiltinContext::new("/nonexistent");
        ctx.database = Some(Database::postgres(""));
        let q = args(json!({"query": "SELECT 1"}));
        assert!(matches!(
            run_builtin("sql.read", &q, &ctx),
            Err(ConnectorError::NotConfigured(_))
        ));
        ctx.secrets
            .insert("DATABASE_URL".into(), "nope://user:hunter2-pw@h/db".into());
        let err = run_builtin("sql.read", &q, &ctx).unwrap_err();
        assert!(!err.to_string().contains("hunter2"), "{err}");
        // A read the guard refuses never reaches psql.
        ctx.secrets
            .insert("DATABASE_URL".into(), "postgres://u@h/db".into());
        let err =
            run_builtin("sql.read", &args(json!({"query": "DELETE FROM t"})), &ctx).unwrap_err();
        assert!(matches!(err, ConnectorError::Refused(_)));
    }

    #[test]
    fn csv_parsing_keeps_nulls_quotes_and_newlines() {
        let out = "id,name,note\n1,ana,\"a, \"\"b\"\"\nc\"\n2,,\"\"\n";
        let (cols, rows) = parse_csv_table(out, false);
        assert_eq!(cols, ["id", "name", "note"]);
        assert_eq!(
            rows,
            vec![
                vec![json!("1"), json!("ana"), json!("a, \"b\"\nc")],
                vec![json!("2"), Value::Null, json!("")],
            ]
        );
        let (_, rows) = parse_csv_table("a\n1\n2\n3", true);
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn cut_sqlite_output_keeps_whole_rows() {
        let out = "[{\"a\":1,\"b\":\"x\"},\n{\"a\":2,\"b\":\"y\"},\n{\"a\":3,\"b\":\"z";
        let (cols, rows) = parse_sqlite_json(out, true).unwrap();
        assert_eq!(cols, ["a", "b"]);
        assert_eq!(rows.len(), 2);
        assert!(parse_sqlite_json(out, false).is_err());
    }
}
