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
    /// A word that is not part of one `SELECT`, in upper case for the model.
    Keyword(String),
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
            Refusal::Keyword(word) => {
                return write!(
                    f,
                    "{word} is not part of a single SELECT, so it does not run here. If it is a name, bracket it, as in [{word}]."
                );
            }
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

/// The reserved words that can appear inside one `SELECT`. Every other reserved word is refused
/// wherever it appears, so the allowlist never has to know which statements exist: SQL Server runs
/// `SELECT 1 AS x SETUSER` as two statements with no semicolon between them (measured), and every
/// statement that can follow a `SELECT` that way begins with a reserved word, a parenthesis, or a
/// label. `HOLDLOCK` and the `MERGE` join hint are left out because a read needs neither.
const SELECT_WORDS: &[&str] = &[
    "ALL",
    "AND",
    "ANY",
    "AS",
    "ASC",
    "BETWEEN",
    "BROWSE",
    "BY",
    "CASE",
    "COALESCE",
    "COLLATE",
    "CONTAINS",
    "CONTAINSTABLE",
    "CONVERT",
    "CROSS",
    "CURRENT",
    "CURRENT_DATE",
    "CURRENT_TIMESTAMP",
    "CURRENT_USER",
    "DESC",
    "DISTINCT",
    "DOUBLE",
    "ELSE",
    "END",
    "ESCAPE",
    "EXCEPT",
    "EXISTS",
    "FETCH",
    "FOR",
    "FREETEXT",
    "FREETEXTTABLE",
    "FROM",
    "FULL",
    "GROUP",
    "HAVING",
    "IDENTITYCOL",
    "IN",
    "INDEX",
    "INNER",
    "INTERSECT",
    "IS",
    "JOIN",
    "LEFT",
    "LIKE",
    "NOT",
    "NULL",
    "NULLIF",
    "OF",
    "ON",
    "OPTION",
    "OR",
    "ORDER",
    "OUTER",
    "OVER",
    "PERCENT",
    "PIVOT",
    "PRECISION",
    "RIGHT",
    "ROWGUIDCOL",
    "SELECT",
    "SEMANTICKEYPHRASETABLE",
    "SEMANTICSIMILARITYDETAILSTABLE",
    "SEMANTICSIMILARITYTABLE",
    "SESSION_USER",
    "SOME",
    "SYSTEM_USER",
    "TABLESAMPLE",
    "THEN",
    "TO",
    "TOP",
    "TRY_CONVERT",
    "UNION",
    "UNPIVOT",
    "USER",
    "VALUES",
    "WHEN",
    "WHERE",
    "WITH",
];

/// The statements that begin with a word SQL Server does not reserve, and `GO`, which the editor
/// reads as a batch separator. Each needs a semicolon after the statement before it (measured), which
/// `tokens` refuses, so this only keeps a parse change from opening them.
const OTHER_STATEMENTS: &[&str] = &["DISABLE", "ENABLE", "GET", "GO", "MOVE", "RECEIVE", "SEND", "THROW"];

/// Reserved words that finish an expression, so a `(` right after one at the top level opens a
/// second statement.
const EXPRESSION_ENDS: &[&str] = &["ASC", "DESC", "END", "NULL"];

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
        if token.kind == Kind::Symbol && token.text == "(" {
            d += 1;
        }
    }
    if d != 0 {
        return Err(Refusal::Unclosed);
    }
    Ok(Tokens { t, depth })
}

/// Admits the statement, or names why it is not a plain single read, without opening a session.
pub(crate) fn read_refusal(sql: &str) -> Result<(), Refusal> {
    let tk = tokens(sql)?;
    let main = main_select(&tk)?;

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
    one_select(&tk, main)?;
    // `SELECT_WORDS` already refuses every verb `is_write` knows. This keeps that true if the list
    // ever grows one.
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

/// Refuses whatever is not part of one `SELECT`: a reserved word outside `SELECT_WORDS` anywhere, and,
/// at the top level after the read's own `SELECT`, anything that would begin a second statement with
/// no semicolon: a `SELECT` that does not follow a set operator, a word in `OTHER_STATEMENTS`, a
/// label's `:`, or a parenthesized query (`opens_statement`). `END`, `ELSE`, and `FETCH` also begin or
/// end statements, so they count as `SELECT` words only inside a `CASE` or after `ROWS`.
fn one_select(tk: &Tokens<'_>, main: usize) -> Result<(), Refusal> {
    let mut cases = Vec::new();
    for (i, t) in tk.t.iter().enumerate() {
        let top = i > main && tk.depth[i] == 0;
        let keyword = || Err(Refusal::Keyword(t.text.to_ascii_uppercase()));
        if t.kind == Kind::Symbol {
            if top && (t.text == ":" || t.text == "(" && opens_statement(tk, i, main)) {
                return Err(Refusal::SecondStatement);
            }
        } else if tk.word(i, "CASE") {
            cases.push(tk.depth[i]);
        } else if tk.word(i, "END") {
            if cases.last() != Some(&tk.depth[i]) {
                return keyword();
            }
            cases.pop();
        } else if tk.word(i, "ELSE") {
            if cases.last() != Some(&tk.depth[i]) {
                return keyword();
            }
        } else if tk.word(i, "FETCH") {
            if !tk.word_in(i - 1, &["ROW", "ROWS"]) {
                return keyword();
            }
        } else if tk.word(i, "SELECT") {
            if top && !set_operand(tk, i) {
                return Err(Refusal::SecondStatement);
            }
        } else if tk.reserved(i) {
            if !tk.word_in(i, SELECT_WORDS) {
                return keyword();
            }
        } else if top && !tk.sym(i - 1, ".") && tk.word_in(i, OTHER_STATEMENTS) {
            return keyword();
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

/// Whether the top-level `(` at `i` opens a parenthesized query after a finished expression or an
/// alias, as in `SELECT 1 (SELECT 2)` and `SELECT 1 AS x (SELECT 2)`, which SQL Server runs as two
/// statements (measured). A `TOP` count is not a finished expression, and a derived table's alias may
/// take a column list. The session refuses a second result set too, for the forms this cannot see.
fn opens_statement(tk: &Tokens<'_>, i: usize, main: usize) -> bool {
    let p = i - 1;
    let t = &tk.t[p];
    match t.kind {
        Kind::String | Kind::Number => !tk.word(p - 1, "TOP"),
        Kind::Symbol => t.text == ")" && !tk.word(tk.opener(p) - 1, "TOP"),
        Kind::Word if tk.reserved(p) => tk.word_in(p, EXPRESSION_ENDS),
        Kind::Word | Kind::Ident => tk.word(p - 1, "AS") && !column_list(tk, p, main),
        _ => false,
    }
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
            "SELECT (1) AS one",
            "SELECT (SELECT 1) AS one",
            "WITH c AS (SELECT 1 AS a) SELECT (a) FROM c",
            "SELECT t.id * 2, dbo.t.id * 3 FROM db.dbo.t",
        ] {
            assert_eq!(read_refusal(sql), Ok(()), "{sql}");
        }
    }

    #[test]
    fn writes_and_second_statements_are_refused() {
        assert_eq!(refusal("UPDATE dbo.t SET x = 1"), Some(Refusal::NotSelect));
        assert_eq!(refusal("SELECT 1; SELECT 2"), Some(Refusal::SecondStatement));
        assert_eq!(refusal("SELECT 1 SELECT 2"), Some(Refusal::SecondStatement));
        assert_eq!(refusal("SELECT 1 DROP TABLE dbo.t"), Some(Refusal::Keyword("DROP".into())));
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

    /// Measured on SQL Server 2022: each of these trailing reserved words starts a second statement
    /// that runs without a semicolon, and `DUMP`/`LOAD` would if the server still had them.
    #[test]
    fn every_reserved_word_outside_select_syntax_is_refused() {
        for (sql, word) in [
            ("SELECT 'FIRST_RESULT' AS marker FROM t READTEXT dbo.t.note 0x00 0 5", "READTEXT"),
            ("SELECT 'FIRST' AS marker SETUSER", "SETUSER"),
            ("SELECT 1 AS x WRITETEXT dbo.t.note 0x00 'a'", "WRITETEXT"),
            ("SELECT 1 AS x UPDATETEXT dbo.t.note 0x00 0 NULL 'a'", "UPDATETEXT"),
            ("SELECT 1 AS x DUMP DATABASE d TO DISK = 'f'", "DUMP"),
            ("SELECT 1 AS x LOAD DATABASE d FROM DISK = 'f'", "LOAD"),
            ("SELECT 1 AS x END CONVERSATION 'h'", "END"),
            ("SELECT 1 AS x ELSE SELECT 2", "ELSE"),
            ("SELECT 1 AS x KILL 51", "KILL"),
            ("SELECT 1 AS x CHECKPOINT", "CHECKPOINT"),
            ("SELECT 1 AS x RECONFIGURE", "RECONFIGURE"),
            ("SELECT 1 AS x SHUTDOWN", "SHUTDOWN"),
            ("SELECT 1 AS x REVERT", "REVERT"),
            ("SELECT 1 AS x PRINT 'a'", "PRINT"),
            ("SELECT 1 AS x RAISERROR('a', 16, 1)", "RAISERROR"),
            ("SELECT 1 AS x OPEN c", "OPEN"),
            ("SELECT 1 AS x FETCH NEXT FROM c", "FETCH"),
            ("SELECT 1 AS x GRANT SELECT ON dbo.t TO public", "GRANT"),
            ("SELECT * FROM dbo.t WITH (HOLDLOCK)", "HOLDLOCK"),
            ("SELECT * FROM dbo.t INNER MERGE JOIN dbo.u ON t.id = u.id", "MERGE"),
            ("SELECT t.backup FROM dbo.t", "BACKUP"),
            ("SELECT * FROM (SELECT 1 AS a WHERE 1 = 1 SETUSER) d", "SETUSER"),
        ] {
            assert_eq!(refusal(sql), Some(Refusal::Keyword(word.into())), "{sql}");
        }
    }

    /// Measured: these need a semicolon before them today, which the allowlist refuses, but a parse
    /// change must not open them.
    #[test]
    fn statements_that_begin_with_a_non_reserved_word_are_refused() {
        for word in ["THROW", "SEND", "RECEIVE", "GET", "MOVE", "ENABLE", "DISABLE", "GO"] {
            let sql = format!("SELECT 1 AS x {word} a");
            assert_eq!(refusal(&sql), Some(Refusal::Keyword(word.into())), "{sql}");
        }
        // The same words as name parts, bracketed, or inside parentheses stay names.
        for sql in [
            "SELECT t.receive, t.[move] FROM dbo.t",
            "SELECT [get] FROM dbo.t",
            "SELECT * FROM dbo.t WHERE id IN (SELECT send FROM dbo.u)",
        ] {
            assert_eq!(read_refusal(sql), Ok(()), "{sql}");
        }
    }

    /// Measured: a parenthesized query after an expression or a label starts a second statement.
    #[test]
    fn a_parenthesized_or_labelled_second_statement_is_refused() {
        for sql in [
            "SELECT 1 (SELECT 2 AS y)",
            "SELECT 'a' AS x (SELECT 2 AS y)",
            "SELECT COUNT(*) (SELECT 2 AS y) FROM dbo.t",
            "SELECT TOP 1 name FROM dbo.t ORDER BY name DESC (SELECT 2 AS y)",
            "SELECT CASE WHEN 1 = 1 THEN 1 END (SELECT 2 AS y)",
            "SELECT 1 AS x lbl: (SELECT 2 AS y)",
            "SELECT 1 AS x lbl: SELECT 2",
        ] {
            assert_eq!(refusal(sql), Some(Refusal::SecondStatement), "{sql}");
        }
        for sql in [
            "SELECT TOP 1 (SELECT 2) AS x FROM dbo.t",
            "SELECT TOP (1) (SELECT 2) AS x FROM dbo.t",
            "SELECT 2 * (SELECT 3)",
            "SELECT id FROM dbo.a UNION (SELECT id FROM dbo.b)",
            "SELECT id FROM dbo.t WHERE (id = 1) AND (id < 2)",
            "SELECT id FROM dbo.t ORDER BY (id)",
            "SELECT * FROM dbo.t WITH (NOLOCK) WHERE id IN (1, 2)",
            "SELECT COUNT(*) OVER (PARTITION BY id ORDER BY id ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) FROM dbo.t",
        ] {
            assert_eq!(read_refusal(sql), Ok(()), "{sql}");
        }
    }

    #[test]
    fn the_select_keyword_list_is_reserved_words_only() {
        for word in SELECT_WORDS {
            assert!(RESERVED.contains(word), "{word} is not a SQL Server reserved word");
        }
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
