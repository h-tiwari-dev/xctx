//! The second guard on SQL text: one statement, and for `sql.read` one that only reads.
//!
//! The first guard is the tool's own read-only mode (`sqlite3 -readonly -safe`, or psql with
//! `default_transaction_read_only=on`); this check catches mistakes early and with a clear message, and keeps
//! the command-line tools' own commands (sqlite's `.shell`, psql's `\!`) out of reach. It reads the statement
//! the way the database would: quoted strings, dollar-quoted bodies and comments are skipped, so a `;` or a
//! keyword inside them does not count (block comments nest in PostgreSQL but not in SQLite). A quoted identifier
//! is a name, not a keyword, except that one used as a function (`"pg_read_file"(...)`) is checked against the
//! deny lists like an unquoted one.

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

/// PRAGMAs that only read, when called without `=` and without an argument (with an argument, some of them,
/// such as `user_version(5)` or `journal_mode(WAL)`, set a value).
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

/// The [`READ_PRAGMAS`] that may take an argument, because the argument only says what to read
/// (`table_info(users)`, `foreign_key_check(orders)`, `integrity_check(10)`).
const ARG_PRAGMAS: &[&str] = &[
    "TABLE_INFO",
    "TABLE_XINFO",
    "TABLE_LIST",
    "INDEX_LIST",
    "INDEX_INFO",
    "INDEX_XINFO",
    "FOREIGN_KEY_LIST",
    "FOREIGN_KEY_CHECK",
    "INTEGRITY_CHECK",
    "QUICK_CHECK",
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
];

/// Prefixes of words refused in reads and writes alike: whole families of functions that reach files, large
/// objects or other databases (`dblink_connect`, `dblink_send_query`, `lo_import`, `pg_ls_dir`,
/// `pg_read_file`, `pg_file_write`, `pg_stat_file`), and `COPY`.
const FORBIDDEN_PREFIXES: &[&str] = &[
    "DBLINK",
    "LO_",
    "PG_LS_",
    "PG_READ_",
    "PG_FILE",
    "PG_STAT_FILE",
    "COPY",
];

/// True for a word (or the content of a quoted name) refused in every statement.
fn forbidden_always(word: &str) -> bool {
    FORBIDDEN_ALWAYS.contains(&word) || FORBIDDEN_PREFIXES.iter().any(|p| word.starts_with(p))
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Tok {
    /// An unquoted word, upper-cased.
    Word(String),
    /// A string or a number.
    Lit,
    /// A quoted identifier (`"name"`, and for SQLite `` `name` `` and `[name]`): its content, unescaped and
    /// upper-cased, so a quoted function name such as `"pg_read_file"(...)` is checked like an unquoted one.
    Ident(String),
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
            // PostgreSQL nests block comments; SQLite ends one at the first `*/`.
            b'/' if b.get(i + 1) == Some(&b'*') && dialect == SqlDialect::Sqlite => {
                let close = b[i + 2..]
                    .windows(2)
                    .position(|w| w == b"*/")
                    .ok_or_else(|| refuse("a /* comment is not closed"))?;
                i += 2 + close + 2;
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
            b'&' if dialect == SqlDialect::Postgres
                && matches!(toks.last(), Some(Tok::Word(w)) if w == "U")
                && start > 0
                && matches!(b[start - 1], b'u' | b'U')
                && matches!(b.get(i + 1), Some(b'"' | b'\'')) =>
            {
                return Err(refuse(
                    "U&'...' and U&\"...\" (names and strings with unicode escapes) are not allowed",
                ));
            }
            b'"' => {
                i = close_quote(b, i + 1, b'"', false)
                    .ok_or_else(|| refuse("a quoted name is not closed"))?;
                let name = quoted_name(&sql[start + 1..i - 1], "\"\"", "\"");
                push(&mut toks, Tok::Ident(name), start, i)?;
            }
            b'`' if dialect == SqlDialect::Sqlite => {
                i = close_quote(b, i + 1, b'`', false)
                    .ok_or_else(|| refuse("a quoted name is not closed"))?;
                let name = quoted_name(&sql[start + 1..i - 1], "``", "`");
                push(&mut toks, Tok::Ident(name), start, i)?;
            }
            b'[' if dialect == SqlDialect::Sqlite => {
                i = b[i..]
                    .iter()
                    .position(|&x| x == b']')
                    .map(|p| i + p + 1)
                    .ok_or_else(|| refuse("a [quoted name] is not closed"))?;
                let name = quoted_name(&sql[start + 1..i - 1], "]]", "]");
                push(&mut toks, Tok::Ident(name), start, i)?;
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

/// The content of a quoted name with its doubled quote unescaped, upper-cased.
fn quoted_name(inner: &str, doubled: &str, quote: &str) -> String {
    inner.replace(doubled, quote).to_ascii_uppercase()
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
    if let Some(w) = names(&lexed).find(|w| forbidden_always(w)) {
        return Err(refuse(format!(
            "{w} is not allowed: it reaches outside the database"
        )));
    }
    Ok((lexed, &sql[start..end]))
}

/// The unquoted words, and the quoted names used as functions (followed by `(`), of a statement.
fn names(lexed: &Lexed) -> impl Iterator<Item = &str> {
    lexed.toks.iter().enumerate().filter_map(|(i, t)| match t {
        Tok::Word(w) => Some(w.as_str()),
        Tok::Ident(name) if lexed.toks.get(i + 1) == Some(&Tok::Punct(b'(')) => Some(name.as_str()),
        _ => None,
    })
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
/// functions that reach files, large objects or other databases (`readfile`, `pg_read_*`, `pg_ls_*`, `lo_*`,
/// `dblink*`, ...), also when the function name is quoted (`"pg_read_file"(...)`), PostgreSQL's `U&` escapes,
/// `PRAGMA`s that set a value (`journal_mode(WAL)`), and the tools' own commands (`.shell`, `\!`).
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
    } else if let Some(w) = names(&lexed).find(|w| FORBIDDEN_IN_READ.contains(w)) {
        return Err(refuse(format!(
            "{w} is not allowed in sql.read (writes go through sql.write and the Inbox)"
        )));
    }
    Ok(statement.to_owned())
}

/// `PRAGMA [schema.]name` with `name` in [`READ_PRAGMAS`], or `PRAGMA [schema.]name(arg)` with `name` in
/// [`ARG_PRAGMAS`].
fn check_read_pragma(toks: &[Tok]) -> Result<()> {
    let bad = || {
        refuse(
            "only read-only PRAGMAs such as table_info(name) or user_version, without = or a value to set, \
             may run in sql.read",
        )
    };
    let rest = &toks[1..];
    let (name, after) = match rest {
        [
            Tok::Word(_) | Tok::Ident(_),
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
        [
            Tok::Punct(b'('),
            Tok::Word(_) | Tok::Lit | Tok::Ident(_),
            Tok::Punct(b')'),
        ] if ARG_PRAGMAS.contains(&name.as_str()) => Ok(()),
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
    fn function_families_and_quoted_function_names_are_refused() {
        for d in [Sqlite, Postgres] {
            for sql in [
                "SELECT \"pg_read_file\"('/etc/passwd')",
                "SELECT dblink_connect('c','x'), dblink_send_query('c','DROP TABLE t')",
                "SELECT \"PG_LS_DIR\"('/')",
                "SELECT lo_from_bytea(0, 'x')",
                "SELECT lo_put(1, 0, 'x')",
                "SELECT pg_ls_logdir()",
                "SELECT pg_read_server_files()",
                "SELECT pg_file_write('/tmp/x', 'y', false)",
                "SELECT pg_stat_file('/etc/passwd')",
                "SELECT \"dblink\"('x', 'y')",
                "SELECT \"Pg_Read_Binary_File\"('x')",
                "COPY t TO STDOUT",
            ] {
                read_refused(sql, d);
                assert!(check_write_statement(sql, d).is_err(), "{d:?}: {sql}");
            }
        }
        for sql in ["SELECT `readfile`('x')", "SELECT [writefile]('x', 'y')"] {
            read_refused(sql, Sqlite);
        }
        // Reads may not change settings, however the function is named.
        read_refused(
            "SELECT pg_catalog.\"set_config\"('default_transaction_read_only', 'off', false)",
            Postgres,
        );
        read_refused("SELECT U&\"\\0070g_read_file\"('/etc/passwd')", Postgres);
        read_refused("SELECT U&'\\0041'", Postgres);
        // Quoted names that are only names stay usable.
        read_ok("SELECT \"delete\", \"copy\" FROM \"into\"", Postgres);
        read_ok("SELECT menu&1 FROM t", Postgres);
    }

    #[test]
    fn sqlite_comments_do_not_nest_but_postgres_ones_do() {
        let sql = "SELECT 1 /* /* */ ; DELETE FROM t; -- */";
        read_refused(sql, Sqlite);
        assert!(check_write_statement(sql, Sqlite).is_err());
        assert_eq!(read_ok(sql, Postgres), "SELECT 1");
        assert_eq!(read_ok("SELECT 1 /* a /* b */", Sqlite), "SELECT 1");
        read_refused("SELECT 1 /* a /* b */", Postgres);
    }

    #[test]
    fn pragmas_take_an_argument_only_when_it_names_what_to_read() {
        for sql in [
            "PRAGMA table_info(users)",
            "PRAGMA main.table_xinfo(users)",
            "PRAGMA index_list('users')",
            "PRAGMA foreign_key_check(orders)",
            "PRAGMA integrity_check(10)",
            "PRAGMA \"main\".index_info(\"idx\")",
            "PRAGMA journal_mode",
            "PRAGMA user_version",
        ] {
            read_ok(sql, Sqlite);
        }
        for sql in [
            "PRAGMA journal_mode(WAL)",
            "PRAGMA user_version(5)",
            "PRAGMA application_id(7)",
            "PRAGMA main.foreign_keys(0)",
            "PRAGMA page_size(1024)",
            "PRAGMA encoding('UTF-16')",
            "PRAGMA user_version = 5",
        ] {
            read_refused(sql, Sqlite);
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
