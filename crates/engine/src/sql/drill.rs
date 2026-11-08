//! Rewrites a grouped PostgreSQL `SELECT` into the `SELECT *` of the rows behind one of its result
//! rows. It keeps the `WITH`, `FROM`, and `WHERE` text as written and adds one equality per
//! `GROUP BY` key, so it accepts only the queries where every key is a column of the result.

use std::ops::Range;

use super::RESERVED;
use super::lex::{LexKind, Lexeme, Strings, lex, lex_strings};
use crate::text::{quote_escaped_literal, quote_literal};

/// Why a query cannot be drilled into, in words the user reads.
pub(crate) type Refusal = &'static str;

const UNCLOSED: Refusal = "The chart's query has an unclosed quote, comment, or parenthesis.";
const STRINGS: Refusal = "The chart's query reads differently with standard_conforming_strings off.";
const PLACEHOLDER: Refusal = "The chart's query has a parameter placeholder.";
const STATEMENTS: Refusal = "The chart's query is more than one statement.";
const NOT_SELECT: Refusal = "The chart's query is not a SELECT.";
const SEARCH_CYCLE: Refusal = "The chart's query has a CTE with SEARCH or CYCLE.";
const SET_OP: Refusal = "The chart's query uses UNION, INTERSECT, or EXCEPT.";
const INTO: Refusal = "The chart's query uses SELECT INTO.";
const LOCKS: Refusal = "The chart's query locks rows with FOR UPDATE or FOR SHARE.";
const WINDOW: Refusal = "The chart's query has a WINDOW clause.";
const DISTINCT_ON: Refusal = "The chart's query uses DISTINCT ON.";
const NO_FROM: Refusal = "The chart's query has no FROM.";
const NO_GROUP_BY: Refusal = "The chart's query has no GROUP BY.";
const GROUP_QUANTIFIER: Refusal = "The chart's query uses GROUP BY ALL or GROUP BY DISTINCT.";
const GROUPING_SETS: Refusal = "The chart's query uses GROUPING SETS, ROLLUP, or CUBE.";
const STAR: Refusal = "The chart's query selects * before a grouped column.";
const UNSURE_ALIAS: Refusal = "Write AS before the alias of each grouped column.";
const ALIAS_KEY: Refusal = "A GROUP BY alias could also name a table column. Group by position instead.";
const NOT_SELECTED: Refusal = "Each GROUP BY key must also be a column of the result.";

/// The refusals of a result that has no groups at all, so it has no rows behind a point to read.
pub(crate) const UNGROUPED: [Refusal; 3] = [NOT_SELECT, NO_FROM, NO_GROUP_BY];

const COMMENT_CHARS: usize = 80;

/// Words before a trailing word that make it an operand rather than an alias.
const OPERAND_AFTER: &[&str] = &[
    "and", "or", "not", "is", "in", "like", "ilike", "similar", "to", "between", "symmetric", "asymmetric", "distinct", "from", "as",
    "case", "when", "then", "else", "escape", "collate", "zone", "overlaps", "unique",
];
/// Trailing words that end an expression instead of naming it.
const NEVER_ALIAS: &[&str] = &["end", "isnull", "notnull"];
/// A word after a string literal that may be an interval field, as in `interval '1' day`.
const INTERVAL_FIELDS: &[&str] = &["year", "month", "day", "hour", "minute", "second"];

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
/// that may be an alias or the end of the expression, as in `x::timestamp with time zone`.
enum Item {
    Star,
    /// `alias` is the index of the alias token.
    Expr { expr: Range<usize>, alias: Option<usize> },
    Unsure { with: Range<usize>, without: Range<usize> },
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
    Tail,
    Locking,
    SetOp,
}

struct Tokens<'a> {
    sql: &'a str,
    t: Vec<Lexeme>,
    depth: Vec<u32>,
}

impl Tokens<'_> {
    fn text(&self, i: usize) -> &str {
        &self.sql[self.t[i].span.clone()]
    }
    fn kind(&self, i: usize) -> Option<LexKind> {
        self.t.get(i).map(|l| l.kind)
    }
    fn word(&self, i: usize, word: &str) -> bool {
        self.kind(i) == Some(LexKind::Word) && self.text(i).eq_ignore_ascii_case(word)
    }
    fn word_in(&self, i: usize, words: &[&str]) -> bool {
        self.kind(i) == Some(LexKind::Word) && words.iter().any(|w| self.text(i).eq_ignore_ascii_case(w))
    }
    fn name(&self, i: usize) -> bool {
        matches!(self.kind(i), Some(LexKind::Word | LexKind::QuotedIdent))
    }
    /// The index of the `)` that closes the `(` at `open`.
    fn close(&self, open: usize) -> Option<usize> {
        (open + 1..self.t.len()).find(|&j| self.depth[j] == self.depth[open] && self.t[j].kind == LexKind::Close)
    }
    /// The source text from the first token to the last, comments between them included.
    fn span(&self, r: &Range<usize>) -> Range<usize> {
        self.t[r.start].span.start..self.t[r.end - 1].span.end
    }
    /// The source text between two tokens, trimmed of whitespace, comments kept.
    fn between(&self, after: usize, before: usize) -> Option<Range<usize>> {
        let start = self.t[after].span.end;
        let end = self.t.get(before).map_or(self.sql.len(), |l| l.span.start);
        let text = &self.sql[start..end];
        let lead = text.len() - text.trim_start().len();
        let inner = text.trim();
        (!inner.is_empty()).then(|| start + lead..start + lead + inner.len())
    }
    /// How PostgreSQL compares names: an unquoted word in lower case, a quoted one byte for byte.
    fn normalized(&self, r: &Range<usize>) -> Vec<String> {
        r.clone().map(|i| if self.t[i].kind == LexKind::Word { self.text(i).to_ascii_lowercase() } else { self.text(i).to_owned() }).collect()
    }
    fn folded(&self, i: usize) -> String {
        let text = self.text(i);
        match self.t[i].kind {
            LexKind::QuotedIdent => text[1..text.len() - 1].replace("\"\"", "\""),
            _ => text.to_ascii_lowercase(),
        }
    }
    /// `a`, `t.a`, or `s.t.a`, with no reserved word among them.
    fn column_ref(&self, r: &Range<usize>) -> bool {
        r.len() % 2 == 1
            && r.clone().all(|i| {
                if (i - r.start) % 2 == 1 {
                    self.t[i].kind == LexKind::Dot
                } else {
                    self.t[i].kind == LexKind::QuotedIdent || (self.t[i].kind == LexKind::Word && !RESERVED.contains(&self.text(i).to_ascii_lowercase().as_str()))
                }
            })
    }
    /// Splits `r` at the commas outside parentheses.
    fn list(&self, r: Range<usize>) -> Vec<Range<usize>> {
        let base = self.depth.get(r.start).copied().unwrap_or(0);
        let mut out = Vec::new();
        let mut start = r.start;
        for i in r.clone() {
            if self.t[i].kind == LexKind::Comma && self.depth[i] == base {
                out.push(start..i);
                start = i + 1;
            }
        }
        out.push(start..r.end);
        out
    }

    fn item(&self, r: Range<usize>) -> Item {
        use LexKind::*;
        let (a, b) = (r.start, r.end);
        let star = |i: usize| self.kind(i) == Some(Operator) && self.text(i) == "*";
        if star(b - 1) && (b - a == 1 || self.kind(b - 2) == Some(Dot)) {
            return Item::Star;
        }
        if b - a >= 3 && self.word(b - 2, "AS") && self.name(b - 1) {
            return Item::Expr { expr: a..b - 2, alias: Some(b - 1) };
        }
        let plain = Item::Expr { expr: r.clone(), alias: None };
        if b - a < 2 || !self.name(b - 1) || self.word_in(b - 1, NEVER_ALIAS) {
            return plain;
        }
        let prev = b - 2;
        let aliased = Item::Expr { expr: a..b - 1, alias: Some(b - 1) };
        let unsure = Item::Unsure { with: r.clone(), without: a..b - 1 };
        match self.t[prev].kind {
            String if self.word_in(b - 1, INTERVAL_FIELDS) => unsure,
            Close | CloseBracket | Number | String | DollarString | QuotedIdent => aliased,
            Word if self.word_in(prev, OPERAND_AFTER) => plain,
            Word if self.word_in(prev, &["null", "true", "false", "end"]) || self.column_ref(&(a..b - 1)) => aliased,
            Word => unsure,
            _ => plain,
        }
    }

    fn clause(&self, i: usize) -> Option<Clause> {
        if self.kind(i) != Some(LexKind::Word) || (i > 0 && self.word(i - 1, "AS")) {
            return None;
        }
        let w = self.text(i).to_ascii_lowercase();
        Some(match w.as_str() {
            "from" if i > 0 && (self.word(i - 1, "DISTINCT") || self.word(i - 1, "ROWS")) => return None,
            "from" => Clause::From,
            "into" => Clause::Into,
            "where" => Clause::Where,
            "group" if self.word(i + 1, "BY") => Clause::GroupBy,
            "having" => Clause::Having,
            "window" => Clause::Window,
            "order" if self.word(i + 1, "BY") => Clause::OrderBy,
            "limit" | "offset" | "fetch" => Clause::Tail,
            "for" => Clause::Locking,
            "union" | "intersect" | "except" => Clause::SetOp,
            _ => return None,
        })
    }

    /// The index of the main `SELECT` after `WITH [RECURSIVE] name [(cols)] AS [[NOT] MATERIALIZED] (…), …`.
    fn after_ctes(&self, mut i: usize) -> Result<usize, Refusal> {
        if self.word(i, "RECURSIVE") {
            i += 1;
        }
        loop {
            if !self.name(i) {
                return Err(NOT_SELECT);
            }
            i += 1;
            if self.kind(i) == Some(LexKind::Open) {
                i = self.close(i).ok_or(NOT_SELECT)? + 1;
            }
            if !self.word(i, "AS") {
                return Err(NOT_SELECT);
            }
            i += 1;
            if self.word(i, "NOT") {
                i += 1;
            }
            if self.word(i, "MATERIALIZED") {
                i += 1;
            }
            if self.kind(i) != Some(LexKind::Open) {
                return Err(NOT_SELECT);
            }
            i = self.close(i).ok_or(NOT_SELECT)? + 1;
            if self.word(i, "SEARCH") || self.word(i, "CYCLE") {
                return Err(SEARCH_CYCLE);
            }
            if self.kind(i) != Some(LexKind::Comma) {
                break;
            }
            i += 1;
        }
        if self.word(i, "SELECT") { Ok(i) } else { Err(NOT_SELECT) }
    }
}

/// Lexing `sql` and then a statement end finds the end only when every lexeme in `sql` closes.
fn closed(sql: &str) -> bool {
    lex(&format!("{sql}\n;")).last().is_some_and(|l| l.kind == LexKind::Semicolon && l.span.start == sql.len() + 1)
}

fn tokens(sql: &str) -> Result<Tokens<'_>, Refusal> {
    let lexemes = lex(sql);
    if sql.contains('\\') && lexemes != lex_strings(sql, Strings::Escaped) {
        return Err(STRINGS);
    }
    if !closed(sql) {
        return Err(UNCLOSED);
    }
    let mut t: Vec<Lexeme> = lexemes.into_iter().filter(|l| !matches!(l.kind, LexKind::Space | LexKind::LineComment | LexKind::BlockComment)).collect();
    if t.last().is_some_and(|l| l.kind == LexKind::Semicolon) {
        t.pop();
    }
    let mut depth = Vec::with_capacity(t.len());
    let mut d = 0u32;
    // An `ARRAY[…]` constructor holds expressions; any other `[…]` is a subscript, where `lo:hi` is a slice.
    let mut brackets: Vec<bool> = Vec::new();
    for (i, l) in t.iter().enumerate() {
        match l.kind {
            LexKind::Close | LexKind::CloseBracket => d = d.checked_sub(1).ok_or(UNCLOSED)?,
            LexKind::Semicolon => return Err(STATEMENTS),
            LexKind::Param => return Err(PLACEHOLDER),
            LexKind::Other if &sql[l.span.clone()] == ":" && brackets.iter().all(|constructor| *constructor) => {
                if t.get(i + 1).is_some_and(|next| next.kind == LexKind::Word && next.span.start == l.span.end) {
                    return Err(PLACEHOLDER);
                }
            }
            _ => {}
        }
        match l.kind {
            LexKind::OpenBracket => brackets.push(i > 0 && t[i - 1].kind == LexKind::Word && sql[t[i - 1].span.clone()].eq_ignore_ascii_case("array")),
            LexKind::CloseBracket => {
                brackets.pop();
            }
            _ => {}
        }
        depth.push(d);
        if matches!(l.kind, LexKind::Open | LexKind::OpenBracket) {
            d += 1;
        }
    }
    if d != 0 {
        return Err(UNCLOSED);
    }
    Ok(Tokens { sql, t, depth })
}

pub(crate) fn plan(sql: &str) -> Result<Drill<'_>, Refusal> {
    let tk = tokens(sql)?;
    let len = tk.t.len();
    let main = match () {
        _ if tk.word(0, "SELECT") => 0,
        _ if tk.word(0, "WITH") => tk.after_ctes(1)?,
        _ => return Err(NOT_SELECT),
    };
    if (0..len).any(|i| tk.depth[i] == 0 && tk.clause(i) == Some(Clause::SetOp)) {
        return Err(SET_OP);
    }
    let clauses: Vec<(Clause, usize)> = (main + 1..len).filter(|&i| tk.depth[i] == 0).filter_map(|i| tk.clause(i).map(|c| (c, i))).collect();
    for (clause, refusal) in [(Clause::Into, INTO), (Clause::Locking, LOCKS), (Clause::Window, WINDOW)] {
        if clauses.iter().any(|(c, _)| *c == clause) {
            return Err(refusal);
        }
    }
    let mut items_start = main + 1;
    if tk.word(items_start, "DISTINCT") {
        if tk.word(items_start + 1, "ON") {
            return Err(DISTINCT_ON);
        }
        items_start += 1;
    } else if tk.word(items_start, "ALL") {
        items_start += 1;
    }
    let mut next = clauses.iter().copied().peekable();
    let Some((Clause::From, from_at)) = next.next() else { return Err(NO_FROM) };
    let end = |n: Option<&(Clause, usize)>| n.map_or(len, |(_, i)| *i);
    let from_end = end(next.peek());
    let from = tk.between(from_at, from_end).ok_or(NO_FROM)?;
    let filter = match next.next_if(|(c, _)| *c == Clause::Where) {
        Some((_, at)) => Some(tk.between(at, end(next.peek())).ok_or(NOT_SELECT)?),
        None => None,
    };
    let Some((Clause::GroupBy, group_at)) = next.next() else { return Err(NO_GROUP_BY) };
    let keys_start = group_at + 2;
    let keys_end = end(next.peek());
    if tk.word_in(keys_start, &["ALL", "DISTINCT"]) {
        return Err(GROUP_QUANTIFIER);
    }
    if keys_start >= keys_end {
        return Err(NO_GROUP_BY);
    }

    let items: Vec<Item> = tk.list(items_start..from_at).into_iter().map(|r| if r.is_empty() { Item::Expr { expr: r, alias: None } } else { tk.item(r) }).collect();
    let mut keys: Vec<Key> = Vec::new();
    for key in tk.list(keys_start..keys_end) {
        let (column, expr) = match_key(&tk, &items, key)?;
        if items[..=column].iter().any(|i| matches!(i, Item::Star)) {
            return Err(STAR);
        }
        let column = u16::try_from(column).map_err(|_| NOT_SELECTED)?;
        if !keys.iter().any(|k| k.column == column) {
            keys.push(Key { column, expr: tk.span(&expr) });
        }
    }
    Ok(Drill { sql, with: (main > 0).then(|| tk.t[0].span.start..tk.t[main - 1].span.end), from, filter, keys })
}

/// The select item a `GROUP BY` key names, and the expression to compare: the same expression, the
/// item at a 1-based position, or a column the key names bare.
fn match_key(tk: &Tokens<'_>, items: &[Item], key: Range<usize>) -> Result<(usize, Range<usize>), Refusal> {
    if key.is_empty() || tk.word_in(key.start, &["ROLLUP", "CUBE"]) || (tk.word(key.start, "GROUPING") && tk.word(key.start + 1, "SETS")) {
        return Err(GROUPING_SETS);
    }
    let first = key.start;
    if tk.kind(first) == Some(LexKind::Open) && tk.close(first) == Some(key.end - 1) && (key.len() == 2 || tk.list(first + 1..key.end - 1).len() > 1) {
        return Err(GROUPING_SETS);
    }
    if key.len() == 1 && tk.kind(first) == Some(LexKind::Number) && tk.text(first).bytes().all(|b| b.is_ascii_digit()) {
        let n: usize = tk.text(first).parse().map_err(|_| NOT_SELECTED)?;
        return match n.checked_sub(1).and_then(|i| items.get(i).map(|item| (i, item))) {
            Some((i, Item::Expr { expr, .. })) if !expr.is_empty() => Ok((i, expr.clone())),
            Some((_, Item::Unsure { .. })) => Err(UNSURE_ALIAS),
            Some((_, Item::Star)) => Err(STAR),
            _ => Err(NOT_SELECTED),
        };
    }
    let wanted = tk.normalized(&key);
    for (i, item) in items.iter().enumerate() {
        let readings: &[&Range<usize>] = match item {
            Item::Star => &[],
            Item::Expr { expr, .. } => &[expr],
            Item::Unsure { with, without } => &[with, without],
        };
        if let Some(expr) = readings.iter().find(|r| !r.is_empty() && tk.normalized(r) == wanted) {
            return Ok((i, (*expr).clone()));
        }
    }
    if key.len() == 1 && tk.name(first) {
        let name = tk.folded(first);
        // A bare GROUP BY name means a FROM column before an output alias. An item that is a column of
        // that name means the same either way; for any other item the name may mean a table column.
        for (i, item) in items.iter().enumerate() {
            if let Item::Expr { expr, .. } = item
                && tk.column_ref(expr)
                && tk.folded(expr.end - 1) == name
            {
                return Ok((i, expr.clone()));
            }
        }
        let alias = |item: &Item| match item {
            Item::Expr { alias: Some(a), .. } => tk.folded(*a) == name,
            Item::Unsure { with, .. } => tk.folded(with.end - 1) == name,
            _ => false,
        };
        if items.iter().any(alias) {
            return Err(ALIAS_KEY);
        }
    }
    if items.iter().any(|i| matches!(i, Item::Star)) { Err(STAR) } else { Err(NOT_SELECTED) }
}

impl Drill<'_> {
    /// The result columns whose values pick the rows, one per distinct `GROUP BY` key.
    pub(crate) fn columns(&self) -> impl Iterator<Item = u16> + '_ {
        self.keys.iter().map(|k| k.column)
    }

    /// The rows behind the result row whose key columns hold `values`, in the order of `columns`.
    pub(crate) fn sql(&self, values: &[Option<String>], title: &str) -> String {
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
            let newline = if lex(text).last().is_some_and(|l| l.kind == LexKind::LineComment) { "\n" } else { "" };
            out.push_str(&format!("\nWHERE ({text}{newline})"));
            joiner = "\n  AND ";
        }
        for (key, value) in self.keys.iter().zip(values) {
            let expr = &self.sql[key.expr.clone()];
            let test = match value {
                None => "IS NULL".to_owned(),
                Some(v) if v.contains('\\') => format!("= {}", quote_escaped_literal(v)),
                Some(v) => format!("= {}", quote_literal(v)),
            };
            out.push_str(&format!("{joiner}({expr}) {test}"));
            joiner = "\n  AND ";
        }
        out
    }
}

/// Text that cannot end the comment it goes in: control characters and line breaks become spaces.
fn one_line(text: &str) -> String {
    let words: Vec<&str> = text.split(|c: char| c.is_whitespace() || c.is_control()).filter(|w| !w.is_empty()).collect();
    let line = words.join(" ");
    match line.char_indices().nth(COMMENT_CHARS) {
        Some((cut, _)) => format!("{}…", &line[..cut]),
        None => line,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drill(sql: &str, values: &[Option<&str>]) -> Result<String, Refusal> {
        let plan = plan(sql)?;
        assert_eq!(plan.columns().count(), values.len(), "one value per key in {sql}");
        Ok(plan.sql(&values.iter().map(|v| v.map(str::to_owned)).collect::<Vec<_>>(), "Untitled-1.sql"))
    }

    fn columns(sql: &str) -> Vec<u16> {
        plan(sql).unwrap().columns().collect()
    }

    #[test]
    fn a_grouped_select_becomes_the_rows_of_its_group() {
        let cases: &[(&str, &[Option<&str>], &str)] = &[
            (
                "SELECT c.region, count(*) FROM orders o JOIN customers c ON c.id = o.customer_id GROUP BY c.region",
                &[Some("north")],
                "-- Rows behind north in Untitled-1.sql\nSELECT *\nFROM orders o JOIN customers c ON c.id = o.customer_id\nWHERE (c.region) = 'north'",
            ),
            (
                "select c.region, count(*) as n from orders o join customers c on c.id = o.customer_id where o.status = 'paid' group by 1 order by 2 desc",
                &[Some("EU-West")],
                "-- Rows behind EU-West in Untitled-1.sql\nSELECT *\nFROM orders o join customers c on c.id = o.customer_id\nWHERE (o.status = 'paid')\n  AND (c.region) = 'EU-West'",
            ),
            (
                "SELECT c.region AS region, sum(o.total) FROM orders o JOIN customers c ON c.id = o.customer_id GROUP BY region",
                &[Some("south")],
                "-- Rows behind south in Untitled-1.sql\nSELECT *\nFROM orders o JOIN customers c ON c.id = o.customer_id\nWHERE (c.region) = 'south'",
            ),
            (
                "SELECT date_trunc('day', o.created_at) AS day, count(*) FROM orders o GROUP BY 1",
                &[Some("2026-03-01 00:00:00+00")],
                "-- Rows behind 2026-03-01 00:00:00+00 in Untitled-1.sql\nSELECT *\nFROM orders o\nWHERE (date_trunc('day', o.created_at)) = '2026-03-01 00:00:00+00'",
            ),
            (
                "SELECT status, count(*) FROM orders WHERE total > 10 GROUP BY status HAVING count(*) > 5 ORDER BY 2 DESC LIMIT 3 OFFSET 1",
                &[Some("paid")],
                "-- Rows behind paid in Untitled-1.sql\nSELECT *\nFROM orders\nWHERE (total > 10)\n  AND (status) = 'paid'",
            ),
            (
                "SELECT status, count(*) FROM orders GROUP BY status FETCH FIRST 2 ROWS ONLY;",
                &[None],
                "-- Rows behind NULL in Untitled-1.sql\nSELECT *\nFROM orders\nWHERE (status) IS NULL",
            ),
        ];
        for (sql, values, want) in cases {
            assert_eq!(drill(sql, values).as_deref(), Ok(*want), "{sql}");
        }
    }

    #[test]
    fn a_with_prefix_and_comments_stay_as_written() {
        let sql = "-- revenue by month\nWITH RECURSIVE paid AS (SELECT * FROM orders WHERE status = 'paid' /* GROUP BY 1 */), x(n) AS MATERIALIZED (SELECT 1)\n\
                   SELECT date_trunc('month', p.created_at) AS month, /* the total */ sum(p.total)\n\
                   FROM paid p -- only paid\n\
                   WHERE p.note <> 'FROM t GROUP BY 1' -- a string that looks like SQL\n\
                   GROUP BY 1";
        assert_eq!(
            drill(sql, &[Some("2026-01-01 00:00:00+00")]).unwrap(),
            "-- Rows behind 2026-01-01 00:00:00+00 in Untitled-1.sql\n\
             WITH RECURSIVE paid AS (SELECT * FROM orders WHERE status = 'paid' /* GROUP BY 1 */), x(n) AS MATERIALIZED (SELECT 1)\n\
             SELECT *\n\
             FROM paid p -- only paid\n\
             WHERE (p.note <> 'FROM t GROUP BY 1' -- a string that looks like SQL\n)\n  \
             AND (date_trunc('month', p.created_at)) = '2026-01-01 00:00:00+00'"
        );
    }

    #[test]
    fn every_group_key_picks_the_rows() {
        let sql = "SELECT a, b, count(*) FROM t GROUP BY a, b";
        assert_eq!(columns(sql), [0, 1]);
        assert_eq!(drill(sql, &[Some("x"), None]).unwrap(), "-- Rows behind x, NULL in Untitled-1.sql\nSELECT *\nFROM t\nWHERE (a) = 'x'\n  AND (b) IS NULL");
        assert_eq!(columns("SELECT count(*), b, a FROM t GROUP BY a, b, a"), [2, 1], "a repeated key compares once");
        assert_eq!(columns("SELECT sum(x) total, upper(name) up FROM t GROUP BY upper(NAME)"), [1], "words compare in lower case");
        assert_eq!(columns("SELECT t.\"Region\" r, count(*) FROM t GROUP BY \"Region\""), [0]);
    }

    #[test]
    fn a_label_is_quoted_the_same_under_any_string_setting() {
        let sql = "SELECT name, count(*) FROM t GROUP BY 1";
        assert!(drill(sql, &[Some("O'Brien")]).unwrap().ends_with("WHERE (name) = 'O''Brien'"));
        assert!(drill(sql, &[Some(r"C:\dir's")]).unwrap().ends_with(r"WHERE (name) = E'C:\\dir''s'"));
    }

    #[test]
    fn the_comment_line_cannot_be_broken_out_of() {
        let sql = "SELECT name, count(*) FROM t GROUP BY 1";
        let out = drill(sql, &[Some("a\nSELECT 1; --\r\u{2028}b")]).unwrap();
        assert_eq!(out.lines().next(), Some("-- Rows behind a SELECT 1; -- b in Untitled-1.sql"));
        let long = "x".repeat(200);
        let line = drill(sql, &[Some(&long)]).unwrap().lines().next().unwrap().to_owned();
        assert_eq!(line, format!("-- Rows behind {}… in Untitled-1.sql", "x".repeat(COMMENT_CHARS)));
    }

    #[test]
    fn an_item_alias_is_read_only_where_it_is_certain() {
        let cases: &[(&str, &str)] = &[
            ("SELECT region r, count(*) FROM t GROUP BY 1", "(region)"),
            ("SELECT count(*) n, c.region r FROM t GROUP BY 2", "(c.region)"),
            ("SELECT x::int y, count(*) FROM t GROUP BY x::int", "(x::int)"),
            ("SELECT x::timestamp with time zone, count(*) FROM t GROUP BY x::timestamp with time zone", "(x::timestamp with time zone)"),
            ("SELECT CASE WHEN a THEN 'y' ELSE 'n' END, count(*) FROM t GROUP BY 1", "(CASE WHEN a THEN 'y' ELSE 'n' END)"),
            ("SELECT CASE WHEN a THEN 'y' END bucket, count(*) FROM t GROUP BY 1", "(CASE WHEN a THEN 'y' END)"),
            ("SELECT a IS NULL, count(*) FROM t GROUP BY 1", "(a IS NULL)"),
            ("SELECT a IS NULL missing, count(*) FROM t GROUP BY 1", "(a IS NULL)"),
            ("SELECT a ISNULL, count(*) FROM t GROUP BY 1", "(a ISNULL)"),
            ("SELECT NOT a, count(*) FROM t GROUP BY 1", "(NOT a)"),
            ("SELECT a COLLATE \"C\", count(*) FROM t GROUP BY 1", "(a COLLATE \"C\")"),
            ("SELECT 1 AS from, a AS \"group\", count(*) FROM t GROUP BY 2", "(a)"),
            ("SELECT a IS DISTINCT FROM b, count(*) FROM t GROUP BY 1", "(a IS DISTINCT FROM b)"),
        ];
        for (sql, want) in cases {
            let out = drill(sql, &[Some("v")]).unwrap_or_else(|e| panic!("{sql}: {e}"));
            assert!(out.ends_with(&format!("{want} = 'v'")), "{sql}\n{out}");
        }
        for sql in ["SELECT x::double precision, count(*) FROM t GROUP BY 1", "SELECT interval '1' day, count(*) FROM t GROUP BY 1"] {
            assert_eq!(plan(sql).map(|_| ()), Err(UNSURE_ALIAS), "{sql}");
        }
        assert_eq!(columns("SELECT interval '1' day, count(*) FROM t GROUP BY interval '1' day"), [0], "the key itself settles which reading it is");
    }

    #[test]
    fn a_query_that_cannot_be_rewritten_exactly_is_refused() {
        let cases: &[(&str, Refusal)] = &[
            ("SELECT a, count(*) FROM t GROUP BY a UNION SELECT b, 1 FROM u GROUP BY b", SET_OP),
            ("WITH x AS (SELECT 1) (SELECT a FROM t GROUP BY a) EXCEPT SELECT 1", NOT_SELECT),
            ("SELECT a, count(*) GROUP BY a", NO_FROM),
            ("SELECT 1", NO_FROM),
            ("SELECT a, b FROM t", NO_GROUP_BY),
            ("SELECT a, b FROM t WHERE a > 1 ORDER BY 1", NO_GROUP_BY),
            ("SELECT a, count(*) FROM t GROUP BY ROLLUP (a)", GROUPING_SETS),
            ("SELECT a, count(*) FROM t GROUP BY CUBE (a)", GROUPING_SETS),
            ("SELECT a, count(*) FROM t GROUP BY GROUPING SETS ((a), ())", GROUPING_SETS),
            ("SELECT a, count(*) FROM t GROUP BY ()", GROUPING_SETS),
            ("SELECT a, b, count(*) FROM t GROUP BY (a, b)", GROUPING_SETS),
            ("SELECT DISTINCT ON (a) a, count(*) FROM t GROUP BY a", DISTINCT_ON),
            ("SELECT a, count(*) INTO x FROM t GROUP BY a", INTO),
            ("SELECT a, count(*) FROM t GROUP BY a FOR UPDATE", LOCKS),
            ("SELECT a, count(*) FROM t GROUP BY a WINDOW w AS (ORDER BY a)", WINDOW),
            ("SELECT a, count(*) FROM t GROUP BY ALL a", GROUP_QUANTIFIER),
            ("SELECT a, count(*) FROM t GROUP BY DISTINCT a", GROUP_QUANTIFIER),
            ("SELECT a, count(*) FROM t WHERE a = $1 GROUP BY a", PLACEHOLDER),
            ("SELECT a, count(*) FROM t WHERE a = :name GROUP BY a", PLACEHOLDER),
            ("SELECT a, count(*) FROM t WHERE b = ARRAY[:x] GROUP BY a", PLACEHOLDER),
            ("SELECT *, count(*) FROM t GROUP BY a", STAR),
            ("SELECT t.*, a, count(*) FROM t GROUP BY a", STAR),
            ("SELECT *, count(*) FROM t GROUP BY 1", STAR),
            ("SELECT a, count(*) FROM t GROUP BY 3", NOT_SELECTED),
            ("SELECT a, count(*) FROM t GROUP BY a, b", NOT_SELECTED),
            ("SELECT a AS b, count(*) FROM t GROUP BY a, b", ALIAS_KEY),
            ("SELECT count(*), sum(x) FROM t GROUP BY a + 1", NOT_SELECTED),
            ("SELECT upper(region) AS region, count(*) FROM t GROUP BY region", ALIAS_KEY),
            ("SELECT date_trunc('day', created_at) day, count(*) FROM t GROUP BY day", ALIAS_KEY),
            ("SELECT a, count(*) FROM t GROUP BY a; SELECT 1", STATEMENTS),
            ("SELECT a, count(*) FROM t WHERE (a = 1 GROUP BY a", UNCLOSED),
            ("SELECT a, count(*) FROM t WHERE a = 1) GROUP BY a", UNCLOSED),
            ("SELECT a, count(*) FROM t WHERE a = 'open GROUP BY a", UNCLOSED),
            ("SELECT a, count(*) FROM t GROUP BY a /* open", UNCLOSED),
            ("SELECT a, count(*) FROM t WHERE a = 'it\\' GROUP BY a --'", STRINGS),
            ("WITH RECURSIVE r(n) AS (SELECT 1) SEARCH DEPTH FIRST BY n SET o SELECT n, count(*) FROM r GROUP BY n", SEARCH_CYCLE),
            ("VALUES (1)", NOT_SELECT),
            ("(SELECT a, count(*) FROM t GROUP BY a)", NOT_SELECT),
            ("", NOT_SELECT),
        ];
        for (sql, want) in cases {
            assert_eq!(plan(sql).map(|_| ()), Err(*want), "{sql}");
        }
    }

    #[test]
    fn the_keys_come_from_group_by_whatever_column_the_chart_labels_with() {
        assert_eq!(plan("SELECT count(*) FROM orders GROUP BY status").map(|_| ()), Err(NOT_SELECTED), "only the aggregate is selected");
        assert_eq!(columns("SELECT count(*) AS n, sum(total), status FROM orders GROUP BY status"), [2]);
    }

    #[test]
    fn subscripts_slices_and_casts_are_not_placeholders() {
        for sql in [
            "SELECT a[1:2], count(*) FROM t GROUP BY 1",
            "SELECT a[lo:hi], count(*) FROM t GROUP BY 1",
            "SELECT a::text, count(*) FROM t WHERE b = ':x' GROUP BY 1",
            "SELECT a, count(*) FROM t WHERE b = $$:x$$ AND \"$1\" = 1 GROUP BY 1",
        ] {
            assert!(plan(sql).is_ok(), "{sql}");
        }
    }
}
