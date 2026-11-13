use super::lex::{is_ident_cont, is_ident_start, run};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StmtSpan {
    pub start: usize,
    pub end: usize,
}

impl StmtSpan {
    pub fn text<'a>(&self, sql: &'a str) -> &'a str {
        &sql[self.start..self.end]
    }
}

#[derive(Debug, Clone, Copy)]
pub enum ByteTarget {
    Cursor(usize),
    Selection { from: usize, to: usize },
    All,
}

/// Words that open a statement. A blank line before one of them ends the statement before it.
const STARTERS: &[&str] = &[
    "SELECT",
    "WITH",
    "INSERT",
    "UPDATE",
    "DELETE",
    "MERGE",
    "VALUES",
    "TABLE",
    "CREATE",
    "ALTER",
    "DROP",
    "TRUNCATE",
    "GRANT",
    "REVOKE",
    "COMMENT",
    "BEGIN",
    "START",
    "COMMIT",
    "ROLLBACK",
    "ABORT",
    "SAVEPOINT",
    "RELEASE",
    "SET",
    "RESET",
    "SHOW",
    "EXPLAIN",
    "ANALYZE",
    "VACUUM",
    "COPY",
    "CALL",
    "DO",
    "LOCK",
    "REFRESH",
    "REINDEX",
    "CLUSTER",
    "DECLARE",
    "FETCH",
    "MOVE",
    "CLOSE",
    "PREPARE",
    "EXECUTE",
    "DEALLOCATE",
    "LISTEN",
    "NOTIFY",
    "UNLISTEN",
    "DISCARD",
    "CHECKPOINT",
    "LOAD",
    "SECURITY",
    "IMPORT",
    "REASSIGN",
];

/// Words after which a statement continues on the next non-blank line.
const CONTINUATIONS: &[&str] = &[
    "AS",
    "UNION",
    "INTERSECT",
    "EXCEPT",
    "ALL",
    "DISTINCT",
    "AND",
    "OR",
    "NOT",
    "ON",
    "FOR",
    "DO",
    "INSTEAD",
    "ALSO",
    "THEN",
    "ELSE",
    "WHEN",
    "IN",
    "EXISTS",
    "FROM",
    "JOIN",
    "WHERE",
    "BY",
    "INTO",
    "SET",
    "USING",
    "TO",
    "RETURNING",
    "SELECT",
    "VALUES",
    "EXPLAIN",
    "ANALYZE",
    "VERBOSE",
];

/// The keyword that completes a `WITH`, `INSERT`, or `EXPLAIN` prefix.
const BODIES: &[&str] = &["SELECT", "VALUES", "INSERT", "UPDATE", "DELETE", "MERGE", "TABLE", "DEFAULT"];

fn is_word(list: &[&str], word: &str) -> bool {
    list.iter().any(|w| w.eq_ignore_ascii_case(word))
}

#[derive(Clone, Copy)]
enum Last<'a> {
    Nothing,
    Word(&'a str),
    Punct(u8),
}

/// What the lexer knows about the statement it is inside.
struct Stmt<'a> {
    first: Option<&'a str>,
    /// False while a `WITH`, `INSERT`, or `EXPLAIN` waits for its body, or an `UPDATE` for its `SET`.
    complete: bool,
    parens: usize,
    body_depth: usize,
    last: Last<'a>,
}

impl<'a> Stmt<'a> {
    fn new() -> Self {
        Stmt { first: None, complete: false, parens: 0, body_depth: 0, last: Last::Nothing }
    }

    fn word(&mut self, word: &'a str) {
        match self.first {
            None => {
                self.first = Some(word);
                self.complete = !is_word(&["WITH", "INSERT", "EXPLAIN", "UPDATE"], word);
            }
            Some(first) if self.parens == 0 && !self.complete => {
                self.complete = if first.eq_ignore_ascii_case("UPDATE") {
                    word.eq_ignore_ascii_case("SET")
                } else {
                    is_word(BODIES, word)
                };
            }
            Some(_) => {}
        }
        let begin_atomic = word.eq_ignore_ascii_case("ATOMIC")
            && matches!(self.last, Last::Word(w) if w.eq_ignore_ascii_case("BEGIN"));
        if begin_atomic || self.body_depth > 0 && word.eq_ignore_ascii_case("CASE") {
            self.body_depth += 1;
        } else if self.body_depth > 0 && word.eq_ignore_ascii_case("END") {
            self.body_depth -= 1;
        }
        self.last = Last::Word(word);
    }

    fn punct(&mut self, c: u8) {
        match c {
            b'(' => self.parens += 1,
            b')' => self.parens = self.parens.saturating_sub(1),
            _ => {}
        }
        self.last = Last::Punct(c);
    }

    /// Whether a blank line here may end the statement, given the word that follows the blank line.
    fn ends_before(&self, next: &str) -> bool {
        if !is_word(STARTERS, next) {
            return false;
        }
        let ended = match self.last {
            Last::Nothing => return true, // only whitespace or comments so far, which `push_trimmed` drops
            Last::Word(w) => !is_word(CONTINUATIONS, w),
            Last::Punct(c) => !matches!(
                c,
                b',' | b'('
                    | b'.'
                    | b'+'
                    | b'-'
                    | b'*'
                    | b'/'
                    | b'<'
                    | b'>'
                    | b'='
                    | b'~'
                    | b'!'
                    | b'@'
                    | b'#'
                    | b'%'
                    | b'^'
                    | b'&'
                    | b'|'
                    | b'?'
            ),
        };
        ended && self.complete && self.parens == 0 && self.body_depth == 0
    }
}

/// Splits on top-level `;`, and on a blank line when the text before it is a complete statement and the next word starts a new one.
pub fn split_statements(sql: &str) -> Vec<StmtSpan> {
    let bytes = sql.as_bytes();
    let mut spans = Vec::new();
    let mut stmt_start = 0;
    let mut stmt = Stmt::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        match c {
            b'-' if bytes.get(i + 1) == Some(&b'-') => i = line_comment_end(bytes, i),
            b'/' if bytes.get(i + 1) == Some(&b'*') => i = skip_block_comment(bytes, i),
            b'\'' => {
                let escapes = i > 0 && matches!(bytes[i - 1], b'E' | b'e') && (i < 2 || !is_ident_cont(bytes[i - 2]));
                i = skip_string(bytes, i, escapes);
            }
            b'"' => i = skip_quoted(bytes, i, b'"', false),
            b'$' => match dollar_tag(bytes, i) {
                Some(tag_len) => i = skip_dollar_quoted(bytes, i, tag_len),
                None => i += 1,
            },
            b'{' => {
                stmt.punct(c);
                i = super::refs::end_at(sql, i).unwrap_or(i + 1);
            }
            b';' if stmt.body_depth == 0 => {
                push_trimmed(&mut spans, sql, stmt_start, i);
                stmt_start = i + 1;
                stmt = Stmt::new();
                i += 1;
            }
            b'\n' => match after_blank_lines(bytes, i) {
                Some(next) => {
                    if stmt.ends_before(first_word(sql, next)) {
                        push_trimmed(&mut spans, sql, stmt_start, i);
                        stmt_start = next;
                        stmt = Stmt::new();
                    }
                    i = next;
                }
                None => i += 1,
            },
            c if is_ident_start(c) => {
                let end = run(bytes, i + 1, is_ident_cont);
                stmt.word(&sql[i..end]);
                i = end;
            }
            c if c.is_ascii_whitespace() => i += 1,
            c => {
                stmt.punct(c);
                i += 1;
            }
        }
    }
    push_trimmed(&mut spans, sql, stmt_start, bytes.len());
    spans
}

/// With a newline at `i`, the start of the next non-blank line when at least one blank line follows.
fn after_blank_lines(bytes: &[u8], i: usize) -> Option<usize> {
    let mut j = i + 1;
    let mut blank = false;
    loop {
        while j < bytes.len() && bytes[j] != b'\n' && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if j < bytes.len() && bytes[j] == b'\n' {
            blank = true;
            j += 1;
        } else {
            return blank.then_some(j);
        }
    }
}

/// The first word at or after `i`, past whitespace and comments. Empty when none follows.
fn first_word(sql: &str, mut i: usize) -> &str {
    let bytes = sql.as_bytes();
    while i < bytes.len() {
        match bytes[i] {
            b'-' if bytes.get(i + 1) == Some(&b'-') => i = line_comment_end(bytes, i),
            b'/' if bytes.get(i + 1) == Some(&b'*') => i = skip_block_comment(bytes, i),
            c if c.is_ascii_whitespace() => i += 1,
            c if is_ident_start(c) => return &sql[i..run(bytes, i + 1, is_ident_cont)],
            _ => return "",
        }
    }
    ""
}

pub fn pick(spans: &[StmtSpan], sql: &str, target: ByteTarget) -> Vec<StmtSpan> {
    match target {
        ByteTarget::All => spans.to_vec(),
        ByteTarget::Selection { from, to } if from < to => split_statements(&sql[from..to])
            .into_iter()
            .map(|s| StmtSpan { start: s.start + from, end: s.end + from })
            .collect(),
        ByteTarget::Selection { from, .. } => pick(spans, sql, ByteTarget::Cursor(from)),
        // A cursor between statements picks one only on a line that statement shares; a blank line picks nothing.
        ByteTarget::Cursor(off) => {
            let line_start = sql[..off].rfind('\n').map_or(0, |n| n + 1);
            let line_end = sql[off..].find('\n').map_or(sql.len(), |n| off + n);
            let on_line = |s: &&StmtSpan| s.start <= line_end && terminator_end(sql, s) >= line_start;
            let under = spans.iter().find(|s| s.start <= off && off <= terminator_end(sql, s));
            let previous = || spans.iter().rev().filter(on_line).find(|s| s.end <= off);
            let next = || spans.iter().filter(on_line).find(|s| s.start > off);
            under.or_else(previous).or_else(next).into_iter().copied().collect()
        }
    }
}

fn terminator_end(sql: &str, span: &StmtSpan) -> usize {
    let rest = &sql[span.end..];
    let ws = rest.len() - rest.trim_start().len();
    if rest[ws..].starts_with(';') { span.end + ws + 1 } else { span.end }
}

fn push_trimmed(spans: &mut Vec<StmtSpan>, sql: &str, start: usize, end: usize) {
    let text = &sql[start..end];
    let leading = text.len() - text.trim_start().len();
    let trimmed = text.trim();
    if trimmed.is_empty() || is_only_comments(trimmed) {
        return;
    }
    spans.push(StmtSpan { start: start + leading, end: start + leading + trimmed.len() });
}

fn is_only_comments(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'-' if bytes.get(i + 1) == Some(&b'-') => i = line_comment_end(bytes, i),
            b'/' if bytes.get(i + 1) == Some(&b'*') => i = skip_block_comment(bytes, i),
            c if c.is_ascii_whitespace() => i += 1,
            _ => return false,
        }
    }
    true
}

pub(super) fn skip_block_comment(bytes: &[u8], mut i: usize) -> usize {
    let mut depth = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*') {
            depth += 1;
            i += 2;
        } else if bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/') {
            depth -= 1;
            i += 2;
            if depth == 0 {
                return i;
            }
        } else {
            i += 1;
        }
    }
    bytes.len()
}

/// The end of the string literal whose opening quote is at `i`, past every part Postgres joins to it.
/// `escapes` is true for an `E` literal, whose backslash escapes hold in every part.
pub(super) fn skip_string(bytes: &[u8], i: usize, escapes: bool) -> usize {
    let mut end = skip_quoted(bytes, i, b'\'', escapes);
    while let Some(next) = continued_at(bytes, end) {
        end = skip_quoted(bytes, next, b'\'', escapes);
    }
    end
}

/// The quote of the part that continues a literal ending at `i`. `scan.l`'s `quotecontinue` allows
/// whitespace and `--` comments between the parts, and requires a newline among them.
fn continued_at(bytes: &[u8], mut i: usize) -> Option<usize> {
    let mut newline = false;
    loop {
        match *bytes.get(i)? {
            b'\'' => return newline.then_some(i),
            b'-' if bytes.get(i + 1) == Some(&b'-') => i = line_comment_end(bytes, i),
            c if c.is_ascii_whitespace() => {
                newline |= matches!(c, b'\n' | b'\r');
                i += 1;
            }
            _ => return None,
        }
    }
}

/// The end of the `--` comment at `i`, which is the next `\n` or `\r`.
pub(super) fn line_comment_end(bytes: &[u8], i: usize) -> usize {
    run(bytes, i, |c| !matches!(c, b'\n' | b'\r'))
}

/// The end of one quoted part at `i`. A doubled quote stays inside it, and so does any byte after a
/// backslash when `escapes` is true.
pub(super) fn skip_quoted(bytes: &[u8], i: usize, quote: u8, escapes: bool) -> usize {
    let mut j = i + 1;
    while j < bytes.len() {
        if escapes && bytes[j] == b'\\' {
            j += 2;
        } else if bytes[j] == quote {
            if bytes.get(j + 1) == Some(&quote) {
                j += 2;
            } else {
                return j + 1;
            }
        } else {
            j += 1;
        }
    }
    bytes.len()
}

/// The length of the `$tag$` at `i`. A tag is `scan.l`'s `{dolq_start}{dolq_cont}*`: a letter, `_`, or
/// any byte from 0x80 up, then those or digits. A digit first makes a parameter such as `$1`.
pub(super) fn dollar_tag(bytes: &[u8], i: usize) -> Option<usize> {
    let mut j = i + 1;
    if bytes.get(j).is_some_and(|&c| is_ident_start(c)) {
        j = run(bytes, j + 1, |c| is_ident_start(c) || c.is_ascii_digit());
    }
    (bytes.get(j) == Some(&b'$')).then_some(j + 1 - i)
}

pub(super) fn skip_dollar_quoted(bytes: &[u8], i: usize, tag_len: usize) -> usize {
    let tag = &bytes[i..i + tag_len];
    let mut j = i + tag_len;
    while j + tag_len <= bytes.len() {
        if &bytes[j..j + tag_len] == tag {
            return j + tag_len;
        }
        j += 1;
    }
    bytes.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(sql: &str) -> Vec<&str> {
        split_statements(sql).iter().map(|s| s.text(sql)).collect()
    }

    #[test]
    fn splits_on_semicolons_and_trims() {
        assert_eq!(texts("select 1;\n  select 2  ;;\n"), vec!["select 1", "select 2"]);
        assert_eq!(texts("select 1"), vec!["select 1"]);
        assert_eq!(texts("  \n "), Vec::<&str>::new());
    }

    #[test]
    fn quotes_hide_semicolons() {
        assert_eq!(texts("select 'a;b'; select \"c;d\""), vec!["select 'a;b'", "select \"c;d\""]);
        assert_eq!(texts("select 'it''s;'; select 2"), vec!["select 'it''s;'", "select 2"]);
        assert_eq!(texts(r"select E'\';'; select 2"), vec![r"select E'\';'", "select 2"]);
        assert_eq!(texts(r"select e'a\\'; select 2"), vec![r"select e'a\\'", "select 2"]);
    }

    #[test]
    fn a_continued_e_string_keeps_its_escapes_and_hides_semicolons() {
        let sql = "update t set body = E'it\\'s'\n'\\'; still the string' where id = 1; select 2";
        assert_eq!(texts(sql), vec!["update t set body = E'it\\'s'\n'\\'; still the string' where id = 1", "select 2"]);
        assert_eq!(texts("select 'a'\n'b;c'; select 2"), vec!["select 'a'\n'b;c'", "select 2"]);
    }

    #[test]
    fn dollar_quotes_hide_semicolons_and_parameters_do_not() {
        let sql = "create function f() returns int language sql as $$ select 1; $$; select $1;";
        assert_eq!(texts(sql), vec!["create function f() returns int language sql as $$ select 1; $$", "select $1"]);
        let tagged = "do $body$ begin perform 1; end $body$; select 2";
        assert_eq!(texts(tagged), vec!["do $body$ begin perform 1; end $body$", "select 2"]);
    }

    #[test]
    fn a_dollar_inside_a_name_starts_no_dollar_quote() {
        assert_eq!(texts("select 1 as a$x$; select 2"), vec!["select 1 as a$x$", "select 2"]);
        assert_eq!(texts("select é$x$; select 2"), vec!["select é$x$", "select 2"]);
    }

    #[test]
    fn a_dollar_quote_tag_may_hold_non_ascii_letters() {
        assert_eq!(texts("select $é$ ; $é$; select 2"), vec!["select $é$ ; $é$", "select 2"]);
        assert_eq!(texts("select $_ü1$ a;b $_ü1$ as v; select 2"), vec!["select $_ü1$ a;b $_ü1$ as v", "select 2"]);
    }

    #[test]
    fn comments_hide_semicolons_and_comment_only_chunks_vanish() {
        assert_eq!(texts("select 1 -- a;b\n; select 2"), vec!["select 1 -- a;b", "select 2"]);
        assert_eq!(
            texts("select /* x; /* nested; */ y */ 1; select 2"),
            vec!["select /* x; /* nested; */ y */ 1", "select 2"]
        );
        assert_eq!(texts("-- only a comment\n/* and another */"), Vec::<&str>::new());
        assert_eq!(texts("select 1; -- trailing"), vec!["select 1"]);
    }

    #[test]
    fn a_line_comment_ends_at_a_carriage_return() {
        assert_eq!(texts("select 1 -- c\r; select 2"), vec!["select 1 -- c", "select 2"]);
        assert_eq!(texts("-- only\r-- comments"), Vec::<&str>::new());
        assert_eq!(texts("select 1\n\n-- c\rselect 2"), vec!["select 1", "-- c\rselect 2"]);
    }

    #[test]
    fn begin_atomic_bodies_keep_their_semicolons() {
        let sql = "create function f(x int) returns int begin atomic select case when x > 1 then 1 else 0 end; return x; end; select 2";
        assert_eq!(
            texts(sql),
            vec![
                "create function f(x int) returns int begin atomic select case when x > 1 then 1 else 0 end; return x; end",
                "select 2"
            ]
        );
    }

    #[test]
    fn plain_begin_still_splits() {
        assert_eq!(
            texts("begin; insert into t values (1); commit"),
            vec!["begin", "insert into t values (1)", "commit"]
        );
    }

    const USER_DOC: &str = "SELECT * FROM public.addresses\nWHERE id > 5\nAND id < 10\nORDER BY id DESC\n\nselect * from public.categories";

    #[test]
    fn a_blank_line_before_a_statement_starter_splits() {
        assert_eq!(
            texts(USER_DOC),
            vec![
                "SELECT * FROM public.addresses\nWHERE id > 5\nAND id < 10\nORDER BY id DESC",
                "select * from public.categories"
            ]
        );
        assert_eq!(texts("select 1\n\nselect 2"), vec!["select 1", "select 2"]);
        assert_eq!(texts("select 1\n  \n\t\nselect 2"), vec!["select 1", "select 2"]);
        assert_eq!(texts("select 1\nselect 2"), vec!["select 1\nselect 2"], "only a blank line or a semicolon splits");
        assert_eq!(texts("select 1;\n\nselect 2\n\nselect 3"), vec!["select 1", "select 2", "select 3"]);
        assert_eq!(texts("select 1 -- note\n\n-- header\nselect 2"), vec!["select 1 -- note", "-- header\nselect 2"]);
        assert_eq!(texts("select 1\n\nfrom t"), vec!["select 1\n\nfrom t"], "the next word must start a statement");
    }

    #[test]
    fn a_blank_line_inside_an_unfinished_statement_keeps_it_whole() {
        for sql in [
            "WITH x AS (select 1)\n\nselect * from x",
            "with a as (select 1),\n\nb as (select 2)\n\nselect * from a, b",
            "insert into t (a)\n\nselect 1",
            "insert into t (a)\n\nvalues (1)",
            "select 1\nunion\n\nselect 2",
            "select 1 union all\n\nselect 2",
            "create view v as\n\nselect 1",
            "explain (analyze)\n\nselect 1",
            "explain\n\nselect 1",
            "update t\n\nset a = 1",
            "select case when x then 1\n\nelse 2\n\nend from t",
            "select (\n\nselect 1)",
            "declare c cursor for\n\nselect 1",
            "select a,\n\nb from t",
            "select 1 +\n\nselect 2",
            "create function f() returns int begin atomic\n\nselect 1;\n\nend",
        ] {
            assert_eq!(texts(sql), vec![sql], "{sql:?}");
        }
    }

    #[test]
    fn blank_lines_inside_quotes_and_comments_do_not_split() {
        assert_eq!(texts("select '\n\nselect 2'"), vec!["select '\n\nselect 2'"]);
        assert_eq!(texts("select $$\n\nselect$$"), vec!["select $$\n\nselect$$"]);
        assert_eq!(texts("/*\n\nselect 1 */ select 2"), vec!["/*\n\nselect 1 */ select 2"]);
        assert_eq!(texts("-- c\n\nselect 1"), vec!["select 1"]);
    }

    #[test]
    fn cursor_in_the_users_doc_picks_one_query() {
        let spans = split_statements(USER_DOC);
        let line6 = USER_DOC.rfind("select").unwrap() + 3;
        let at = |off: usize| {
            pick(&spans, USER_DOC, ByteTarget::Cursor(off)).iter().map(|s| s.text(USER_DOC)).collect::<Vec<_>>()
        };
        assert_eq!(at(line6), vec!["select * from public.categories"]);
        assert_eq!(
            at(USER_DOC.find("WHERE").unwrap()),
            vec!["SELECT * FROM public.addresses\nWHERE id > 5\nAND id < 10\nORDER BY id DESC"]
        );
    }

    #[test]
    fn cursor_picks_the_statement_under_it_or_on_its_line() {
        let sql = "select 1;  \n\nselect 2; -- note\n-- alone\n";
        let spans = split_statements(sql);
        let at =
            |off: usize| pick(&spans, sql, ByteTarget::Cursor(off)).iter().map(|s| s.text(sql)).collect::<Vec<_>>();
        assert_eq!(at(0), vec!["select 1"]);
        assert_eq!(at(8), vec!["select 1"]);
        assert_eq!(at(11), vec!["select 1"], "trailing spaces after the semicolon");
        assert_eq!(at(sql.find("note").unwrap()), vec!["select 2"], "a comment on the statement's line");
        assert_eq!(texts_at(sql, &spans, 12), Vec::<&str>::new(), "the blank line between them");
        assert_eq!(texts_at(sql, &spans, sql.find("alone").unwrap()), Vec::<&str>::new(), "a comment-only line");
        assert_eq!(texts_at(sql, &spans, sql.len()), Vec::<&str>::new(), "the empty last line");
    }

    #[test]
    fn cursor_on_a_blank_line_picks_nothing_unless_a_statement_spans_it() {
        let sql = "\n\n  select 1";
        let spans = split_statements(sql);
        assert!(pick(&spans, sql, ByteTarget::Cursor(0)).is_empty());
        assert_eq!(pick(&spans, sql, ByteTarget::Cursor(2)), vec![spans[0]], "the statement's own indentation");
        let with = "with x as (select 1)\n\nselect * from x";
        let spans = split_statements(with);
        assert_eq!(
            texts_at(with, &spans, with.find('\n').unwrap() + 1),
            vec![with],
            "a blank line inside one statement"
        );
        assert!(pick(&[], "", ByteTarget::Cursor(0)).is_empty());
    }

    fn texts_at<'a>(sql: &'a str, spans: &[StmtSpan], off: usize) -> Vec<&'a str> {
        pick(spans, sql, ByteTarget::Cursor(off)).iter().map(|s| s.text(sql)).collect()
    }

    #[test]
    fn selection_runs_exactly_the_selected_text() {
        let sql = "select 1; select 2; select 3";
        let spans = split_statements(sql);
        let picked = pick(&spans, sql, ByteTarget::Selection { from: 10, to: 28 });
        assert_eq!(picked.iter().map(|s| s.text(sql)).collect::<Vec<_>>(), vec!["select 2", "select 3"]);
        let partial = pick(&spans, sql, ByteTarget::Selection { from: 7, to: 8 });
        assert_eq!(partial.iter().map(|s| s.text(sql)).collect::<Vec<_>>(), vec!["1"]);
        assert_eq!(pick(&spans, sql, ByteTarget::Selection { from: 5, to: 5 }), vec![spans[0]]);
        assert_eq!(pick(&spans, sql, ByteTarget::All), spans);
    }
}
