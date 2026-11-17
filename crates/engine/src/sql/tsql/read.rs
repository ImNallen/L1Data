//! The single-`SELECT` allowlist the assistant's `run_query` applies to a T-SQL statement before it
//! opens a read-only session. A read-only database already refuses every write to the connected
//! database, but it does not fence a sequence advanced in another database, a function that reaches
//! the host, or a second statement, so this admits exactly one read and fails closed on everything
//! else. It is defense in depth, not the guarantee: it cannot see through a view or a function.

use super::builtins::builtin;
use super::{Kind, RESERVED, Token, is_write, lex, selects_into};

/// Why a T-SQL statement is not a plain single read the assistant will run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Refusal {
    Unclosed,
    NotSelect,
    SecondStatement,
    SelectInto,
    Exec,
    OpenExternal,
    LinkedServerName,
    Sequence,
    Use,
    Variable,
    Write,
    /// A function call that is not a vetted in-database built-in, named for the model.
    Unvetted(String),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let reason = match self {
            Refusal::Unclosed => "The statement has an unclosed quote, comment, or parenthesis.",
            Refusal::NotSelect => "Only a single SELECT or WITH … SELECT runs here.",
            Refusal::SecondStatement => "Send one statement. A second statement is not allowed here.",
            Refusal::SelectInto => "SELECT … INTO writes a table, so it does not run here.",
            Refusal::Exec => "EXEC and EXECUTE do not run here.",
            Refusal::OpenExternal => {
                "OPENROWSET, OPENQUERY, OPENDATASOURCE, and OPENXML reach outside the database, so they do not run here."
            }
            Refusal::LinkedServerName => "A four-part (linked-server) name does not run here.",
            Refusal::Sequence => "NEXT VALUE FOR advances a sequence, so it does not run here.",
            Refusal::Use => "USE does not run here. The query runs against the connection's database.",
            Refusal::Variable => "A variable does not run here. Inline the value instead.",
            Refusal::Write => "Only a read runs here. Put a write in a ```sql block for the user to run.",
            Refusal::Unvetted(name) => {
                return write!(f, "The function {name} is not a vetted in-database built-in, so it does not run here.");
            }
        };
        f.write_str(reason)
    }
}

/// The type names that take a length, precision, or scale after `(`, as in `CONVERT(decimal(10, 2),
/// x)`, so a bare one before `(` is a type, not a function call. Reserved type names (`VALUES` is
/// reserved, `CHAR`/`NCHAR`/`CAST`/`CONVERT` are built-ins) are already admitted elsewhere.
const TYPE_NAMES: &[&str] = &[
    "BINARY",
    "DATETIME2",
    "DATETIMEOFFSET",
    "DECIMAL",
    "FLOAT",
    "NUMERIC",
    "NVARCHAR",
    "TIME",
    "VARBINARY",
    "VARCHAR",
];

/// Functions that reach outside the connected database and are reserved words, so the reserved-word
/// allowance below would otherwise admit them. Checked by name anywhere in the statement.
const OPEN_EXTERNAL: &[&str] = &["OPENROWSET", "OPENQUERY", "OPENDATASOURCE", "OPENXML"];

struct Tokens<'a> {
    t: Vec<Token<'a>>,
    depth: Vec<u32>,
}

impl Tokens<'_> {
    fn word(&self, i: usize, word: &str) -> bool {
        self.t.get(i).is_some_and(|t| t.word(word))
    }
    fn word_in(&self, i: usize, words: &[&str]) -> bool {
        words.iter().any(|w| self.word(i, w))
    }
    fn sym(&self, i: usize, text: &str) -> bool {
        self.t.get(i).is_some_and(|t| t.kind == Kind::Symbol && t.text == text)
    }
    fn name(&self, i: usize) -> bool {
        self.t.get(i).is_some_and(|t| matches!(t.kind, Kind::Word | Kind::Ident))
    }
    fn reserved(&self, i: usize) -> bool {
        self.word_in(i, RESERVED)
    }
    /// A name qualified by a preceding `.` or `::`, so a call on it is a schema-qualified function or
    /// a method, never a bare built-in.
    fn qualified(&self, i: usize) -> bool {
        i > 0 && matches!(self.t[i - 1].text, "." | "::")
    }
    fn opener(&self, close: usize) -> usize {
        (0..close).rev().find(|&j| self.depth[j] == self.depth[close] && self.sym(j, "(")).expect("balanced")
    }
    fn closer(&self, open: usize) -> usize {
        (open + 1..self.t.len()).find(|&j| self.depth[j] == self.depth[open] && self.sym(j, ")")).expect("balanced")
    }
}

fn tokens(sql: &str) -> Result<Tokens<'_>, Refusal> {
    let mut t: Vec<Token<'_>> = lex(sql).map_err(|_| Refusal::Unclosed)?.into_iter().filter(|t| !t.comment()).collect();
    let semicolon = |t: &Token<'_>| t.kind == Kind::Symbol && t.text == ";";
    let lead = t.iter().take_while(|t| semicolon(t)).count();
    t.drain(..lead);
    while t.last().is_some_and(semicolon) {
        t.pop();
    }
    let mut depth = Vec::with_capacity(t.len());
    let mut d = 0u32;
    for token in &t {
        if token.kind == Kind::Symbol {
            match token.text {
                ")" => d = d.checked_sub(1).ok_or(Refusal::Unclosed)?,
                // A semicolon left among the tokens separates two statements.
                ";" => return Err(Refusal::SecondStatement),
                _ => {}
            }
        }
        depth.push(d);
        if self_open(token) {
            d += 1;
        }
    }
    if d != 0 {
        return Err(Refusal::Unclosed);
    }
    Ok(Tokens { t, depth })
}

fn self_open(token: &Token<'_>) -> bool {
    token.kind == Kind::Symbol && token.text == "("
}

/// Admits the statement, or names why it is not a plain single read, without opening a session.
pub(crate) fn read_refusal(sql: &str) -> Result<(), Refusal> {
    let tk = tokens(sql)?;
    let main = main_select(&tk)?;

    one_statement(&tk, main)?;

    for (i, t) in tk.t.iter().enumerate() {
        if matches!(t.kind, Kind::Variable | Kind::Reference) {
            return Err(Refusal::Variable);
        }
        if t.word("USE") {
            return Err(Refusal::Use);
        }
        if t.word("EXEC") || t.word("EXECUTE") {
            return Err(Refusal::Exec);
        }
        if tk.word_in(i, OPEN_EXTERNAL) {
            return Err(Refusal::OpenExternal);
        }
        if t.word("NEXT") && tk.word(i + 1, "VALUE") && tk.word(i + 2, "FOR") {
            return Err(Refusal::Sequence);
        }
    }

    if selects_into(&tk.t) {
        return Err(Refusal::SelectInto);
    }
    if four_part_name(&tk) {
        return Err(Refusal::LinkedServerName);
    }
    // `is_write` catches INSERT/UPDATE/DELETE/MERGE, DDL, DBCC, BULK, BACKUP, RESTORE, and an implicit
    // execution (a bare name as the first token), after the specific reasons above have had their say.
    if is_write(sql) {
        return Err(Refusal::Write);
    }
    functions(&tk, main)
}

/// The index of the read's own `SELECT`: the first token, or the one after a `WITH` list whose every
/// entry is `name [(columns)] AS (query)`. This rejects a leading `DECLARE`, `SET`, `USE`, `VALUES`,
/// bare procedure name, `WITH XMLNAMESPACES`, and a CTE that feeds a write.
fn main_select(tk: &Tokens<'_>) -> Result<usize, Refusal> {
    if tk.word(0, "SELECT") {
        return Ok(0);
    }
    if !tk.word(0, "WITH") {
        return Err(Refusal::NotSelect);
    }
    let mut i = 1;
    loop {
        if !tk.name(i) || tk.reserved(i) {
            return Err(Refusal::NotSelect);
        }
        i += 1;
        if tk.sym(i, "(") {
            i = tk.closer(i) + 1;
        }
        if !(tk.word(i, "AS") && tk.sym(i + 1, "(")) {
            return Err(Refusal::NotSelect);
        }
        i = tk.closer(i + 1) + 1;
        if tk.word(i, "SELECT") {
            return Ok(i);
        }
        if !tk.sym(i, ",") {
            return Err(Refusal::NotSelect);
        }
        i += 1;
    }
}

/// Refuses a statement word at the top level after the main `SELECT`, where a batch without
/// semicolons would begin its next statement. A `SELECT` after a set operator continues the one read,
/// and `WITH` at the top level is a table hint (`FROM t WITH (NOLOCK)`), not a new statement.
fn one_statement(tk: &Tokens<'_>, main: usize) -> Result<(), Refusal> {
    let mut cases = 0u32;
    // A word after `.` is a name part (`dbo.backup`), not a statement keyword.
    for i in (main + 1..tk.t.len()).filter(|&i| tk.depth[i] == 0 && !tk.sym(i - 1, ".")) {
        if tk.word(i, "CASE") {
            cases += 1;
        } else if tk.word(i, "END") && cases > 0 {
            cases -= 1;
        } else if (tk.word(i, "ELSE") && cases > 0)
            || tk.word(i, "WITH")
            || (tk.word(i, "FETCH") && tk.word_in(i - 1, &["ROW", "ROWS"]))
            || (tk.word(i, "SELECT") && set_operand(tk, i))
        {
        } else if super::STATEMENT_WORDS.iter().any(|w| tk.word(i, w)) {
            return Err(Refusal::SecondStatement);
        }
    }
    Ok(())
}

/// Whether the `SELECT` at `i` continues a set operation, so it is the right side of `UNION`,
/// `INTERSECT`, or `EXCEPT`, optionally with `ALL`.
fn set_operand(tk: &Tokens<'_>, i: usize) -> bool {
    let op = if tk.word(i - 1, "ALL") { i.checked_sub(2) } else { Some(i - 1) };
    op.is_some_and(|j| tk.word_in(j, &["UNION", "INTERSECT", "EXCEPT"]))
}

/// Whether the name at `i`, before a `(`, is a CTE's or a derived table's alias, so the `(` opens its
/// column list, not a call: `WITH c(id) AS …`, `FROM (SELECT 1) d(a)`, `JOIN (VALUES (1)) AS v(a)`.
/// `main_select` already checked the `WITH` list's shape.
fn column_list(tk: &Tokens<'_>, i: usize, main: usize) -> bool {
    if i < main && tk.depth[i] == 0 {
        return true;
    }
    let close = if tk.word(i - 1, "AS") { i - 2 } else { i - 1 };
    tk.sym(close, ")") && {
        let open = tk.opener(close);
        tk.word_in(open - 1, &["FROM", "JOIN", "APPLY"]) || tk.sym(open - 1, ",")
    }
}

/// A dotted name of four or more parts, the linked-server form `server.db.schema.object`. Every dot
/// separates two parts, so an omitted part still counts: `srv.db..t`, `srv...t`, and `.db.dbo.t` are
/// four. A trailing `.*` is a column list, not a part. A three-part `db.schema.object` cross-database
/// read stays allowed, gated by the login's permissions.
fn four_part_name(tk: &Tokens<'_>) -> bool {
    let mut dots = 0;
    (0..tk.t.len()).any(|i| {
        if !tk.sym(i, ".") {
            if !(tk.name(i) && i > 0 && tk.sym(i - 1, ".")) {
                dots = 0;
            }
            return false;
        }
        dots += 1;
        dots + usize::from(!tk.sym(i + 1, "*")) >= 4
    })
}

/// Fails closed on every function call: a name immediately before `(` is refused unless it is a bare
/// built-in that stays inside the database, a type name, a column list, or reserved-word syntax such
/// as `IN (` or `OVER (`. A qualified call is always refused. The `OPEN*` reserved functions are
/// already refused by name above, so the reserved-word allowance here cannot admit them.
fn functions(tk: &Tokens<'_>, main: usize) -> Result<(), Refusal> {
    for (i, t) in tk.t.iter().enumerate() {
        if !tk.sym(i + 1, "(") || !tk.name(i) {
            continue;
        }
        if tk.qualified(i) {
            return Err(Refusal::Unvetted(name_of(t)));
        }
        if tk.reserved(i) || tk.word_in(i, TYPE_NAMES) {
            continue;
        }
        // A name after `AS` is a type or an alias, and `CROSS APPLY (` and `OUTER APPLY (` are syntax.
        if tk.word(i - 1, "AS")
            || column_list(tk, i, main)
            || tk.word(i, "APPLY") && tk.word_in(i - 1, &["CROSS", "OUTER"])
        {
            continue;
        }
        match builtin(t) {
            Some(b) if !b.reaches_outside => continue,
            _ => return Err(Refusal::Unvetted(name_of(t))),
        }
    }
    Ok(())
}

fn name_of(t: &Token<'_>) -> String {
    t.name().unwrap_or_else(|| t.text.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refusal(sql: &str) -> Option<Refusal> {
        read_refusal(sql).err()
    }

    #[test]
    fn plain_reads_are_admitted() {
        for sql in [
            "SELECT 1",
            "select id, name from dbo.customers where id > 10 order by id",
            "SELECT COUNT(*) AS n FROM dbo.orders",
            "WITH c AS (SELECT id FROM dbo.customers) SELECT * FROM c",
            "SELECT TOP 10 * FROM dbo.orders",
            "SELECT id FROM dbo.a UNION ALL SELECT id FROM dbo.b",
            "SELECT CONVERT(decimal(10, 2), total) FROM dbo.orders",
            "SELECT CASE WHEN x > 0 THEN 'a' ELSE 'b' END FROM dbo.t",
            "SELECT id FROM dbo.orders ORDER BY id OFFSET 0 ROWS FETCH NEXT 5 ROWS ONLY",
            "SELECT * FROM dbo.t WITH (NOLOCK)",
            "SELECT * FROM shop.dbo.orders",
            "SELECT DATEADD(day, 1, GETDATE())",
            "SELECT * FROM STRING_SPLIT('a,b', ',')",
        ] {
            assert_eq!(read_refusal(sql), Ok(()), "{sql}");
        }
    }

    #[test]
    fn writes_and_second_statements_are_refused() {
        assert_eq!(refusal("UPDATE dbo.t SET x = 1"), Some(Refusal::NotSelect));
        assert_eq!(refusal("SELECT 1; SELECT 2"), Some(Refusal::SecondStatement));
        assert_eq!(refusal("SELECT 1 SELECT 2"), Some(Refusal::SecondStatement));
        assert_eq!(refusal("SELECT 1 DROP TABLE dbo.t"), Some(Refusal::SecondStatement));
        assert_eq!(refusal("SELECT * INTO dbo.backup FROM dbo.orders"), Some(Refusal::SelectInto));
        assert_eq!(refusal("SELECT * INTO #t FROM dbo.orders"), Some(Refusal::SelectInto));
        assert_eq!(refusal("DECLARE @x int = 1 SELECT @x"), Some(Refusal::NotSelect));
        assert_eq!(refusal("USE master"), Some(Refusal::NotSelect));
    }

    #[test]
    fn the_review_escapes_are_refused_before_any_session() {
        assert_eq!(refusal("SELECT NEXT VALUE FOR other_db.dbo.seq"), Some(Refusal::Sequence));
        assert_eq!(refusal("select next value for dbo.seq"), Some(Refusal::Sequence));
        assert_eq!(
            refusal("SELECT full_filesystem_path FROM sys.dm_os_enumerate_filesystem('/etc', '*')"),
            Some(Refusal::Unvetted("dm_os_enumerate_filesystem".into()))
        );
        assert_eq!(
            refusal("SELECT * FROM dm_os_enumerate_filesystem('/etc', '*')"),
            Some(Refusal::Unvetted("dm_os_enumerate_filesystem".into()))
        );
    }

    #[test]
    fn external_and_qualified_calls_are_refused() {
        assert_eq!(refusal("SELECT * FROM OPENQUERY(srv, 'select 1')"), Some(Refusal::OpenExternal));
        assert_eq!(refusal("SELECT * FROM OPENROWSET('x', 'y', 'z')"), Some(Refusal::OpenExternal));
        assert_eq!(refusal("SELECT * FROM OPENDATASOURCE('x', 'y')"), Some(Refusal::OpenExternal));
        assert_eq!(refusal("SELECT master.dbo.xp_cmdshell('dir')"), Some(Refusal::Unvetted("xp_cmdshell".into())));
        assert_eq!(refusal("SELECT * FROM srv.db.dbo.tbl"), Some(Refusal::LinkedServerName));
        assert_eq!(refusal("SELECT dbo.my_udf(1)"), Some(Refusal::Unvetted("my_udf".into())));
        assert_eq!(refusal("SELECT my_udf(1)"), Some(Refusal::Unvetted("my_udf".into())));
        // A leading EXEC is not a SELECT; a nested one is the EXEC refusal.
        assert_eq!(refusal("EXEC sys.sp_who"), Some(Refusal::NotSelect));
        assert_eq!(refusal("SELECT * FROM (EXEC master.dbo.xp_cmdshell 'dir') x"), Some(Refusal::Exec));
        assert_eq!(refusal("SELECT * FROM srv.db.dbo.tbl.extra"), Some(Refusal::LinkedServerName));
        assert_eq!(refusal("SELECT four.part.name.here FROM dbo.t"), Some(Refusal::LinkedServerName));
    }

    #[test]
    fn variables_and_bad_syntax_are_refused() {
        assert_eq!(refusal("SELECT @@VERSION"), Some(Refusal::Variable));
        assert_eq!(refusal("SELECT @x"), Some(Refusal::Variable));
        assert_eq!(refusal("SELECT (1"), Some(Refusal::Unclosed));
        assert_eq!(refusal("SELECT 'x"), Some(Refusal::Unclosed));
    }

    #[test]
    fn a_bare_values_is_not_a_statement_sql_server_runs() {
        assert_eq!(refusal("VALUES (1), (2)"), Some(Refusal::NotSelect));
        assert_eq!(read_refusal("SELECT * FROM (VALUES (1), (2)) AS v(a)"), Ok(()));
    }

    #[test]
    fn a_linked_server_name_counts_omitted_parts() {
        for sql in [
            "SELECT * FROM linked_server.remote_db..orders",
            "SELECT * FROM a.b..c",
            "SELECT * FROM a..b.c",
            "SELECT * FROM a...b",
            "SELECT * FROM [a].[b]..[c]",
            "SELECT * FROM [a] . [b] . . [c]",
            "SELECT * FROM a /* x */ . b . /* y */ . c",
            "SELECT * FROM a.\nb..c",
            "SELECT * FROM .db.dbo.t",
            "SELECT srv.db.dbo.t.* FROM dbo.t",
        ] {
            assert_eq!(refusal(sql), Some(Refusal::LinkedServerName), "{sql}");
        }
        for sql in ["SELECT * FROM db..t", "SELECT db.dbo.t.* FROM db.dbo.t", "SELECT t.* FROM dbo.t AS t"] {
            assert_eq!(read_refusal(sql), Ok(()), "{sql}");
        }
    }

    #[test]
    fn cte_and_derived_table_column_lists_are_not_calls() {
        for sql in [
            "WITH c(id) AS (SELECT 1) SELECT id FROM c",
            "WITH c (id, n) AS (SELECT 1, 2), d(x) AS (SELECT id FROM c) SELECT x FROM d",
            "SELECT a FROM (SELECT 1) x(a)",
            "SELECT a FROM (SELECT 1) AS x(a)",
            "SELECT a FROM (VALUES (1)) v(a)",
            "SELECT a FROM dbo.t JOIN (VALUES (1)) v(a) ON v.a = t.id",
            "SELECT a FROM dbo.t, (SELECT 1) d(a)",
            "SELECT a FROM dbo.t CROSS APPLY (VALUES (1)) v(a)",
        ] {
            assert_eq!(read_refusal(sql), Ok(()), "{sql}");
        }
        for (sql, name) in [
            ("SELECT TOP (1) SERVERPROPERTY('Edition')", "SERVERPROPERTY"),
            ("SELECT TOP (1) my_udf(1) FROM dbo.t", "my_udf"),
            ("WITH c(id) AS (SELECT my_udf(1)) SELECT id FROM c", "my_udf"),
            ("SELECT a FROM (SELECT 1) x(a) WHERE my_udf(a) = 1", "my_udf"),
            ("SELECT a FROM (SELECT 1) x(a) CROSS APPLY my_tvf(a)", "my_tvf"),
            (
                "SELECT * FROM (SELECT 1 AS a) AS d CROSS APPLY dm_os_enumerate_filesystem('/etc', '*')",
                "dm_os_enumerate_filesystem",
            ),
        ] {
            assert_eq!(refusal(sql), Some(Refusal::Unvetted(name.into())), "{sql}");
        }
        assert_eq!(refusal("WITH XMLNAMESPACES ('urn:x' AS x) SELECT 1"), Some(Refusal::NotSelect));
        assert_eq!(refusal("WITH c AS (SELECT 1 AS a) DELETE FROM dbo.t"), Some(Refusal::NotSelect));
    }
}
