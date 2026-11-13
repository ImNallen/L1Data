use std::borrow::Cow;

use super::keywords::{NEVER_A_FUNCTION, OTHER_KEYWORDS, RESERVED, TYPE_OR_FUNCTION_NAME};
use super::lex::{LexKind, Lexeme, Strings, lex, lex_strings};
use crate::model::FormatError;

/// Lays out one statement in the engine's fixed style. Statements other than queries, DML, and
/// `CREATE TABLE` keep their whitespace and change only keyword case. The result is refused unless
/// it lexes to the same significant lexemes as the input.
pub fn format_statement(sql: &str) -> Result<String, FormatError> {
    let lexemes = lex(sql);
    let (toks, trailing) = read(sql, &lexemes);
    if toks.is_empty() {
        return Err(FormatError::NothingToFormat);
    }
    let out = match root_clause(&toks) {
        Some(clause) => Layout::new(&toks, roles(&toks, false), clause).run(&trailing),
        None => verbatim(sql, &lexemes, &roles(&toks, true)),
    };
    checked(sql, out)
}

/// The formatter has no session, so the output must keep its meaning whether or not the tab runs
/// with `standard_conforming_strings` off.
fn checked(input: &str, output: String) -> Result<String, FormatError> {
    let significant = |sql: &str, strings: Strings| -> Vec<(LexKind, String)> {
        lex_strings(sql, strings)
            .into_iter()
            .filter(|l| l.kind != LexKind::Space)
            .map(|l| {
                (
                    l.kind,
                    if l.kind == LexKind::Word { sql[l.span].to_ascii_lowercase() } else { sql[l.span].to_owned() },
                )
            })
            .collect()
    };
    let same = [Strings::Standard, Strings::Escaped]
        .into_iter()
        .all(|strings| significant(input, strings) == significant(&output, strings));
    if same { Ok(output) } else { Err(FormatError::Unsafe) }
}

/// A significant lexeme with the comments and whitespace the input had before it.
struct Tok<'a> {
    kind: LexKind,
    text: &'a str,
    notes: Vec<Note<'a>>,
    /// Whitespace directly before the token.
    spaced: bool,
}

struct Note<'a> {
    text: &'a str,
    line: bool,
    own_line: bool,
    spaced: bool,
    newline_after: bool,
}

fn read<'a>(sql: &'a str, lexemes: &[Lexeme]) -> (Vec<Tok<'a>>, Vec<Note<'a>>) {
    let mut toks = Vec::new();
    let mut notes: Vec<Note<'a>> = Vec::new();
    let (mut spaced, mut newline) = (false, false);
    for l in lexemes {
        let text = &sql[l.span.clone()];
        if l.kind == LexKind::Space {
            spaced = true;
            newline |= text.contains(['\n', '\r']);
            continue;
        }
        if let Some(last) = notes.last_mut() {
            last.newline_after = newline;
        }
        match l.kind {
            LexKind::LineComment | LexKind::BlockComment => {
                notes.push(Note {
                    text,
                    line: l.kind == LexKind::LineComment,
                    own_line: newline,
                    spaced,
                    newline_after: false,
                });
            }
            kind => {
                toks.push(Tok { kind, text, notes: std::mem::take(&mut notes), spaced });
            }
        }
        (spaced, newline) = (false, false);
    }
    (toks, notes)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Keyword,
    /// Next to a `.` or after `AS`, so a name whatever it spells.
    Name,
    Plain,
}

/// Unreserved and column-name keywords act as keywords only in these phrases. `(` and `)` match
/// punctuation and mark nothing.
const PHRASES: &[&[&str]] = &[
    &["insert", "into"],
    &["delete", "from"],
    &["group", "by"],
    &["order", "by"],
    &["partition", "by"],
    &["on", "conflict"],
    &["on", "constraint"],
    &["do", "nothing"],
    &["do", "update", "set"],
    &["with", "recursive"],
    &["as", "materialized", "("],
    &["as", "not", "materialized", "("],
    &["for", "update"],
    &["for", "no", "key", "update"],
    &["for", "share"],
    &["for", "key", "share"],
    &["skip", "locked"],
    &["fetch", "first"],
    &["fetch", "next"],
    &["row", "only"],
    &["rows", "only"],
    &["row", "fetch"],
    &["rows", "fetch"],
    &["with", "ties"],
    &["with", "ordinality"],
    &["nulls", "first"],
    &["nulls", "last"],
    &[")", "over"],
    &[")", "filter", "("],
    &["within", "group", "("],
    &["rows", "between"],
    &["range", "between"],
    &["groups", "between"],
    &["rows", "unbounded"],
    &["range", "unbounded"],
    &["groups", "unbounded"],
    &["rows", "current", "row"],
    &["range", "current", "row"],
    &["groups", "current", "row"],
    &["unbounded", "preceding"],
    &["unbounded", "following"],
    &["current", "row"],
    &["preceding"],
    &["following"],
    &["exclude", "current", "row"],
    &["exclude", "group"],
    &["exclude", "ties"],
    &["exclude", "no", "others"],
    &["at", "time", "zone"],
    &["with", "time", "zone"],
    &["without", "time", "zone"],
    &["is", "unknown"],
    &["is", "not", "unknown"],
    &["between"],
    &["exists", "("],
    &["grouping", "sets"],
    &["primary", "key"],
    &["foreign", "key"],
    &["on", "delete"],
    &["on", "update"],
    &["on", "delete", "cascade"],
    &["on", "update", "cascade"],
    &["on", "delete", "restrict"],
    &["on", "update", "restrict"],
    &["on", "delete", "set"],
    &["on", "update", "set"],
    &["no", "action"],
    &["generated", "always", "as"],
    &["generated", "by", "default", "as"],
    &["as", "identity"],
    &["if", "exists"],
    &["if", "not", "exists"],
];

const VERBATIM_PHRASES: &[&[&str]] = &[
    &["or", "replace"],
    &["returns"],
    &["language"],
    &["immutable"],
    &["stable"],
    &["volatile"],
    &["strict"],
    &["security", "definer"],
    &["security", "invoker"],
    &["add", "column"],
    &["drop", "column"],
    &["alter", "column"],
    &["rename", "column"],
    &["rename", "to"],
    &["owner", "to"],
    &["cascade"],
    &["restrict"],
];

/// After a leading `CREATE`, `ALTER`, or `DROP`, these words name what the statement acts on.
const DDL_HEAD: &[&str] = &[
    "aggregate",
    "collation",
    "column",
    "concurrently",
    "constraint",
    "data",
    "database",
    "default",
    "domain",
    "event",
    "exists",
    "extension",
    "foreign",
    "function",
    "global",
    "if",
    "index",
    "local",
    "materialized",
    "not",
    "operator",
    "or",
    "policy",
    "privileges",
    "procedure",
    "publication",
    "recursive",
    "replace",
    "role",
    "rule",
    "schema",
    "sequence",
    "server",
    "subscription",
    "table",
    "temp",
    "temporary",
    "trigger",
    "type",
    "unique",
    "unlogged",
    "user",
    "view",
    "wrapper",
];

/// Keywords that are values, so an operator after one is binary.
const VALUE_WORDS: &[&str] = &[
    "current_catalog",
    "current_date",
    "current_role",
    "current_schema",
    "current_time",
    "current_timestamp",
    "current_user",
    "default",
    "false",
    "localtime",
    "localtimestamp",
    "null",
    "session_user",
    "system_user",
    "true",
    "user",
];

fn lower(tok: &Tok<'_>) -> Option<String> {
    (tok.kind == LexKind::Word).then(|| tok.text.to_ascii_lowercase())
}

fn listed(list: &[&str], word: &str) -> bool {
    list.binary_search(&word).is_ok()
}

fn roles(toks: &[Tok<'_>], verbatim: bool) -> Vec<Role> {
    let words: Vec<Option<String>> = toks.iter().map(lower).collect();
    let word = |i: usize| words.get(i).and_then(|w| w.as_deref());
    let kind = |i: usize| toks.get(i).map(|t| t.kind);
    let mut roles: Vec<Role> = (0..toks.len())
        .map(|i| match word(i) {
            Some(w) if listed(RESERVED, w) => Role::Keyword,
            Some(w)
                if listed(TYPE_OR_FUNCTION_NAME, w)
                    && !(matches!(w, "left" | "right") && kind(i + 1) == Some(LexKind::Open)) =>
            {
                Role::Keyword
            }
            _ => Role::Plain,
        })
        .collect();
    if let Some(first) = word(0) {
        if listed(NEVER_A_FUNCTION, first) || listed(OTHER_KEYWORDS, first) {
            roles[0] = Role::Keyword;
        }
        if matches!(first, "create" | "alter" | "drop") {
            let names_what = |i: usize| {
                word(i).is_some_and(|w| DDL_HEAD.contains(&w))
                    && matches!(kind(i + 1), Some(LexKind::Word | LexKind::QuotedIdent))
            };
            let head = (1..toks.len()).take_while(|&i| names_what(i)).count();
            roles[1..=head].fill(Role::Keyword);
        }
    }
    let mut after_as = vec![false; toks.len()];
    let phrases = PHRASES.iter().chain(if verbatim { VERBATIM_PHRASES } else { &[] });
    for phrase in phrases {
        for i in 0..toks.len() {
            let matches = phrase.iter().enumerate().all(|(k, part)| match *part {
                "(" => kind(i + k) == Some(LexKind::Open),
                ")" => kind(i + k) == Some(LexKind::Close),
                w => word(i + k) == Some(w),
            });
            if matches {
                for (k, part) in phrase.iter().enumerate().filter(|(_, p)| !matches!(**p, "(" | ")")) {
                    roles[i + k] = Role::Keyword;
                    after_as[i + k] |= k > 0 && phrase[0] == "as" && part != &"as";
                }
            }
        }
    }
    for i in 0..toks.len() {
        if word(i).is_none() {
            continue;
        }
        let dotted = (i > 0 && kind(i - 1) == Some(LexKind::Dot)) || kind(i + 1) == Some(LexKind::Dot);
        let alias = i > 0 && word(i - 1) == Some("as") && roles[i - 1] == Role::Keyword && !after_as[i];
        if dotted || alias {
            roles[i] = Role::Name;
        }
    }
    roles
}

fn verbatim(sql: &str, lexemes: &[Lexeme], roles: &[Role]) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut i = 0;
    for l in lexemes {
        let text = &sql[l.span.clone()];
        match l.kind {
            LexKind::Space | LexKind::LineComment | LexKind::BlockComment => out.push_str(text),
            _ => {
                if roles[i] == Role::Keyword {
                    out.push_str(&text.to_ascii_uppercase());
                } else {
                    out.push_str(text);
                }
                i += 1;
            }
        }
    }
    out
}

/// Only these statements get a layout; the rest keep their whitespace.
fn root_clause(toks: &[Tok<'_>]) -> Option<Clause> {
    if toks[0].kind == LexKind::Open {
        return Some(Clause::Start);
    }
    let words: Vec<String> = toks.iter().map_while(lower).collect();
    match words.first().map(String::as_str)? {
        "select" | "with" | "values" | "insert" | "update" | "delete" => Some(Clause::Start),
        "create" => {
            let rest = words[1..]
                .iter()
                .skip_while(|w| matches!(w.as_str(), "global" | "local" | "temp" | "temporary" | "unlogged"));
            (rest.clone().next().map(String::as_str) == Some("table")).then_some(Clause::CreateTable)
        }
        _ => None,
    }
}

/// The clause a query frame is in. It decides which keywords may open the next clause and whether
/// a top-level `AND` or `OR` starts a line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Clause {
    Start,
    With,
    Select,
    From,
    Join,
    On,
    Where,
    GroupBy,
    Having,
    Window,
    OrderBy,
    Limit,
    Offset,
    Fetch,
    Locking,
    SetOp,
    InsertInto,
    Values,
    OnConflict,
    Update,
    Set,
    DeleteFrom,
    Returning,
    /// `CREATE TABLE` before its column list, which is the only part laid out.
    CreateTable,
    TableTail,
}

impl Clause {
    fn breaks_and_or(self) -> bool {
        matches!(self, Clause::Where | Clause::On | Clause::Having)
    }

    /// A main `INSERT`, `UPDATE`, or `DELETE` may start here.
    fn before_statement(self) -> bool {
        matches!(self, Clause::Start | Clause::With)
    }

    fn inert(self) -> bool {
        matches!(self, Clause::CreateTable | Clause::TableTail)
    }
}

/// A keyword that opens a clause: the clause, how many words the keyword spans, and whether it starts a line.
struct ClauseStart {
    clause: Clause,
    words: usize,
    line: bool,
}

#[derive(Debug, Clone, Copy)]
enum Frame {
    /// A statement or a subquery, laid out clause by clause. `close` is the indent of a subquery's `)`.
    Query {
        indent: usize,
        close: Option<usize>,
        clause: Clause,
        open_betweens: usize,
    },
    Inline,
    Case {
        indent: usize,
    },
    Columns {
        indent: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sep {
    Tight,
    Space,
    Line(usize),
}

struct Out {
    text: String,
    indent: usize,
}

impl Out {
    fn put(&mut self, sep: Sep, text: &str) {
        if !self.text.is_empty() {
            match sep {
                Sep::Tight => {}
                Sep::Space => self.text.push(' '),
                Sep::Line(indent) => {
                    self.text.push('\n');
                    self.text.extend(std::iter::repeat_n(' ', indent));
                    self.indent = indent;
                }
            }
        }
        self.text.push_str(text);
    }
}

struct Layout<'a> {
    toks: &'a [Tok<'a>],
    words: Vec<Option<String>>,
    roles: Vec<Role>,
    frames: Vec<Frame>,
    out: Out,
    /// The indent of the line the next token starts, after a comma or an opening parenthesis.
    pending: Option<usize>,
    /// Tokens before this index belong to a clause keyword already placed.
    phrase_end: usize,
}

fn root(clause: Clause) -> Frame {
    Frame::Query { indent: 0, close: None, clause, open_betweens: 0 }
}

impl<'a> Layout<'a> {
    fn new(toks: &'a [Tok<'a>], roles: Vec<Role>, clause: Clause) -> Self {
        let words = toks.iter().map(lower).collect();
        Layout {
            toks,
            words,
            roles,
            frames: vec![root(clause)],
            out: Out { text: String::new(), indent: 0 },
            pending: None,
            phrase_end: 0,
        }
    }

    fn run(mut self, trailing: &[Note<'_>]) -> String {
        for i in 0..self.toks.len() {
            let own = self.advance(i);
            let pending = self.pending.take().map(Sep::Line);
            let sep = own.or(pending).unwrap_or_else(|| self.gap(i));
            let tok = &self.toks[i];
            let sep = self.put_notes(&tok.notes, sep, tok.spaced);
            let text = if self.roles[i] == Role::Keyword {
                Cow::Owned(tok.text.to_ascii_uppercase())
            } else {
                Cow::Borrowed(tok.text)
            };
            self.out.put(sep, &text);
            self.open(i, sep);
        }
        self.put_trailing_notes(trailing);
        self.out.text
    }

    fn word(&self, i: usize) -> &str {
        self.words.get(i).and_then(|w| w.as_deref()).unwrap_or("")
    }

    fn kind(&self, i: usize) -> Option<LexKind> {
        self.toks.get(i).map(|t| t.kind)
    }

    fn keyword(&self, i: usize, word: &str) -> bool {
        self.roles.get(i) == Some(&Role::Keyword) && self.word(i) == word
    }

    /// The indent of a line that continues the current item.
    fn cont(&self) -> usize {
        self.frames
            .iter()
            .rev()
            .find_map(|f| match *f {
                Frame::Query { indent, .. } => Some(indent + 2),
                Frame::Case { indent } | Frame::Columns { indent } => Some(indent + 4),
                Frame::Inline => None,
            })
            .unwrap_or(2)
    }

    fn advance(&mut self, i: usize) -> Option<Sep> {
        if matches!(self.kind(i), Some(LexKind::Close | LexKind::CloseBracket)) {
            while let Some(frame) = self.frames.pop() {
                match frame {
                    Frame::Case { .. } => {}
                    Frame::Inline => return None,
                    Frame::Columns { indent } => return Some(Sep::Line(indent)),
                    Frame::Query { close: Some(close), .. } => return Some(Sep::Line(close)),
                    Frame::Query { close: None, .. } => {
                        self.frames.push(frame);
                        return None;
                    }
                }
            }
        }
        if self.kind(i) != Some(LexKind::Word) || self.roles[i] == Role::Name {
            return None;
        }
        match self.frames.last_mut() {
            Some(&mut Frame::Case { indent }) => match self.word(i) {
                "when" | "else" if self.roles[i] == Role::Keyword => Some(Sep::Line(indent + 2)),
                "end" if self.roles[i] == Role::Keyword => {
                    self.frames.pop();
                    Some(Sep::Line(indent))
                }
                _ => None,
            },
            Some(&mut Frame::Query { indent, clause, open_betweens, .. }) => {
                if i >= self.phrase_end
                    && let Some(start) = self.clause_at(i, clause)
                {
                    self.roles[i..i + start.words].fill(Role::Keyword);
                    self.phrase_end = i + start.words;
                    self.set_query(start.clause, 0);
                    return start.line.then_some(Sep::Line(indent));
                }
                if self.keyword(i, "between") {
                    self.set_query(clause, open_betweens + 1);
                } else if self.keyword(i, "and") && open_betweens > 0 {
                    self.set_query(clause, open_betweens - 1);
                } else if (self.keyword(i, "and") || self.keyword(i, "or")) && clause.breaks_and_or() {
                    return Some(Sep::Line(indent + 2));
                }
                None
            }
            _ => None,
        }
    }

    fn set_query(&mut self, next: Clause, betweens: usize) {
        if let Some(Frame::Query { clause, open_betweens, .. }) = self.frames.last_mut() {
            (*clause, *open_betweens) = (next, betweens);
        }
    }

    /// The clause a word opens in a query frame.
    fn clause_at(&self, i: usize, current: Clause) -> Option<ClauseStart> {
        use Clause::*;
        if current.inert() {
            return None;
        }
        let line = |clause, words| ClauseStart { clause, words, line: true };
        let inline = |clause, words| ClauseStart { clause, words, line: false };
        let w = |k: usize| self.word(i + k);
        Some(match w(0) {
            "select" => line(Select, 1),
            "with" if current == Start => line(With, if w(1) == "recursive" { 2 } else { 1 }),
            "insert" if w(1) == "into" && current.before_statement() => line(InsertInto, 2),
            "update" if current.before_statement() => line(Update, 1),
            "delete" if w(1) == "from" && current.before_statement() => line(DeleteFrom, 2),
            "values"
                if matches!(current, Start | With | InsertInto | SetOp)
                    && !(i > 0 && self.word(i - 1) == "default") =>
            {
                line(Values, 1)
            }
            "from" if !(i > 1 && self.word(i - 1) == "distinct" && matches!(self.word(i - 2), "is" | "not")) => {
                line(From, 1)
            }
            "on" if current == Join => inline(On, 1),
            "on" if w(1) == "conflict" => line(OnConflict, 2),
            "do" if current == OnConflict && w(1) == "update" && w(2) == "set" => inline(Set, 3),
            "set" if current == Update => line(Set, 1),
            "where" => line(Where, 1),
            "group" if w(1) == "by" => line(GroupBy, 2),
            "having" => line(Having, 1),
            "window" => line(Window, 1),
            "order" if w(1) == "by" => line(OrderBy, 2),
            "limit" => line(Limit, 1),
            "offset" => line(Offset, 1),
            "fetch" if matches!(w(1), "first" | "next") => line(Fetch, 2),
            "for" if matches!(w(1), "update" | "share" | "no" | "key") => line(Locking, 1),
            "union" | "intersect" | "except" => line(SetOp, if matches!(w(1), "all" | "distinct") { 2 } else { 1 }),
            "returning" => line(Returning, 1),
            _ => {
                let mut k = usize::from(w(0) == "natural");
                match w(k) {
                    "inner" | "cross" => k += 1,
                    "left" | "right" | "full" => k += if w(k + 1) == "outer" { 2 } else { 1 },
                    _ => {}
                }
                return (w(k) == "join").then_some(line(Join, k + 1));
            }
        })
    }

    fn open(&mut self, i: usize, sep: Sep) {
        match self.kind(i) {
            Some(LexKind::Open) => {
                let subquery =
                    matches!(self.word(i + 1), "select" | "with" | "values" | "insert" | "update" | "delete")
                        && self.roles[i + 1] != Role::Name;
                let line = self.out.indent;
                let frame = match self.frames.last() {
                    _ if subquery => {
                        Frame::Query { indent: line + 2, close: Some(line), clause: Clause::Start, open_betweens: 0 }
                    }
                    Some(Frame::Query { clause: Clause::CreateTable, .. }) => {
                        self.set_query(Clause::TableTail, 0);
                        Frame::Columns { indent: line }
                    }
                    _ => Frame::Inline,
                };
                if !matches!(frame, Frame::Inline) {
                    self.pending = Some(line + 2);
                }
                self.frames.push(frame);
            }
            Some(LexKind::OpenBracket) => self.frames.push(Frame::Inline),
            // A comma that a line comment pushed onto its own line already starts the item's line.
            Some(LexKind::Comma) if !matches!(sep, Sep::Line(_)) => {
                if let Some(Frame::Query { indent, .. } | Frame::Columns { indent }) = self.frames.last() {
                    self.pending = Some(indent + 2);
                }
            }
            Some(LexKind::Semicolon) => {
                self.frames = vec![root(Clause::Start)];
                self.pending = Some(0);
            }
            Some(LexKind::Word) if self.keyword(i, "case") => {
                let indent = self.cont();
                self.frames.push(Frame::Case { indent });
            }
            _ => {}
        }
    }

    fn gap(&self, i: usize) -> Sep {
        use LexKind::*;
        if i == 0 {
            return Sep::Tight;
        }
        let (a, b) = (&self.toks[i - 1], &self.toks[i]);
        if a.kind == Other || b.kind == Other {
            return if b.spaced || !b.notes.is_empty() { Sep::Space } else { Sep::Tight };
        }
        let tight = matches!(b.kind, Comma | Close | CloseBracket | Semicolon | Dot | OpenBracket)
            || matches!(a.kind, Open | OpenBracket | Dot)
            || a.text == "::"
            || b.text == "::"
            || (b.kind == Open && self.callable(i - 1))
            || self.prefix(i - 1);
        if tight { Sep::Tight } else { Sep::Space }
    }

    /// A word whose `(` opens its arguments, so the two stay together.
    fn callable(&self, i: usize) -> bool {
        if matches!(self.frames.last(), Some(Frame::Query { clause: Clause::CreateTable, .. })) {
            return false;
        }
        match (self.toks[i].kind, self.roles[i]) {
            (LexKind::QuotedIdent, _) => true,
            (LexKind::Word, Role::Keyword) => match self.word(i) {
                "array" | "cast" => true,
                "any" | "all" | "some" => i > 0 && self.kind(i - 1) == Some(LexKind::Operator),
                _ => false,
            },
            (LexKind::Word, _) => true,
            _ => false,
        }
    }

    fn prefix(&self, i: usize) -> bool {
        self.kind(i) == Some(LexKind::Operator)
            && self.toks[i].text != "::"
            && !(i > 0 && self.ends_operand(i - 1))
            && self.starts_operand(i + 1)
            && self.kind(i + 1) != Some(LexKind::Operator)
    }

    fn ends_operand(&self, i: usize) -> bool {
        use LexKind::*;
        match self.kind(i) {
            Some(QuotedIdent | String | DollarString | Number | Param | Close | CloseBracket) => true,
            Some(Word) => self.roles[i] != Role::Keyword || listed(VALUE_WORDS, self.word(i)) || self.word(i) == "end",
            _ => false,
        }
    }

    fn starts_operand(&self, i: usize) -> bool {
        use LexKind::*;
        match self.kind(i) {
            Some(QuotedIdent | String | DollarString | Number | Param | Open | Operator) => true,
            Some(Word) => {
                self.roles[i] != Role::Keyword
                    || listed(VALUE_WORDS, self.word(i))
                    || matches!(self.word(i), "array" | "case" | "cast")
            }
            _ => false,
        }
    }

    /// Writes the comments before a token and returns the separator the token gets after them.
    fn put_notes(&mut self, notes: &[Note<'_>], next: Sep, spaced: bool) -> Sep {
        if notes.is_empty() {
            return next;
        }
        let indent = match next {
            Sep::Line(indent) => indent,
            _ => self.cont(),
        };
        let owed = self.write_notes(notes, indent);
        match next {
            Sep::Line(_) => next,
            _ if owed => Sep::Line(indent),
            _ if spaced => Sep::Space,
            _ => Sep::Tight,
        }
    }

    fn put_trailing_notes(&mut self, notes: &[Note<'_>]) {
        self.write_notes(notes, self.cont());
    }

    /// Writes each comment where the input had it relative to its neighbors, and returns whether the
    /// last one ends its line.
    fn write_notes(&mut self, notes: &[Note<'_>], indent: usize) -> bool {
        let mut owed = false;
        for note in notes {
            let sep = if note.own_line || owed {
                Sep::Line(indent)
            } else if note.spaced {
                Sep::Space
            } else {
                Sep::Tight
            };
            self.out.put(sep, note.text);
            owed = note.line || note.newline_after;
        }
        owed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(sql: &str) -> String {
        format_statement(sql).unwrap_or_else(|e| panic!("{e:?} for {sql:?}"))
    }

    const SAMPLE: &str = "select o.id, o.total::numeric(10,2), c.meta->>'tier' as tier from orders o join customers c on c.id = o.customer_id where o.status = any(array['paid','shipped']) and o.note ilike E'%\\'x%' -- trailing\norder by 2 desc";

    #[test]
    fn a_query_gets_one_clause_per_line_and_keeps_strings_operators_and_comments() {
        assert_eq!(
            fmt(SAMPLE),
            "SELECT o.id,\n  o.total::numeric(10, 2),\n  c.meta ->> 'tier' AS tier\nFROM orders o\nJOIN customers c ON c.id = o.customer_id\nWHERE o.status = ANY(ARRAY['paid', 'shipped'])\n  AND o.note ILIKE E'%\\'x%' -- trailing\nORDER BY 2 DESC"
        );
    }

    const FUNCTION: &str = "create function f() returns int language plpgsql as $body$ begin   return  1; end $body$;";

    #[test]
    fn a_function_keeps_its_whitespace_and_its_body_byte_for_byte() {
        let out = fmt(FUNCTION);
        assert_eq!(out, "CREATE FUNCTION f() RETURNS int LANGUAGE plpgsql AS $body$ begin   return  1; end $body$;");
        assert!(out.contains("$body$ begin   return  1; end $body$"));
    }

    const UPSERT: &str = "insert into t(a,b) values (1,'x') on conflict (a) do update set b = excluded.b returning *";

    #[test]
    fn an_upsert_keeps_do_update_set_on_the_on_conflict_line() {
        assert_eq!(
            fmt(UPSERT),
            "INSERT INTO t(a, b)\nVALUES (1, 'x')\nON CONFLICT (a) DO UPDATE SET b = excluded.b\nRETURNING *"
        );
    }

    const WINDOW: &str = "select sum(total) over (partition by region order by day rows between 1 preceding and current row) from s /* keep me */";

    #[test]
    fn a_window_stays_inline_and_a_column_named_day_keeps_its_case() {
        assert_eq!(
            fmt(WINDOW),
            "SELECT sum(total) OVER (PARTITION BY region ORDER BY day ROWS BETWEEN 1 PRECEDING AND CURRENT ROW)\nFROM s /* keep me */"
        );
    }

    const OPERATORS: &str = "select $1::int, x #>> '{a,b}', y @> '[1]'::jsonb, z !~ 'q' from t";

    #[test]
    fn operators_get_one_space_and_casts_and_params_stay_tight() {
        assert_eq!(fmt(OPERATORS), "SELECT $1::int,\n  x #>> '{a,b}',\n  y @> '[1]'::jsonb,\n  z !~ 'q'\nFROM t");
    }

    const CTE: &str = "with recent as (select id, total from orders where created_at > now() - interval '7 days'), big as (select * from recent where total > (select avg(total) from recent)) select b.id from big b order by b.total desc limit 5";

    #[test]
    fn a_cte_and_its_nested_subquery_indent_two_more_each() {
        assert_eq!(
            fmt(CTE),
            "WITH recent AS (\n  SELECT id,\n    total\n  FROM orders\n  WHERE created_at > now() - interval '7 days'\n),\n  big AS (\n    SELECT *\n    FROM recent\n    WHERE total > (\n      SELECT avg(total)\n      FROM recent\n    )\n  )\nSELECT b.id\nFROM big b\nORDER BY b.total DESC\nLIMIT 5"
        );
    }

    const CASE: &str = "select id, case when total > 100 then 'big' when total between 10 and 100 and paid then 'mid' else 'small' end as size from orders where a between 1 and 2 and b or c";

    #[test]
    fn case_puts_each_branch_on_its_own_line_and_between_keeps_its_and() {
        assert_eq!(
            fmt(CASE),
            "SELECT id,\n  CASE\n    WHEN total > 100 THEN 'big'\n    WHEN total BETWEEN 10 AND 100 AND paid THEN 'mid'\n    ELSE 'small'\n  END AS size\nFROM orders\nWHERE a BETWEEN 1 AND 2\n  AND b\n  OR c"
        );
    }

    const CREATE_TABLE: &str = "create table customers (id bigint generated always as identity primary key, name text not null, type text, day date default current_date, status text check (status in ('a','b')), constraint customers_name_key unique (name))";

    #[test]
    fn create_table_puts_each_column_on_its_own_line_and_leaves_column_names_alone() {
        assert_eq!(
            fmt(CREATE_TABLE),
            "CREATE TABLE customers (\n  id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,\n  name text NOT NULL,\n  type text,\n  day date DEFAULT CURRENT_DATE,\n  status text CHECK (status IN ('a', 'b')),\n  CONSTRAINT customers_name_key UNIQUE (name)\n)"
        );
    }

    const COMMENTED: &str = "select a, -- first column\n  b, /* second */ c\n-- the source\nfrom t";

    const LEADING_COMMA: &str = "select a -- first\n, b from t";

    #[test]
    fn a_line_comment_between_items_ends_its_line() {
        assert_eq!(fmt(COMMENTED), "SELECT a, -- first column\n  b, /* second */\n  c\n-- the source\nFROM t");
        assert_eq!(fmt(LEADING_COMMA), "SELECT a -- first\n  , b\nFROM t");
    }

    const FORMATTED: &str =
        "SELECT o.id,\n  o.total\nFROM orders o\nWHERE o.status = 'paid'\n  AND o.total > 10\nORDER BY 2 DESC";

    #[test]
    fn a_formatted_statement_comes_back_unchanged() {
        assert_eq!(fmt(FORMATTED), FORMATTED);
    }

    #[test]
    fn names_that_spell_keywords_keep_their_case() {
        assert_eq!(
            fmt("select t.name, type, status, day, x as select from t where t.from = 1"),
            "SELECT t.name,\n  type,\n  status,\n  day,\n  x AS select\nFROM t\nWHERE t.from = 1"
        );
    }

    #[test]
    fn a_string_continued_across_a_newline_stays_byte_for_byte() {
        assert_eq!(fmt("select 'a'\n'b', 'c' from t"), "SELECT 'a'\n'b',\n  'c'\nFROM t");
        assert_eq!(
            fmt("update t set body = E'it\\'s'\n'\\'  two  spaces' where id = 1"),
            "UPDATE t\nSET body = E'it\\'s'\n'\\'  two  spaces'\nWHERE id = 1"
        );
    }

    #[test]
    fn an_underscored_number_with_a_fraction_stays_whole() {
        assert_eq!(fmt("select 1_000.5 as x, 1_000.25e-3, 1..2"), "SELECT 1_000.5 AS x,\n  1_000.25e-3,\n  1..2");
    }

    #[test]
    fn strings_that_postgres_would_not_join_are_never_joined() {
        assert_eq!(fmt("select 'a' 'b' from t"), "SELECT 'a' 'b'\nFROM t");
    }

    #[test]
    fn a_dollar_quote_with_a_non_ascii_tag_keeps_its_body_byte_for_byte() {
        assert_eq!(
            fmt("insert into notes(body) values ($ü$line one\n    indented line$ü$)"),
            "INSERT INTO notes(body)\nVALUES ($ü$line one\n    indented line$ü$)"
        );
        assert_eq!(fmt("select $é$select from t where x$é$ as v"), "SELECT $é$select from t where x$é$ AS v");
    }

    #[test]
    fn a_lone_carriage_return_ends_a_line() {
        assert_eq!(fmt("select a -- note\rfrom t"), "SELECT a -- note\nFROM t");
        assert_eq!(fmt("select a,\r/* own */ b from t"), "SELECT a,\n  /* own */\n  b\nFROM t");
    }

    #[test]
    fn other_statements_change_only_keyword_case() {
        assert_eq!(fmt("alter table orders  add column note text"), "ALTER TABLE orders  ADD COLUMN note text");
        assert_eq!(fmt("do $$ begin perform 1; end $$"), "DO $$ begin perform 1; end $$");
        assert_eq!(fmt("grant select on t to app"), "GRANT SELECT ON t TO app");
    }

    #[test]
    fn a_statement_of_only_comments_has_nothing_to_format() {
        assert_eq!(format_statement(" -- nothing\n"), Err(FormatError::NothingToFormat));
    }

    #[test]
    fn output_that_lexes_differently_is_refused() {
        assert_eq!(checked("select a - -1", "SELECT a --1".into()), Err(FormatError::Unsafe));
        assert_eq!(checked("select 'a'", "SELECT 'b'".into()), Err(FormatError::Unsafe));
        assert_eq!(checked("select a /* c */", "SELECT a".into()), Err(FormatError::Unsafe));
        assert_eq!(checked("select \"A\"", "SELECT \"a\"".into()), Err(FormatError::Unsafe));
        assert_eq!(checked("select a  from\nt", "SELECT a\nFROM t".into()), Ok("SELECT a\nFROM t".into()));
    }

    #[test]
    fn a_backslash_that_would_end_a_string_with_standard_strings_off_is_refused() {
        assert_eq!(format_statement("select 'a\\'b' as x"), Err(FormatError::Unsafe));
        assert_eq!(format_statement("select n'a\\'b' as x"), Err(FormatError::Unsafe));
        assert_eq!(fmt("select 'C:\\path' as p from t"), "SELECT 'C:\\path' AS p\nFROM t");
        assert_eq!(fmt("select x'1F\\' as p from t"), "SELECT x'1F\\' AS p\nFROM t");
    }

    #[test]
    fn formatting_twice_changes_nothing_more() {
        for sql in
            [SAMPLE, FUNCTION, UPSERT, WINDOW, OPERATORS, CTE, CASE, CREATE_TABLE, COMMENTED, LEADING_COMMA, FORMATTED]
        {
            let once = fmt(sql);
            assert_eq!(fmt(&once), once, "{sql:?}");
        }
    }
}
