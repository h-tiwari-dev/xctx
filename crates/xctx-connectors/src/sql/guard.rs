//! The second guard on SQL text: one statement, and for `sql.read` one that only reads.
//!
//! The first guard is the tool's own read-only mode (`sqlite3 -readonly -safe`, or psql with
//! `default_transaction_read_only=on`); this check catches mistakes early and with a clear message, and keeps
//! the command-line tools' own commands (sqlite's `.shell`, psql's `\!`) out of reach. It reads the statement
//! the way the database would: quoted strings, quoted identifiers, dollar-quoted bodies and comments are skipped,
//! so a `;` or a keyword inside them does not count.

use crate::error::{ConnectorError, Result};

/// Which SQL the guard reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SqlDialect {
    /// SQLite: `[ident]` and `` `ident` `` quoting, `PRAGMA`.
    Sqlite,
    /// PostgreSQL: `$tag$...$tag$` and `E'...'` strings, `SHOW`, `TABLE`.
    Postgres,
}

/// Words a read may start with.
const SQLITE_READ_START: &[&str] = &["SELECT", "WITH", "EXPLAIN", "PRAGMA", "VALUES"];
const POSTGRES_READ_START: &[&str] = &["SELECT", "WITH", "EXPLAIN", "SHOW", "TABLE", "VALUES"];

/// PRAGMAs that only read, when called without `=`.
const READ_PRAGMAS: &[&str] = &[
    "TABLE_INFO",
    "TABLE_XINFO",
    "TABLE_LIST",
    "INDEX_LIST",
    "INDEX_INFO",
    "INDEX_XINFO",
    "FOREIGN_KEY_LIST",
    "FOREIGN_KEY_CHECK",
    "DATABASE_LIST",
    "COLLATION_LIST",
    "FUNCTION_LIST",
    "MODULE_LIST",
    "PRAGMA_LIST",
    "COMPILE_OPTIONS",
    "INTEGRITY_CHECK",
    "QUICK_CHECK",
    "USER_VERSION",
    "SCHEMA_VERSION",
    "APPLICATION_ID",
    "PAGE_COUNT",
    "PAGE_SIZE",
    "FREELIST_COUNT",
    "ENCODING",
    "JOURNAL_MODE",
    "FOREIGN_KEYS",
];

/// Words that are never part of a read, wherever they appear (a data-changing `WITH`, `EXPLAIN ANALYZE`,
/// `SELECT INTO`), and functions that reach outside the database.
const FORBIDDEN_IN_READ: &[&str] = &[
    "INSERT",
    "UPDATE",
    "DELETE",
    "MERGE",
    "UPSERT",
    "CREATE",
    "DROP",
    "ALTER",
    "TRUNCATE",
    "GRANT",
    "REVOKE",
    "INTO",
    "ANALYZE",
    "VACUUM",
    "REINDEX",
    "LOCK",
    "NOTIFY",
    "REFRESH",
    "CALL",
    "SET_CONFIG",
    "NEXTVAL",
    "SETVAL",
    "PG_TERMINATE_BACKEND",
    "PG_CANCEL_BACKEND",
    "PG_RELOAD_CONF",
    "PG_ROTATE_LOGFILE",
    "PG_ADVISORY_LOCK",
    "PG_ADVISORY_XACT_LOCK",
];

/// Words refused in reads and writes alike: they reach files, programs or other databases.
const FORBIDDEN_ALWAYS: &[&str] = &[
    "ATTACH",
    "DETACH",
    "COPY",
    "LOAD_EXTENSION",
    "READFILE",
    "WRITEFILE",
    "EDIT",
    "FSDIR",
    "PG_READ_FILE",
    "PG_READ_BINARY_FILE",
    "PG_LS_DIR",
    "PG_STAT_FILE",
    "LO_IMPORT",
    "LO_EXPORT",
    "DBLINK",
    "DBLINK_EXEC",
];

#[derive(Clone, Debug, PartialEq, Eq)]
enum Tok {
    /// An unquoted word, upper-cased.
    Word(String),
    /// A string, a quoted identifier or a number.
    Lit,
    /// Any other character.
    Punct(u8),
}

struct Lexed {
    toks: Vec<Tok>,
    /// The byte range from the first token to the end of the last one before a trailing `;`.
    span: Option<(usize, usize)>,
}

fn refuse(message: impl Into<String>) -> ConnectorError {
    ConnectorError::Refused(message.into())
}

fn lex(sql: &str, dialect: SqlDialect) -> Result<Lexed> {
    let b = sql.as_bytes();
    let n = b.len();
    let mut i = 0;
    let mut toks = Vec::new();
    let mut span: Option<(usize, usize)> = None;
    let mut after_semicolon = false;
    let mut push = |toks: &mut Vec<Tok>, tok: Tok, start: usize, end: usize| -> Result<()> {
        if after_semicolon {
            return Err(refuse(
                "only one statement may run at a time; remove the `;` and what follows it",
            ));
        }
        if tok == Tok::Punct(b';') {
            after_semicolon = true;
            return Ok(());
        }
        span = Some((span.map_or(start, |s| s.0), end));
        toks.push(tok);
        Ok(())
    };
    while i < n {
        let c = b[i];
        let start = i;
        match c {
            c if c.is_ascii_whitespace() => i += 1,
            b'-' if b.get(i + 1) == Some(&b'-') => {
                while i < n && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                let mut depth = 0;
                loop {
                    if i + 1 >= n {
                        return Err(refuse("a /* comment is not closed"));
                    }
                    if b[i] == b'/' && b[i + 1] == b'*' {
                        depth += 1;
                        i += 2;
                    } else if b[i] == b'*' && b[i + 1] == b'/' {
                        depth -= 1;
                        i += 2;
                        if depth == 0 {
                            break;
                        }
                    } else {
                        i += 1;
                    }
                }
            }
            b'\'' => {
                let escapes = dialect == SqlDialect::Postgres
                    && start > 0
                    && matches!(b[start - 1], b'e' | b'E')
                    && matches!(toks.last(), Some(Tok::Word(w)) if w == "E");
                i = close_quote(b, i + 1, b'\'', escapes)
                    .ok_or_else(|| refuse("a string is not closed"))?;
                if escapes {
                    toks.pop();
                }
                push(&mut toks, Tok::Lit, start, i)?;
            }
            b'"' => {
                i = close_quote(b, i + 1, b'"', false)
                    .ok_or_else(|| refuse("a quoted name is not closed"))?;
                push(&mut toks, Tok::Lit, start, i)?;
            }
            b'`' if dialect == SqlDialect::Sqlite => {
                i = close_quote(b, i + 1, b'`', false)
                    .ok_or_else(|| refuse("a quoted name is not closed"))?;
                push(&mut toks, Tok::Lit, start, i)?;
            }
            b'[' if dialect == SqlDialect::Sqlite => {
                i = b[i..]
                    .iter()
                    .position(|&x| x == b']')
                    .map(|p| i + p + 1)
                    .ok_or_else(|| refuse("a [quoted name] is not closed"))?;
                push(&mut toks, Tok::Lit, start, i)?;
            }
            b'$' if dialect == SqlDialect::Postgres && dollar_tag(b, i).is_some() => {
                let tag_end = dollar_tag(b, i).unwrap_or(i + 1);
                let tag = &b[i..tag_end];
                let body = tag_end;
                let close = b[body..]
                    .windows(tag.len())
                    .position(|w| w == tag)
                    .ok_or_else(|| refuse("a $$ string is not closed"))?;
                i = body + close + tag.len();
                push(&mut toks, Tok::Lit, start, i)?;
            }
            b'\\' => {
                return Err(refuse(
                    "a backslash outside a string is not allowed (psql would read it as one of its own commands)",
                ));
            }
            c if c.is_ascii_alphabetic() || c == b'_' || c >= 0x80 => {
                while i < n
                    && (b[i].is_ascii_alphanumeric()
                        || b[i] == b'_'
                        || b[i] == b'$'
                        || b[i] >= 0x80)
                {
                    i += 1;
                }
                let word = sql[start..i].to_ascii_uppercase();
                push(&mut toks, Tok::Word(word), start, i)?;
            }
            c if c.is_ascii_digit() => {
                while i < n && (b[i].is_ascii_alphanumeric() || b[i] == b'.' || b[i] == b'_') {
                    i += 1;
                }
                push(&mut toks, Tok::Lit, start, i)?;
            }
            c => {
                i += 1;
                push(&mut toks, Tok::Punct(c), start, i)?;
            }
        }
    }
    Ok(Lexed { toks, span })
}

/// The index just past the closing `quote` of a quoted run starting at `i`; a doubled quote is an escaped one.
fn close_quote(b: &[u8], mut i: usize, quote: u8, backslash_escapes: bool) -> Option<usize> {
    while i < b.len() {
        if backslash_escapes && b[i] == b'\\' {
            i += 2;
            continue;
        }
        if b[i] == quote {
            if b.get(i + 1) == Some(&quote) {
                i += 2;
                continue;
            }
            return Some(i + 1);
        }
        i += 1;
    }
    None
}

/// The end of a `$tag$` opening at `i`, if it is one (not a `$1` parameter).
fn dollar_tag(b: &[u8], i: usize) -> Option<usize> {
    let mut j = i + 1;
    if b.get(j) == Some(&b'$') {
        return Some(j + 1);
    }
    if !b
        .get(j)
        .is_some_and(|c| c.is_ascii_alphabetic() || *c == b'_')
    {
        return None;
    }
    while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
        j += 1;
    }
    (b.get(j) == Some(&b'$')).then_some(j + 1)
}

fn checked(sql: &str, dialect: SqlDialect) -> Result<(Lexed, &str)> {
    let lexed = lex(sql, dialect)?;
    let Some((start, end)) = lexed.span else {
        return Err(refuse("the statement is empty"));
    };
    if !matches!(lexed.toks.first(), Some(Tok::Word(_))) {
        return Err(refuse(
            "a statement must start with an SQL keyword (the tools' own dot and backslash commands are not allowed)",
        ));
    }
    if let Some(w) = words(&lexed).find(|w| FORBIDDEN_ALWAYS.contains(w)) {
        return Err(refuse(format!(
            "{w} is not allowed: it reaches outside the database"
        )));
    }
    Ok((lexed, &sql[start..end]))
}

fn words(lexed: &Lexed) -> impl Iterator<Item = &str> {
    lexed.toks.iter().filter_map(|t| match t {
        Tok::Word(w) => Some(w.as_str()),
        _ => None,
    })
}

/// Check that `sql` is a single statement that only reads, and return it as it should be run: leading
/// comments and a trailing `;` removed.
///
/// Allowed: `SELECT`, `WITH`, `EXPLAIN` and `VALUES`; for SQLite, the read-only `PRAGMA`s (such as
/// `table_info(x)`) without `=`; for PostgreSQL, `SHOW` and `TABLE`. Refused: several statements, any
/// statement that writes (also inside `WITH`, or as `EXPLAIN ANALYZE`, `SELECT INTO`), `ATTACH`, `COPY`,
/// functions that reach files or other databases (`readfile`, `pg_read_file`, `dblink`, ...), and the tools'
/// own commands (`.shell`, `\!`).
pub fn check_read_statement(sql: &str, dialect: SqlDialect) -> Result<String> {
    let (lexed, statement) = checked(sql, dialect)?;
    let first = match lexed.toks.first() {
        Some(Tok::Word(w)) => w.as_str(),
        _ => "",
    };
    let starts: &[&str] = match dialect {
        SqlDialect::Sqlite => SQLITE_READ_START,
        SqlDialect::Postgres => POSTGRES_READ_START,
    };
    if !starts.contains(&first) {
        return Err(refuse(format!(
            "sql.read runs only {}; {first} is not one of them (writes go through sql.write and the Inbox)",
            starts.join(", ")
        )));
    }
    if first == "PRAGMA" {
        check_read_pragma(&lexed.toks)?;
    } else if let Some(w) = words(&lexed).find(|w| FORBIDDEN_IN_READ.contains(w)) {
        return Err(refuse(format!(
            "{w} is not allowed in sql.read (writes go through sql.write and the Inbox)"
        )));
    }
    Ok(statement.to_owned())
}

/// `PRAGMA [schema.]name` or `PRAGMA [schema.]name(arg)`, with `name` in [`READ_PRAGMAS`].
fn check_read_pragma(toks: &[Tok]) -> Result<()> {
    let bad = || {
        refuse("only read-only PRAGMAs such as table_info(name), without =, may run in sql.read")
    };
    let rest = &toks[1..];
    let (name, after) = match rest {
        [
            Tok::Word(_) | Tok::Lit,
            Tok::Punct(b'.'),
            Tok::Word(name),
            after @ ..,
        ] => (name, after),
        [Tok::Word(name), after @ ..] => (name, after),
        _ => return Err(bad()),
    };
    if !READ_PRAGMAS.contains(&name.as_str()) {
        return Err(bad());
    }
    match after {
        [] => Ok(()),
        [Tok::Punct(b'('), Tok::Word(_) | Tok::Lit, Tok::Punct(b')')] => Ok(()),
        _ => Err(bad()),
    }
}

/// Check that `sql` is a single statement for `sql.write`, and return it as it should be run.
///
/// Refused: several statements, `PRAGMA`, `VACUUM INTO`, `ATTACH`, `COPY`, functions that reach files or other
/// databases, and the tools' own commands. Whether it may run at all is the user's decision in the Inbox.
pub fn check_write_statement(sql: &str, dialect: SqlDialect) -> Result<String> {
    let (lexed, statement) = checked(sql, dialect)?;
    let mut ws = words(&lexed);
    match ws.next() {
        Some("PRAGMA") => Err(refuse("PRAGMA is not run by sql.write")),
        Some("VACUUM") if words(&lexed).any(|w| w == "INTO") => Err(refuse(
            "VACUUM INTO writes a file and is not run by sql.write",
        )),
        _ => Ok(statement.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use SqlDialect::*;

    fn read_ok(sql: &str, d: SqlDialect) -> String {
        match check_read_statement(sql, d) {
            Ok(s) => s,
            Err(e) => panic!("{sql}: {e}"),
        }
    }

    fn read_refused(sql: &str, d: SqlDialect) {
        assert!(
            matches!(
                check_read_statement(sql, d),
                Err(ConnectorError::Refused(_))
            ),
            "should refuse: {sql}"
        );
    }

    #[test]
    fn reads_are_allowed_and_trimmed() {
        assert_eq!(
            read_ok("  -- why\n/* note */ SELECT 1;  ", Sqlite),
            "SELECT 1"
        );
        assert_eq!(
            read_ok("select 'a;b', \"c;d\" from t;", Sqlite),
            "select 'a;b', \"c;d\" from t"
        );
        read_ok("WITH x AS (SELECT 1) SELECT * FROM x", Postgres);
        read_ok("EXPLAIN QUERY PLAN SELECT * FROM t", Sqlite);
        read_ok("PRAGMA table_info(users)", Sqlite);
        read_ok("PRAGMA main.index_list('users')", Sqlite);
        read_ok("PRAGMA user_version", Sqlite);
        read_ok("SHOW search_path", Postgres);
        read_ok(
            "SELECT $$ ; DELETE $$, E'it\\'s; DROP', $1 FROM t",
            Postgres,
        );
        read_ok("SELECT [delete], `update` FROM t", Sqlite);
        read_ok("SELECT replace(name, 'a', 'b') AS comment FROM t", Sqlite);
        read_ok("SELECT a[1] FROM t", Postgres);
    }

    #[test]
    fn writes_and_tricks_are_refused_in_reads() {
        for sql in [
            "",
            " ; ",
            "-- only a comment",
            "INSERT INTO t VALUES (1)",
            "UPDATE t SET a = 1",
            "DELETE FROM t",
            "DROP TABLE t",
            "SELECT 1; DELETE FROM t",
            "SELECT 1;; ",
            "SELECT 1; -- trailing comment is fine but a second statement is not\nSELECT 2",
            "ATTACH DATABASE '/tmp/x.db' AS x",
            "SELECT * FROM t; ATTACH 'x' AS y",
            "SELECT load_extension('/tmp/evil')",
            "SELECT writefile('/tmp/x', 'y')",
            "SELECT readfile('/etc/passwd')",
            "SELECT * FROM fsdir('/')",
            ".shell rm -rf /",
            ".output /tmp/x",
            "PRAGMA journal_mode = WAL",
            "PRAGMA writable_schema",
            "PRAGMA table_info(x); DELETE FROM t",
            "SELECT 'unclosed",
            "SELECT /* unclosed",
            "VACUUM INTO '/tmp/copy.db'",
            "REPLACE INTO t VALUES (1)",
            "SELECT * INTO newtable FROM t",
        ] {
            read_refused(sql, Sqlite);
        }
        for sql in [
            "WITH d AS (DELETE FROM t RETURNING *) SELECT * FROM d",
            "EXPLAIN ANALYZE DELETE FROM t",
            "COPY t TO PROGRAM 'rm -rf /'",
            "SELECT pg_read_file('/etc/passwd')",
            "SELECT dblink_exec('x', 'DROP TABLE t')",
            "SELECT set_config('default_transaction_read_only', 'off', false)",
            "SET default_transaction_read_only = off",
            "BEGIN READ WRITE",
            "\\! rm -rf /",
            "SELECT 1 \\g /tmp/out",
            "SELECT $tag$ unclosed",
            "PRAGMA table_info(x)",
            "SELECT nextval('s')",
        ] {
            read_refused(sql, Postgres);
        }
    }

    #[test]
    fn writes_are_one_statement_and_stay_inside_the_database() {
        assert_eq!(
            check_write_statement("UPDATE t SET a = 'x;y' WHERE id = 1;", Sqlite).unwrap(),
            "UPDATE t SET a = 'x;y' WHERE id = 1"
        );
        check_write_statement("INSERT INTO t VALUES (1) RETURNING id", Postgres).unwrap();
        for sql in [
            "UPDATE t SET a = 1; DROP TABLE t",
            "ATTACH 'x.db' AS x",
            "COPY t FROM PROGRAM 'curl evil'",
            ".shell ls",
            "\\! ls",
            "PRAGMA journal_mode = WAL",
            "VACUUM INTO '/tmp/x'",
            "SELECT writefile('/tmp/x', 'y')",
            "",
        ] {
            assert!(check_write_statement(sql, Sqlite).is_err(), "{sql}");
        }
    }
}
