//! Rewrites a grouped T-SQL `SELECT` into the `SELECT *` of the rows behind one of its result rows, as
//! `sql::drill` does for PostgreSQL. It keeps the `WITH`, `FROM`, and `WHERE` text as written and adds
//! one comparison per `GROUP BY` key, against a literal of the key column's own type. T-SQL's
//! `GROUP BY` cannot name an output alias or a position, so every key is an expression of the `FROM`.

use std::fmt;
use std::ops::Range;

use super::expression::SCALAR_FUNCTIONS;
use super::{Kind, RESERVED, STATEMENT_WORDS, Token, lex, quote_literal};
use crate::sql::drill::one_line;
use crate::sql::mssql_value::input_sql;

/// Why a T-SQL result cannot be drilled into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Refusal {
    Unclosed,
    Statements,
    NotSelect,
    SetOp,
    Into,
    Variable,
    TempTable,
    Unvetted,
    For,
    Option,
    Pivot,
    Window,
    NoFrom,
    NoGroupBy,
    GroupByAll,
    GroupingSets,
    Star,
    AliasKey,
    NotSelected,
    KeyType(String),
    KeyValue,
    NonAscii,
}

impl Refusal {
    /// The refusals of a result that has no groups at all, so it has no rows behind a point to read.
    pub(crate) fn ungrouped(&self) -> bool {
        matches!(self, Refusal::NotSelect | Refusal::NoFrom | Refusal::NoGroupBy)
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let reason = match self {
            Refusal::Unclosed => "The chart's query has an unclosed quote, comment, or parenthesis.",
            Refusal::Statements => "The chart's batch is more than one statement.",
            Refusal::NotSelect => "The chart's query is not a SELECT.",
            Refusal::SetOp => "The chart's query uses UNION, INTERSECT, or EXCEPT.",
            Refusal::Into => "The chart's query uses SELECT INTO.",
            Refusal::Variable => "The chart's query uses a variable.",
            Refusal::TempTable => {
                "The chart's query reads a #temporary table, which the new cell's session cannot see."
            }
            Refusal::Unvetted => "The chart's query calls functions that may not be safe to run again.",
            Refusal::For => "The chart's query has a FOR XML, FOR JSON, or FOR BROWSE clause.",
            Refusal::Option => "The chart's query has an OPTION clause.",
            Refusal::Pivot => "The chart's query uses PIVOT or UNPIVOT.",
            Refusal::Window => "The chart's query has a WINDOW clause.",
            Refusal::NoFrom => "The chart's query has no FROM.",
            Refusal::NoGroupBy => "The chart's query has no GROUP BY.",
            Refusal::GroupByAll => "The chart's query uses GROUP BY ALL.",
            Refusal::GroupingSets => "The chart's query uses GROUPING SETS, ROLLUP, or CUBE.",
            Refusal::Star => "The chart's query selects * before a grouped column.",
            Refusal::AliasKey => "A GROUP BY column has the name of another column's alias. Rename the alias.",
            Refusal::NotSelected => "Each GROUP BY key must also be a column of the result.",
            Refusal::KeyType(t) => return write!(f, "A grouped {t} column cannot be compared to its text."),
            Refusal::KeyValue => "A grouped value cannot be read back as its type.",
            Refusal::NonAscii => {
                "A grouped varchar value is not plain ASCII, so its literal could spell another value in the column's code page."
            }
        };
        f.write_str(reason)
    }
}

/// Words before a trailing word that make it an operand rather than an alias.
const OPERAND_AFTER: &[&str] = &[
    "AND", "OR", "NOT", "IS", "IN", "LIKE", "BETWEEN", "ESCAPE", "AS", "CASE", "WHEN", "THEN", "ELSE", "COLLATE",
    "ZONE", "SOME", "ANY", "ALL", "EXISTS", "DISTINCT", "FROM",
];
/// Trailing words that end an expression instead of naming it.
const NEVER_ALIAS: &[&str] = &["END", "NULL"];

/// Built-ins a drill-down may call, beyond the table filter's `SCALAR_FUNCTIONS` and the reserved
/// words. A bare name is a built-in or a table-valued function of the default schema, so only
/// these run; a qualified name or a method call is never vouched for.
const BUILT_INS: &[&str] = &[
    "APPROX_COUNT_DISTINCT",
    "AVG",
    "CAST",
    "CEILING",
    "CHECKSUM_AGG",
    "CHOOSE",
    "COUNT",
    "COUNT_BIG",
    "CUME_DIST",
    "DATEFROMPARTS",
    "DATETIME2FROMPARTS",
    "DATETIMEFROMPARTS",
    "DATETRUNC",
    "DATE_BUCKET",
    "DENSE_RANK",
    "FIRST_VALUE",
    "FORMAT",
    "GENERATE_SERIES",
    "GREATEST",
    "IIF",
    "ISDATE",
    "ISJSON",
    "ISNUMERIC",
    "JSON_QUERY",
    "JSON_VALUE",
    "LAG",
    "LAST_VALUE",
    "LEAD",
    "LEAST",
    "MAX",
    "MIN",
    "NTILE",
    "OPENJSON",
    "PERCENT_RANK",
    "QUOTENAME",
    "RANK",
    "ROW_NUMBER",
    "SPACE",
    "SQUARE",
    "STDEV",
    "STDEVP",
    "STR",
    "STRING_AGG",
    "STRING_SPLIT",
    "SUM",
    "TIMEFROMPARTS",
    "TRANSLATE",
    "TRY_CAST",
    "TRY_CONVERT",
    "VAR",
    "VARP",
    // Grouping grammar, refused with its own reason.
    "CUBE",
    "ROLLUP",
    "SETS",
    // Type names that take a length, precision, or scale, as in `CONVERT(decimal(10, 2), x)`.
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

/// The functions whose first argument is a type or a date part, a keyword of any case.
const KEYWORD_FIRST: &[&str] = &[
    "CONVERT",
    "TRY_CONVERT",
    "DATEADD",
    "DATEDIFF",
    "DATEDIFF_BIG",
    "DATENAME",
    "DATEPART",
    "DATETRUNC",
    "DATE_BUCKET",
];

/// A grouped `SELECT`, cut into the text the rewrite keeps. Ranges are byte offsets into `sql`.
#[derive(Debug)]
pub(crate) struct Drill<'a> {
    sql: &'a str,
    with: Option<Range<usize>>,
    from: Range<usize>,
    filter: Option<Range<usize>>,
    keys: Vec<Key>,
}

/// One `GROUP BY` key: the result column that shows its value, and the expression that computes it.
#[derive(Debug)]
struct Key {
    column: u16,
    expr: Range<usize>,
}

/// A select item's expression, without its alias. `Unsure` holds both readings of a trailing word
/// that may be an alias or the end of the expression, as in `CURRENT_TIMESTAMP now`.
enum Item {
    Star,
    /// `alias` is the index of the alias token.
    Expr {
        expr: Range<usize>,
        alias: Option<usize>,
    },
    Unsure {
        with: Range<usize>,
        without: Range<usize>,
    },
}

#[derive(Clone, Copy, PartialEq)]
enum Clause {
    From,
    Into,
    Where,
    GroupBy,
    Having,
    Window,
    OrderBy,
    For,
    Option,
    SetOp,
    Pivot,
}

/// How a key column's text becomes a literal of the column's own type.
enum Literal {
    /// The text is already a T-SQL number of the type: an integer, a decimal with its scale, or a bit.
    Number,
    /// A `varchar` literal, so a SQL collation compares it by its non-Unicode rules, as it grouped. The
    /// literal is read in the database's code page, so only ASCII spells the same in every column's.
    Varchar,
    Nvarchar,
    /// `CONVERT(float, N'…')` rounds some decimal texts to a neighbouring double, so both build the
    /// exact binary value instead.
    Float,
    Real,
    /// `mssql_value::input_sql`, which converts with an explicit style.
    Converted,
}

fn literal_kind(display: &str) -> Result<Literal, Refusal> {
    Ok(match display {
        "tinyint" | "smallint" | "int" | "bigint" | "integer" | "decimal" | "bit" => Literal::Number,
        "varchar" => Literal::Varchar,
        "nvarchar" => Literal::Nvarchar,
        "float" => Literal::Float,
        "real" => Literal::Real,
        "money" | "smallmoney" | "date" | "time" | "datetime" | "smalldatetime" | "datetime2" | "datetimeoffset"
        | "uniqueidentifier" | "varbinary" => Literal::Converted,
        _ => return Err(Refusal::KeyType(display.to_owned())),
    })
}

/// Whether a key column of this result type has a literal for each of its values.
pub(crate) fn comparable(display: &str) -> Result<(), Refusal> {
    literal_kind(display).map(|_| ())
}

fn number(text: &str) -> bool {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let unsigned = text.strip_prefix('-').unwrap_or(text);
    match unsigned.split_once('.') {
        Some((whole, fraction)) => digits(whole) && digits(fraction),
        None => digits(unsigned),
    }
}

/// The finite value `text` shows, if `text` is exactly how the result shows that value, so no other
/// value shows the same text.
fn shown<F: std::str::FromStr + ToString>(text: &str, finite: impl Fn(&F) -> bool) -> Result<F, Refusal> {
    text.parse::<F>().ok().filter(|v| finite(v) && v.to_string() == text).ok_or(Refusal::KeyValue)
}

/// A T-SQL `float` expression of exactly `v`: an odd integer below 2^53, which converts exactly, times a
/// power of two, which `POWER` builds exactly down to the smallest subnormal.
fn exact_float(v: f64) -> String {
    let bits = v.to_bits();
    let sign = if v.is_sign_negative() { "-" } else { "" };
    let exponent = ((bits >> 52) & 0x7ff) as i32;
    let fraction = bits & ((1 << 52) - 1);
    let (mut mantissa, mut power) =
        if exponent == 0 { (fraction, -1074) } else { (fraction | 1 << 52, exponent - 1075) };
    if mantissa == 0 {
        return format!("{sign}CAST(0 AS float)");
    }
    let zeros = mantissa.trailing_zeros();
    mantissa >>= zeros;
    power += zeros as i32;
    match power {
        0 => format!("CAST({sign}{mantissa} AS float)"),
        _ => format!("CAST({sign}{mantissa} AS float) * POWER(CAST(2 AS float), {power})"),
    }
}

fn literal(display: &str, value: &str) -> Result<String, Refusal> {
    match literal_kind(display)? {
        Literal::Number if number(value) => Ok(value.to_owned()),
        Literal::Number => Err(Refusal::KeyValue),
        Literal::Varchar if value.is_ascii() => Ok(format!("'{}'", value.replace('\'', "''"))),
        Literal::Varchar => Err(Refusal::NonAscii),
        Literal::Nvarchar => Ok(quote_literal(value)),
        Literal::Float => Ok(exact_float(shown::<f64>(value, |v| v.is_finite())?)),
        Literal::Real => {
            Ok(format!("CAST({} AS real)", exact_float(f64::from(shown::<f32>(value, |v| v.is_finite())?))))
        }
        Literal::Converted => input_sql(display, value).map_err(|_| Refusal::KeyValue),
    }
}

struct Tokens<'a> {
    sql: &'a str,
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
    fn reserved(&self, i: usize) -> bool {
        self.word_in(i, RESERVED)
    }
    fn name(&self, i: usize) -> bool {
        self.t.get(i).is_some_and(|t| t.kind == Kind::Ident || (t.kind == Kind::Word && !self.reserved(i)))
    }
    fn alias(&self, i: usize) -> bool {
        self.name(i) || self.t.get(i).is_some_and(|t| t.kind == Kind::String)
    }
    /// The index of the `)` that closes the `(` at `open`.
    fn close(&self, open: usize) -> Option<usize> {
        (open + 1..self.t.len()).find(|&j| self.depth[j] == self.depth[open] && self.sym(j, ")"))
    }
    /// The source text from the first token to the last, comments between them included.
    fn span(&self, r: &Range<usize>) -> Range<usize> {
        self.t[r.start].start..self.t[r.end - 1].end
    }
    /// The source text between two tokens, trimmed of whitespace, comments kept.
    fn between(&self, after: usize, before: usize) -> Option<Range<usize>> {
        let start = self.t[after].end;
        let end = self.t.get(before).map_or(self.sql.len(), |t| t.start);
        let text = &self.sql[start..end];
        let lead = text.len() - text.trim_start().len();
        let inner = text.trim();
        (!inner.is_empty()).then(|| start + lead..start + lead + inner.len())
    }
    /// `a`, `t.a`, up to `server.db.s.t.a`, with no reserved word among them.
    fn column_ref(&self, r: &Range<usize>) -> bool {
        r.len() % 2 == 1 && r.clone().all(|i| if (i - r.start) % 2 == 1 { self.sym(i, ".") } else { self.name(i) })
    }
    /// A name's text, brackets and quotes removed, in lower case for a comparison that never misses.
    fn folded(&self, i: usize) -> String {
        let t = &self.t[i];
        let quoted = t.text.trim_start_matches(['N', 'n']);
        t.name().unwrap_or_else(|| quoted[1..quoted.len() - 1].replace("''", "'")).to_lowercase()
    }
    /// A word whose case never matters: a reserved word, a built-in function's name, or the type or date
    /// part that starts a `CAST` type or a `CONVERT` or `DATEADD`.
    fn keyword(&self, i: usize) -> bool {
        self.reserved(i)
            || (self.sym(i + 1, "(") && !(i > 0 && matches!(self.t[i - 1].text, "." | "::")))
            || (i > 0 && self.depth[i] > 0 && self.word(i - 1, "AS"))
            || (i > 1 && self.sym(i - 1, "(") && self.word_in(i - 2, KEYWORD_FIRST))
    }
    /// Whether two tokens read the same in any database: names alike in every byte, or keywords alike
    /// in any case. A database with a case-sensitive collation keeps `Region` and `region` apart.
    fn same_token(&self, i: usize, j: usize) -> bool {
        let (a, b) = (&self.t[i], &self.t[j]);
        match (a.kind, b.kind) {
            (Kind::Word, Kind::Word) if a.text.eq_ignore_ascii_case(b.text) => {
                a.text == b.text || (self.keyword(i) && self.keyword(j))
            }
            (Kind::Word | Kind::Ident, Kind::Word | Kind::Ident) => a.name() == b.name(),
            _ => a.kind == b.kind && a.text == b.text,
        }
    }
    fn same(&self, a: &Range<usize>, b: &Range<usize>) -> bool {
        a.len() == b.len() && a.clone().zip(b.clone()).all(|(i, j)| self.same_token(i, j))
    }
    /// Splits `r` at the commas outside parentheses.
    fn list(&self, r: Range<usize>) -> Vec<Range<usize>> {
        let base = self.depth.get(r.start).copied().unwrap_or(0);
        let mut out = Vec::new();
        let mut start = r.start;
        for i in r.clone() {
            if self.sym(i, ",") && self.depth[i] == base {
                out.push(start..i);
                start = i + 1;
            }
        }
        out.push(start..r.end);
        out
    }

    fn item(&self, r: Range<usize>) -> Item {
        let (a, b) = (r.start, r.end);
        if self.sym(b - 1, "*") && (b - a == 1 || self.sym(b - 2, ".")) {
            return Item::Star;
        }
        if b - a >= 3 && self.alias(a) && self.sym(a + 1, "=") {
            return Item::Expr { expr: a + 2..b, alias: Some(a) };
        }
        if b - a >= 3 && self.word(b - 2, "AS") && self.alias(b - 1) {
            return Item::Expr { expr: a..b - 2, alias: Some(b - 1) };
        }
        let plain = Item::Expr { expr: r.clone(), alias: None };
        if b - a < 2 || !self.alias(b - 1) || self.word_in(b - 1, NEVER_ALIAS) {
            return plain;
        }
        let prev = b - 2;
        let aliased = Item::Expr { expr: a..b - 1, alias: Some(b - 1) };
        match self.t[prev].kind {
            Kind::Word if self.word_in(prev, OPERAND_AFTER) => plain,
            Kind::Word if self.word_in(prev, NEVER_ALIAS) || self.column_ref(&(a..b - 1)) => aliased,
            Kind::Word => Item::Unsure { with: r.clone(), without: a..b - 1 },
            Kind::Ident | Kind::Number | Kind::String => aliased,
            Kind::Symbol if self.t[prev].text == ")" => aliased,
            _ => plain,
        }
    }

    fn clause(&self, i: usize) -> Option<Clause> {
        if self.t[i].kind != Kind::Word {
            return None;
        }
        let w = self.t[i].text.to_ascii_lowercase();
        Some(match w.as_str() {
            // `a IS DISTINCT FROM b`
            "from" if i > 0 && self.word(i - 1, "DISTINCT") => return None,
            "from" => Clause::From,
            "into" => Clause::Into,
            "where" => Clause::Where,
            "group" if self.word(i + 1, "BY") => Clause::GroupBy,
            "having" => Clause::Having,
            "window" if self.name(i + 1) && self.word(i + 2, "AS") => Clause::Window,
            "order" if self.word(i + 1, "BY") => Clause::OrderBy,
            // `FROM t FOR SYSTEM_TIME AS OF …` is part of the FROM.
            "for" if self.word_in(i + 1, &["XML", "JSON", "BROWSE"]) => Clause::For,
            "option" if self.sym(i + 1, "(") => Clause::Option,
            "union" | "intersect" | "except" => Clause::SetOp,
            "pivot" | "unpivot" if self.sym(i + 1, "(") => Clause::Pivot,
            _ => return None,
        })
    }

    /// The index of the main `SELECT` after `WITH name [(cols)] AS (…), …`, noting each CTE's name.
    fn after_ctes(&self, mut i: usize, names: &mut Vec<usize>) -> Result<usize, Refusal> {
        loop {
            if !self.name(i) {
                return Err(Refusal::NotSelect);
            }
            names.push(i);
            i += 1;
            if self.sym(i, "(") {
                i = self.close(i).ok_or(Refusal::NotSelect)? + 1;
            }
            if !self.word(i, "AS") || !self.sym(i + 1, "(") {
                return Err(Refusal::NotSelect);
            }
            i = self.close(i + 1).ok_or(Refusal::NotSelect)? + 1;
            if !self.sym(i, ",") {
                break;
            }
            i += 1;
        }
        if self.word(i, "SELECT") { Ok(i) } else { Err(Refusal::NotSelect) }
    }

    /// Refuses what the new cell cannot read the same way: a variable, a session's temporary table, and
    /// a call to anything but a built-in.
    fn vet(&self, ctes: &[usize]) -> Result<(), Refusal> {
        for (i, t) in self.t.iter().enumerate() {
            if matches!(t.kind, Kind::Variable | Kind::Reference) {
                return Err(Refusal::Variable);
            }
            if t.name().is_some_and(|n| n.starts_with('#') && !n.starts_with("##")) {
                return Err(Refusal::TempTable);
            }
            if self.sym(i + 1, "(") && matches!(t.kind, Kind::Word | Kind::Ident) && !ctes.contains(&i) {
                let after = |text: &str| i > 0 && self.t[i - 1].text == text;
                let vetted = !after(".")
                    && !after("::")
                    && ((i > 0 && self.word(i - 1, "AS"))
                        || [RESERVED, SCALAR_FUNCTIONS, BUILT_INS].iter().any(|list| self.word_in(i, list)));
                if !vetted {
                    return Err(Refusal::Unvetted);
                }
            }
        }
        Ok(())
    }

    /// Refuses a statement word at the top level after the main `SELECT`, where a batch without
    /// semicolons starts its next statement.
    fn one_statement(&self, main: usize) -> Result<(), Refusal> {
        let mut cases = 0u32;
        for i in (main + 1..self.t.len()).filter(|&i| self.depth[i] == 0 && !self.sym(i - 1, ".")) {
            if self.word(i, "CASE") {
                cases += 1;
            } else if self.word(i, "END") && cases > 0 {
                cases -= 1;
            } else if (self.word(i, "ELSE") && cases > 0)
                || self.word(i, "WITH")
                || (self.word(i, "FETCH") && self.word_in(i - 1, &["ROW", "ROWS"]))
            {
            } else if self.word_in(i, STATEMENT_WORDS) {
                return Err(Refusal::Statements);
            }
        }
        Ok(())
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
                ";" => return Err(Refusal::Statements),
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
    Ok(Tokens { sql, t, depth })
}

/// Plans the drill-down of one T-SQL batch, the SQL a result's `Origin` holds.
pub(crate) fn plan(sql: &str) -> Result<Drill<'_>, Refusal> {
    let tk = tokens(sql)?;
    let len = tk.t.len();
    let top: Vec<usize> = (0..len).filter(|&i| tk.depth[i] == 0).collect();
    let mut ctes = Vec::new();
    let main = match () {
        _ if tk.word(0, "SELECT") => 0,
        _ if tk.word(0, "WITH") => tk.after_ctes(1, &mut ctes)?,
        // A grouped statement after another, as in `DECLARE @x int = 1 SELECT … GROUP BY …`.
        _ if top.iter().any(|&i| tk.clause(i) == Some(Clause::GroupBy)) => return Err(Refusal::Statements),
        _ => return Err(Refusal::NotSelect),
    };
    if top.iter().any(|&i| tk.clause(i) == Some(Clause::SetOp)) {
        return Err(Refusal::SetOp);
    }
    tk.one_statement(main)?;
    let clauses: Vec<(Clause, usize)> =
        top.iter().filter(|&&i| i > main).filter_map(|&i| tk.clause(i).map(|c| (c, i))).collect();
    for (clause, refusal) in [
        (Clause::Into, Refusal::Into),
        (Clause::For, Refusal::For),
        (Clause::Option, Refusal::Option),
        (Clause::Pivot, Refusal::Pivot),
        (Clause::Window, Refusal::Window),
    ] {
        if clauses.iter().any(|(c, _)| *c == clause) {
            return Err(refusal);
        }
    }
    if top.iter().any(|&i| tk.word(i, "WITH") && tk.word_in(i + 1, &["ROLLUP", "CUBE"])) {
        return Err(Refusal::GroupingSets);
    }
    tk.vet(&ctes)?;
    let mut items_start = main + 1;
    if tk.word_in(items_start, &["ALL", "DISTINCT"]) {
        items_start += 1;
    }
    if tk.word(items_start, "TOP") {
        items_start += 1;
        if tk.sym(items_start, "(") {
            items_start = tk.close(items_start).ok_or(Refusal::NotSelect)? + 1;
        } else if tk.t.get(items_start).is_some_and(|t| t.kind == Kind::Number) {
            items_start += 1;
        } else {
            return Err(Refusal::NotSelect);
        }
        if tk.word(items_start, "PERCENT") {
            items_start += 1;
        }
        if tk.word(items_start, "WITH") && tk.word(items_start + 1, "TIES") {
            items_start += 2;
        }
    }
    let mut next = clauses.iter().copied().peekable();
    let Some((Clause::From, from_at)) = next.next() else { return Err(Refusal::NoFrom) };
    if from_at <= items_start {
        return Err(Refusal::NotSelect);
    }
    let end = |n: Option<&(Clause, usize)>| n.map_or(len, |(_, i)| *i);
    let from = tk.between(from_at, end(next.peek())).ok_or(Refusal::NoFrom)?;
    let filter = match next.next_if(|(c, _)| *c == Clause::Where) {
        Some((_, at)) => Some(tk.between(at, end(next.peek())).ok_or(Refusal::NotSelect)?),
        None => None,
    };
    let Some((Clause::GroupBy, group_at)) = next.next() else { return Err(Refusal::NoGroupBy) };
    let keys_start = group_at + 2;
    let keys_end = end(next.peek());
    if tk.word(keys_start, "ALL") {
        return Err(Refusal::GroupByAll);
    }
    if keys_start >= keys_end {
        return Err(Refusal::NoGroupBy);
    }

    let items: Vec<Item> = tk
        .list(items_start..from_at)
        .into_iter()
        .map(|r| if r.is_empty() { Item::Expr { expr: r, alias: None } } else { tk.item(r) })
        .collect();
    let mut keys: Vec<Key> = Vec::new();
    for key in tk.list(keys_start..keys_end) {
        let (column, expr) = match_key(&tk, &items, key)?;
        if items[..=column].iter().any(|i| matches!(i, Item::Star)) {
            return Err(Refusal::Star);
        }
        let column = u16::try_from(column).map_err(|_| Refusal::NotSelected)?;
        if !keys.iter().any(|k| k.column == column) {
            keys.push(Key { column, expr: tk.span(&expr) });
        }
    }
    Ok(Drill { sql, with: (main > 0).then(|| tk.t[0].start..tk.t[main - 1].end), from, filter, keys })
}

/// The select item a `GROUP BY` key names, and the expression to compare: the same expression, or the
/// same column where one of the two names it bare.
fn match_key(tk: &Tokens<'_>, items: &[Item], key: Range<usize>) -> Result<(usize, Range<usize>), Refusal> {
    let first = key.start;
    if key.is_empty()
        || (tk.word_in(first, &["ROLLUP", "CUBE"]) && tk.sym(first + 1, "("))
        || (tk.word(first, "GROUPING") && tk.word(first + 1, "SETS"))
    {
        return Err(Refusal::GroupingSets);
    }
    if tk.sym(first, "(")
        && tk.close(first) == Some(key.end - 1)
        && (key.len() == 2 || tk.list(first + 1..key.end - 1).len() > 1)
    {
        return Err(Refusal::GroupingSets);
    }
    for (i, item) in items.iter().enumerate() {
        let readings: &[&Range<usize>] = match item {
            Item::Star => &[],
            Item::Expr { expr, .. } => &[expr],
            Item::Unsure { with, without } => &[with, without],
        };
        if let Some(expr) = readings.iter().find(|r| !r.is_empty() && tk.same(r, &key)) {
            return Ok((i, (*expr).clone()));
        }
    }
    // The query ran, so a bare name resolved to one column of the FROM: the one a qualified name of the
    // same column name reads.
    if tk.column_ref(&key) {
        let name = tk.t[key.end - 1].name();
        for (i, item) in items.iter().enumerate() {
            if let Item::Expr { expr, .. } = item
                && tk.column_ref(expr)
                && (key.len() == 1 || expr.len() == 1)
                && tk.t[expr.end - 1].name() == name
            {
                return Ok((i, expr.clone()));
            }
        }
    }
    if key.len() == 1 && tk.name(first) {
        let name = tk.folded(first);
        let alias = |item: &Item| match item {
            Item::Expr { alias: Some(a), .. } => tk.folded(*a) == name,
            Item::Unsure { with, .. } => tk.folded(with.end - 1) == name,
            _ => false,
        };
        if items.iter().any(alias) {
            return Err(Refusal::AliasKey);
        }
    }
    if items.iter().any(|i| matches!(i, Item::Star)) { Err(Refusal::Star) } else { Err(Refusal::NotSelected) }
}

impl Drill<'_> {
    /// The result columns whose values pick the rows, one per distinct `GROUP BY` key.
    pub(crate) fn columns(&self) -> impl Iterator<Item = u16> + '_ {
        self.keys.iter().map(|k| k.column)
    }

    /// The rows behind the result row whose key columns, of result types `types`, hold `values`, both
    /// in the order of `columns`.
    pub(crate) fn sql(&self, types: &[&str], values: &[Option<String>], title: &str) -> Result<String, Refusal> {
        let label = values.iter().map(|v| v.as_deref().unwrap_or("NULL")).collect::<Vec<_>>().join(", ");
        let mut out = format!("-- Rows behind {} in {}\n", one_line(&label), one_line(title));
        if let Some(with) = &self.with {
            out.push_str(&self.sql[with.clone()]);
            out.push('\n');
        }
        out.push_str("SELECT *\nFROM ");
        out.push_str(&self.sql[self.from.clone()]);
        let mut joiner = "\nWHERE ";
        if let Some(filter) = &self.filter {
            let text = &self.sql[filter.clone()];
            let comment = lex(text).ok().and_then(|t| t.last().map(|t| t.kind == Kind::LineComment));
            let newline = if comment == Some(true) { "\n" } else { "" };
            out.push_str(&format!("\nWHERE ({text}{newline})"));
            joiner = "\n  AND ";
        }
        for ((key, display), value) in self.keys.iter().zip(types).zip(values) {
            let expr = &self.sql[key.expr.clone()];
            let test = match value {
                None => "IS NULL".to_owned(),
                Some(v) => format!("= {}", literal(display, v)?),
            };
            out.push_str(&format!("{joiner}({expr}) {test}"));
            joiner = "\n  AND ";
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A key column's result type and its value in the clicked row.
    type Value<'a> = (&'a str, Option<&'a str>);

    fn drill(sql: &str, values: &[Value<'_>]) -> Result<String, Refusal> {
        let plan = plan(sql)?;
        assert_eq!(plan.columns().count(), values.len(), "one value per key in {sql}");
        let types: Vec<&str> = values.iter().map(|(t, _)| *t).collect();
        let values: Vec<Option<String>> = values.iter().map(|(_, v)| v.map(str::to_owned)).collect();
        plan.sql(&types, &values, "Untitled-1.sql")
    }

    fn text(value: &str) -> Value<'_> {
        ("nvarchar", Some(value))
    }

    fn columns(sql: &str) -> Vec<u16> {
        plan(sql).unwrap_or_else(|e| panic!("{sql}: {e}")).columns().collect()
    }

    #[test]
    fn a_grouped_select_becomes_the_rows_of_its_group() {
        let cases: &[(&str, &[Value<'_>], &str)] = &[
            (
                "SELECT c.region, COUNT(*) FROM dbo.orders o JOIN dbo.customers c ON c.id = o.customer_id GROUP BY c.region",
                &[text("Europe")],
                "-- Rows behind Europe in Untitled-1.sql\nSELECT *\nFROM dbo.orders o JOIN dbo.customers c ON c.id = o.customer_id\nWHERE (c.region) = N'Europe'",
            ),
            (
                "select top (5) percent with ties [status], count(*) as n from orders where total > 10 group by [status] having count(*) > 1 order by 2 desc",
                &[text("paid")],
                "-- Rows behind paid in Untitled-1.sql\nSELECT *\nFROM orders\nWHERE (total > 10)\n  AND ([status]) = N'paid'",
            ),
            (
                "SELECT status, COUNT(*) FROM orders GROUP BY status ORDER BY 2 OFFSET 1 ROWS FETCH NEXT 2 ROWS ONLY;",
                &[("nvarchar", None)],
                "-- Rows behind NULL in Untitled-1.sql\nSELECT *\nFROM orders\nWHERE (status) IS NULL",
            ),
            (
                ";WITH paid AS (SELECT * FROM dbo.orders WHERE status = N'paid') SELECT DISTINCT CAST(created_at AS date) AS [day], SUM(total) FROM paid GROUP BY CAST(created_at AS DATE)",
                &[("date", Some("2026-03-01"))],
                "-- Rows behind 2026-03-01 in Untitled-1.sql\nWITH paid AS (SELECT * FROM dbo.orders WHERE status = N'paid')\nSELECT *\nFROM paid\nWHERE (CAST(created_at AS date)) = CONVERT(date, N'2026-03-01', 23)",
            ),
            (
                "SELECT n = COUNT(*), bucket = DATEADD(month, DATEDIFF(month, 0, created_at), 0) FROM orders WITH (NOLOCK) GROUP BY DATEADD(MONTH, DATEDIFF(MONTH, 0, created_at), 0)",
                &[("datetime", Some("2026-01-01 00:00:00.000"))],
                "-- Rows behind 2026-01-01 00:00:00.000 in Untitled-1.sql\nSELECT *\nFROM orders WITH (NOLOCK)\nWHERE (DATEADD(month, DATEDIFF(month, 0, created_at), 0)) = CONVERT(datetime, N'2026-01-01 00:00:00.000', 121)",
            ),
        ];
        for (sql, values, want) in cases {
            assert_eq!(drill(sql, values).as_deref(), Ok(*want), "{sql}");
        }
    }

    #[test]
    fn a_with_prefix_and_comments_stay_as_written() {
        let sql = "-- revenue by month\nWITH paid (id, total, month) AS (SELECT id, total, MONTH(created_at) FROM dbo.orders /* GROUP BY 1 */), x AS (SELECT 1 AS one)\n\
                   SELECT p.month, /* the total */ SUM(p.total)\n\
                   FROM paid p -- only paid\n\
                   WHERE p.note <> N'FROM t GROUP BY x' -- a string that looks like SQL\n\
                   GROUP BY p.month";
        assert_eq!(
            drill(sql, &[("int", Some("3"))]).unwrap(),
            "-- Rows behind 3 in Untitled-1.sql\n\
             WITH paid (id, total, month) AS (SELECT id, total, MONTH(created_at) FROM dbo.orders /* GROUP BY 1 */), x AS (SELECT 1 AS one)\n\
             SELECT *\n\
             FROM paid p -- only paid\n\
             WHERE (p.note <> N'FROM t GROUP BY x' -- a string that looks like SQL\n)\n  \
             AND (p.month) = 3"
        );
    }

    #[test]
    fn every_group_key_picks_the_rows() {
        let sql = "SELECT a, b, COUNT(*) FROM t GROUP BY a, b";
        assert_eq!(columns(sql), [0, 1]);
        assert_eq!(
            drill(sql, &[text("x"), ("int", None)]).unwrap(),
            "-- Rows behind x, NULL in Untitled-1.sql\nSELECT *\nFROM t\nWHERE (a) = N'x'\n  AND (b) IS NULL"
        );
        assert_eq!(columns("SELECT COUNT(*), b, a FROM t GROUP BY a, b, a"), [2, 1], "a repeated key compares once");
        assert_eq!(
            columns("SELECT SUM(x) total, UPPER(name) up FROM t GROUP BY upper(name)"),
            [1],
            "built-ins in any case"
        );
        assert_eq!(
            columns("SELECT [t].[Region] r, COUNT(*) FROM t GROUP BY \"Region\""),
            [0],
            "brackets and quotes alike"
        );
        assert_eq!(
            columns("SELECT t.region, COUNT(*) FROM t GROUP BY region"),
            [0],
            "a bare key names the same column"
        );
        assert_eq!(columns("SELECT region, COUNT(*) FROM t GROUP BY t.region"), [0]);
        assert_eq!(columns("SELECT CONVERT(DATE, x), COUNT(*) FROM t GROUP BY convert(date, x)"), [0]);
        assert_eq!(columns("SELECT YEAR(d) AS 'year', COUNT(*) FROM t GROUP BY YEAR(d)"), [0], "a string alias");
        assert_eq!(
            columns("SELECT COUNT(*), x COLLATE Latin1_General_BIN2 FROM t GROUP BY x COLLATE Latin1_General_BIN2"),
            [1]
        );
        assert_eq!(columns("SELECT CURRENT_TIMESTAMP now, x, COUNT(*) FROM t GROUP BY x"), [1]);
    }

    #[test]
    fn a_name_differing_only_in_case_is_not_the_same_key() {
        assert_eq!(plan("SELECT Region, COUNT(*) FROM t GROUP BY region").map(|_| ()), Err(Refusal::NotSelected));
        assert_eq!(plan("SELECT t.Region, COUNT(*) FROM t GROUP BY region").map(|_| ()), Err(Refusal::NotSelected));
    }

    #[test]
    fn each_value_is_a_literal_of_its_column_type() {
        let sql = "SELECT k, COUNT(*) FROM t GROUP BY k";
        let cases: &[(&str, &str, &str)] = &[
            ("nvarchar", "O'Brien 旅", "N'O''Brien 旅'"),
            ("varchar", "O'Brien", "'O''Brien'"),
            ("int", "-12", "-12"),
            ("integer", "7", "7"),
            ("bigint", "9223372036854775807", "9223372036854775807"),
            ("bit", "1", "1"),
            ("decimal", "-0.50", "-0.50"),
            ("money", "125.5000", "CONVERT(money, N'125.5000')"),
            ("float", "0.1", "CAST(3602879701896397 AS float) * POWER(CAST(2 AS float), -55)"),
            ("real", "3.4", "CAST(CAST(7130317 AS float) * POWER(CAST(2 AS float), -21) AS real)"),
            ("datetime2", "2024-01-02 03:04:05.1234567", "CONVERT(datetime2, N'2024-01-02 03:04:05.1234567', 121)"),
            (
                "datetimeoffset",
                "2024-01-02 03:04:05.1234567+02:00",
                "CONVERT(datetimeoffset, N'2024-01-02 03:04:05.1234567+02:00', 121)",
            ),
            ("time", "03:04:05.1234567", "CONVERT(time, N'03:04:05.1234567', 121)"),
            (
                "uniqueidentifier",
                "abcd1234-5678-90ab-cdef-1234567890ab",
                "CONVERT(uniqueidentifier, N'abcd1234-5678-90ab-cdef-1234567890ab')",
            ),
            ("varbinary", "0xDEAD", "CONVERT(varbinary(max), N'0xDEAD', 1)"),
        ];
        for (display, value, want) in cases {
            let out = drill(sql, &[(display, Some(value))]).unwrap();
            assert!(out.ends_with(&format!("WHERE (k) = {want}")), "{display}: {out}");
        }
        assert_eq!(drill(sql, &[("int", Some("1; DROP TABLE t"))]), Err(Refusal::KeyValue));
        assert_eq!(drill(sql, &[("varbinary", Some("DEAD"))]), Err(Refusal::KeyValue));
        for display in ["sql_variant", "clr", "xml", "null"] {
            assert_eq!(comparable(display), Err(Refusal::KeyType(display.into())));
            assert_eq!(drill(sql, &[(display, Some("x"))]), Err(Refusal::KeyType(display.into())));
        }
        assert_eq!(
            drill(sql, &[("sql_variant", None)]).unwrap().lines().last(),
            Some("WHERE (k) IS NULL"),
            "NULL needs no literal"
        );
    }

    #[test]
    fn a_varchar_value_outside_ascii_is_refused() {
        let sql = "SELECT k, COUNT(*) FROM t GROUP BY k";
        for value in ["Ω", "Straße", "é", "旅", "a\u{a0}b"] {
            assert_eq!(drill(sql, &[("varchar", Some(value))]), Err(Refusal::NonAscii), "{value}");
            assert!(drill(sql, &[("nvarchar", Some(value))]).is_ok(), "{value}");
        }
        assert!(drill(sql, &[("varchar", Some("~ ?\t"))]).unwrap().ends_with("= '~ ?\t'"));
    }

    #[test]
    fn a_float_is_written_as_its_exact_binary_value() {
        let sql = "SELECT k, COUNT(*) FROM t GROUP BY k";
        let last = |display: &str, value: &str| {
            drill(sql, &[(display, Some(value))]).map(|out| out.rsplit_once("= ").unwrap().1.to_owned())
        };
        // `CONVERT(float, N'31722300588172750')` gives 0x435C2CD0EA810973, not this double.
        assert_eq!(f64::from_bits(0x435C_2CD0_EA81_0974).to_string(), "31722300588172750");
        let cases: &[(&str, &str, &str)] = &[
            ("float", "31722300588172750", "CAST(1982643786760797 AS float) * POWER(CAST(2 AS float), 4)"),
            ("float", "1", "CAST(1 AS float)"),
            ("float", "-3", "CAST(-3 AS float)"),
            ("float", "1024", "CAST(1 AS float) * POWER(CAST(2 AS float), 10)"),
            ("float", "0", "CAST(0 AS float)"),
            ("float", "-0", "-CAST(0 AS float)"),
            ("float", &f64::from_bits(1).to_string(), "CAST(1 AS float) * POWER(CAST(2 AS float), -1074)"),
            ("float", &f64::MIN_POSITIVE.to_string(), "CAST(1 AS float) * POWER(CAST(2 AS float), -1022)"),
            ("float", &f64::MAX.to_string(), "CAST(9007199254740991 AS float) * POWER(CAST(2 AS float), 971)"),
            ("float", &f64::MIN.to_string(), "CAST(-9007199254740991 AS float) * POWER(CAST(2 AS float), 971)"),
            ("real", "-0", "CAST(-CAST(0 AS float) AS real)"),
            ("real", "0.1", "CAST(CAST(13421773 AS float) * POWER(CAST(2 AS float), -27) AS real)"),
            ("real", &f32::MAX.to_string(), "CAST(CAST(16777215 AS float) * POWER(CAST(2 AS float), 104) AS real)"),
        ];
        for (display, value, want) in cases {
            assert_eq!(last(display, value).as_deref(), Ok(*want), "{display} {value}");
        }
        for (display, value) in [
            ("float", "0.10000000000000001"),
            ("float", "1e5"),
            ("float", "inf"),
            ("float", "NaN"),
            ("float", "1; DROP TABLE t"),
            ("real", "0.1000000001"),
            ("real", "1e39"),
        ] {
            assert_eq!(last(display, value), Err(Refusal::KeyValue), "{display} {value}");
        }
    }

    #[test]
    fn the_comment_line_cannot_be_broken_out_of() {
        let sql = "SELECT name, COUNT(*) FROM t GROUP BY name";
        let out = drill(sql, &[text("a\nGO\r\nSELECT 1 --\u{2028}b")]).unwrap();
        assert_eq!(out.lines().next(), Some("-- Rows behind a GO SELECT 1 -- b in Untitled-1.sql"));
        assert_eq!(super::super::split_batches(&out).unwrap().len(), 1);
    }

    #[test]
    fn the_drilled_sql_is_a_read_that_may_run_again() {
        let sql = "WITH x AS (SELECT * FROM dbo.orders) SELECT status, COUNT(*) FROM x WHERE total > 1 GROUP BY status";
        let out = drill(sql, &[text("it's INTO; DELETE")]).unwrap();
        assert_eq!(super::super::rerun_refusal(&out), None, "{out}");
    }

    #[test]
    fn an_item_alias_is_read_only_where_it_is_certain() {
        let cases: &[(&str, &str)] = &[
            ("SELECT region r, COUNT(*) FROM t GROUP BY region", "(region)"),
            ("SELECT COUNT(*) n, c.region r FROM t c GROUP BY c.region", "(c.region)"),
            ("SELECT r = region, COUNT(*) FROM t GROUP BY region", "(region)"),
            ("SELECT 'r' = region, COUNT(*) FROM t GROUP BY region", "(region)"),
            ("SELECT CAST(x AS int) y, COUNT(*) FROM t GROUP BY CAST(x AS INT)", "(CAST(x AS int))"),
            (
                "SELECT CASE WHEN a > 1 THEN 'y' ELSE 'n' END, COUNT(*) FROM t GROUP BY CASE WHEN a > 1 THEN 'y' ELSE 'n' END",
                "(CASE WHEN a > 1 THEN 'y' ELSE 'n' END)",
            ),
            (
                "SELECT CASE WHEN a > 1 THEN 'y' END bucket, COUNT(*) FROM t GROUP BY CASE WHEN a > 1 THEN 'y' END",
                "(CASE WHEN a > 1 THEN 'y' END)",
            ),
            ("SELECT d AT TIME ZONE 'UTC', COUNT(*) FROM t GROUP BY d AT TIME ZONE 'UTC'", "(d AT TIME ZONE 'UTC')"),
            (
                "SELECT d AT TIME ZONE 'UTC' utc, COUNT(*) FROM t GROUP BY d AT TIME ZONE 'UTC'",
                "(d AT TIME ZONE 'UTC')",
            ),
            ("SELECT 1 AS [from], a AS [group], COUNT(*) FROM t GROUP BY a", "(a)"),
        ];
        for (sql, want) in cases {
            let out = drill(sql, &[text("v")]).unwrap_or_else(|e| panic!("{sql}: {e}"));
            assert!(out.ends_with(&format!("{want} = N'v'")), "{sql}\n{out}");
        }
    }

    #[test]
    fn a_query_that_cannot_be_rewritten_exactly_is_refused() {
        use Refusal::*;
        let cases: &[(&str, Refusal)] = &[
            ("SELECT a, COUNT(*) FROM t GROUP BY a UNION SELECT b, 1 FROM u GROUP BY b", SetOp),
            ("SELECT a, COUNT(*) FROM t GROUP BY a EXCEPT SELECT b, 1 FROM u", SetOp),
            ("SELECT a, COUNT(*) FROM t GROUP BY a INTERSECT SELECT b, 1 FROM u", SetOp),
            ("SELECT a, COUNT(*) FROM t GROUP BY ROLLUP (a)", GroupingSets),
            ("SELECT a, COUNT(*) FROM t GROUP BY CUBE (a)", GroupingSets),
            ("SELECT a, COUNT(*) FROM t GROUP BY GROUPING SETS ((a), ())", GroupingSets),
            ("SELECT a, COUNT(*) FROM t GROUP BY a WITH ROLLUP", GroupingSets),
            ("SELECT a, COUNT(*) FROM t GROUP BY a WITH CUBE", GroupingSets),
            ("SELECT a, COUNT(*) FROM t GROUP BY ()", GroupingSets),
            ("SELECT a, b, COUNT(*) FROM t GROUP BY (a, b)", GroupingSets),
            ("SELECT a, COUNT(*) FROM t GROUP BY ALL a", GroupByAll),
            ("SELECT a, COUNT(*) FROM t PIVOT (SUM(x) FOR m IN ([1], [2])) p GROUP BY a", Pivot),
            ("SELECT a, COUNT(*) FROM t UNPIVOT (v FOR m IN ([1], [2])) p GROUP BY a", Pivot),
            ("SELECT a, COUNT(*) INTO dbo.x FROM t GROUP BY a", Into),
            ("SELECT a, COUNT(*) FROM t GROUP BY a FOR XML PATH", For),
            ("SELECT a, COUNT(*) FROM t GROUP BY a FOR JSON AUTO", For),
            ("SELECT a, COUNT(*) FROM t GROUP BY a OPTION (MAXRECURSION 0)", Option),
            ("SELECT a, COUNT(*) FROM t GROUP BY a WINDOW w AS (ORDER BY a)", Window),
            ("SELECT a, COUNT(*) FROM t WHERE a = @a GROUP BY a", Variable),
            ("SELECT a, COUNT(*) FROM t WHERE b = @@SPID GROUP BY a", Variable),
            ("SELECT TOP (@n) a, COUNT(*) FROM t GROUP BY a", Variable),
            ("SELECT a, COUNT(*) FROM #t GROUP BY a", TempTable),
            ("SELECT a, COUNT(*) FROM [#t] GROUP BY a", TempTable),
            ("SELECT a, COUNT(*) FROM t WHERE dbo.f(a) = 1 GROUP BY a", Unvetted),
            ("SELECT dbo.f(a), COUNT(*) FROM t GROUP BY dbo.f(a)", Unvetted),
            ("SELECT a, COUNT(*) FROM tvf(1) GROUP BY a", Unvetted),
            ("SELECT a, COUNT(*) FROM t WHERE h.GetLevel() = 1 GROUP BY a", Unvetted),
            ("SELECT a, COUNT(*) FROM t WHERE geography::Point(1, 2, 4326) IS NOT NULL GROUP BY a", Unvetted),
            ("SELECT a, COUNT(*) FROM t WHERE [f](a) = 1 GROUP BY a", Unvetted),
            ("SELECT *, COUNT(*) FROM t GROUP BY a", Star),
            ("SELECT t.*, a, COUNT(*) FROM t GROUP BY a", Star),
            ("SELECT a, COUNT(*) FROM t GROUP BY a, b", NotSelected),
            ("SELECT COUNT(*) FROM t GROUP BY a", NotSelected),
            ("SELECT a AS b, COUNT(*) FROM t GROUP BY a, b", AliasKey),
            ("SELECT UPPER(region) AS region, COUNT(*) FROM t GROUP BY region", AliasKey),
            ("SELECT region = UPPER(region), COUNT(*) FROM t GROUP BY region", AliasKey),
            ("SELECT CAST(d AS date) [Day], COUNT(*) FROM t GROUP BY day", AliasKey),
            ("SELECT a, COUNT(*) FROM t GROUP BY a; SELECT 1", Statements),
            ("SELECT a, COUNT(*) FROM t GROUP BY a SELECT 1", Statements),
            ("SELECT a, COUNT(*) FROM t WHERE a = 1 DELETE FROM t", Statements),
            ("SELECT a, COUNT(*) FROM t GROUP BY a\nGO\nSELECT 1", Statements),
            ("SET NOCOUNT ON; SELECT a, COUNT(*) FROM t GROUP BY a", Statements),
            ("DECLARE @x int = 1 SELECT a, COUNT(*) FROM t GROUP BY a", Statements),
            ("IF 1 = 1 SELECT a, COUNT(*) FROM t GROUP BY a", Statements),
            ("SELECT a, COUNT(*) FROM t WHERE (a = 1 GROUP BY a", Unclosed),
            ("SELECT a, COUNT(*) FROM t WHERE a = 1) GROUP BY a", Unclosed),
            ("SELECT a, COUNT(*) FROM t WHERE a = N'open GROUP BY a", Unclosed),
            ("SELECT a, COUNT(*) FROM [t GROUP BY a", Unclosed),
            ("SELECT a, COUNT(*) FROM t GROUP BY a /* open", Unclosed),
            ("SELECT a, COUNT(*) GROUP BY a", NoFrom),
            ("SELECT 1", NoFrom),
            ("SELECT a, b FROM t", NoGroupBy),
            ("SELECT a, b FROM t WHERE a > 1 ORDER BY 1", NoGroupBy),
            ("EXEC dbo.p", NotSelect),
            ("INSERT INTO t SELECT 1", NotSelect),
            ("WITH XMLNAMESPACES ('urn:x' AS x) SELECT a, COUNT(*) FROM t GROUP BY a", NotSelect),
            ("(SELECT a, COUNT(*) FROM t GROUP BY a)", NotSelect),
            ("", NotSelect),
        ];
        for (sql, want) in cases {
            assert_eq!(plan(sql).map(|_| ()).as_ref(), Err(want), "{sql}");
        }
        assert!(plan("SELECT a, b FROM t").unwrap_err().ungrouped());
        assert!(!plan("SELECT a, COUNT(*) FROM t GROUP BY ROLLUP (a)").unwrap_err().ungrouped());
    }

    #[test]
    fn words_that_only_look_like_clauses_or_calls_are_read_in_place() {
        for sql in [
            "SELECT a, COUNT(*) FROM t FOR SYSTEM_TIME AS OF '2024-01-01' GROUP BY a",
            "SELECT a, COUNT(*) FROM t WHERE a IS DISTINCT FROM b GROUP BY a",
            "SELECT a, COUNT(*) FROM (VALUES (1), (2)) AS v(a) GROUP BY a",
            "SELECT a, COUNT(*) FROM t WHERE CAST(b AS decimal(10, 2)) > 1 AND CONVERT(nvarchar(20), c) <> N'x' GROUP BY a",
            "SELECT a, COUNT(*) FROM t WHERE EXISTS (SELECT 1 FROM u WHERE u.id IN (1, 2)) GROUP BY a",
            "SELECT a, COUNT(*), ROW_NUMBER() OVER (ORDER BY a) FROM t GROUP BY a",
            "SELECT a, COUNT(*) FROM t CROSS APPLY OPENJSON(t.doc) j GROUP BY a",
            "SELECT a, COUNT(*) FROM t WHERE LEFT(b, 1) = N'x' AND COALESCE(c, 0) > 0 GROUP BY a",
            "SELECT a, COUNT(*) FROM ##global GROUP BY a",
            "SELECT a, CASE WHEN COUNT(*) > 1 THEN 1 ELSE 0 END FROM t GROUP BY a",
            "SELECT [select], COUNT(*) FROM t GROUP BY [select]",
        ] {
            assert!(plan(sql).is_ok(), "{sql}: {:?}", plan(sql).map(|_| ()));
        }
    }
}
