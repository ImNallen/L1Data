use std::borrow::Cow;

use super::{Kind, RESERVED, STATEMENT_WORDS, Token, lex, split_batches};

/// Why the formatter keeps the text as it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Refusal {
    TooLarge,
    NoBatch,
    /// The lexer or the batch splitter refused the text, as a run would.
    Lex(String),
    /// Every batch defines a procedure, function, trigger, or view, and those keep their text.
    OnlyModules,
    Statement(String),
    Syntax(String),
    Symbol(String),
    /// A gap holds whitespace other than space, tab, CR, or LF, which SQL Server might not read as a gap.
    Whitespace,
    Unbalanced,
    Unmatched(&'static str),
    Incomplete,
    /// The laid-out text would lex differently from the input.
    ChangesTokens,
}

/// Where a statement or a clause stands. A statement is laid out clause by clause, and the clause decides
/// which keyword may open the next one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Clause {
    Select,
    Into,
    From,
    Join,
    On,
    Where,
    GroupBy,
    Having,
    OrderBy,
    Offset,
    Fetch,
    QueryOption,
    SetOp,
    InsertTarget,
    Output,
    Values,
    UpdateTarget,
    Set,
    DeleteHead,
    DeleteTarget,
    MergeTarget,
    Using,
    MergeOn,
    When,
    Then,
    MergeUpdate,
    MergeDelete,
    MergeInsert,
    Ctes,
    Declare,
    SetOption,
    Condition,
}

use Clause::*;

impl Clause {
    fn breaks_and_or(self) -> bool {
        matches!(self, Where | On | Having | MergeOn)
    }

    /// A statement that stops here has not reached its body.
    fn incomplete(self) -> bool {
        matches!(
            self,
            InsertTarget
                | UpdateTarget
                | DeleteHead
                | MergeTarget
                | Using
                | When
                | Then
                | MergeUpdate
                | MergeInsert
                | Ctes
                | Condition
        )
    }
}

/// The statements the formatter lays out, by their first word, and the clause each opens. `BEGIN ... END`
/// and `ELSE` are the control flow between them.
const STATEMENTS: &[(&str, Clause)] = &[
    ("SELECT", Select),
    ("INSERT", InsertTarget),
    ("UPDATE", UpdateTarget),
    ("DELETE", DeleteHead),
    ("MERGE", MergeTarget),
    ("WITH", Ctes),
    ("DECLARE", Declare),
    ("SET", SetOption),
    ("IF", Condition),
];

/// What a parenthesized query starts with.
const SUBQUERIES: &[(&str, Clause)] = &[("SELECT", Select), ("VALUES", Values)];

/// `BEGIN` followed by one of these is another statement, not a block.
const NOT_A_BLOCK: &[&str] =
    &["ATOMIC", "CATCH", "CONVERSATION", "DIALOG", "DISTRIBUTED", "TRAN", "TRANSACTION", "TRY"];

const REFUSED_WORDS: &[&str] = &["COMPUTE", "FOR", "PIVOT", "UNPIVOT"];

const MODULES: &[&str] = &["FUNCTION", "PROC", "PROCEDURE", "TRIGGER", "VIEW"];

const SYMBOLS: &[&str] = &[
    "(", ")", ".", ",", ";", "+", "-", "*", "/", "%", "&", "|", "^", "~", "=", "<", ">", "<=", ">=", "<>", "!=", "!<",
    "!>", "+=", "-=", "*=", "/=", "%=", "&=", "^=", "|=", "::", "$",
];

/// Reserved words whose `(` opens their arguments, written against it.
const FUNCTION_KEYWORDS: &[&str] = &[
    "COALESCE",
    "CONTAINS",
    "CONTAINSTABLE",
    "CONVERT",
    "FREETEXT",
    "FREETEXTTABLE",
    "IDENTITY",
    "INDEX",
    "LEFT",
    "NULLIF",
    "OPENDATASOURCE",
    "OPENQUERY",
    "OPENROWSET",
    "OPENXML",
    "RIGHT",
    "TRY_CONVERT",
];

/// Reserved words that are values, so an operator after one is binary.
const VALUE_WORDS: &[&str] = &[
    "CURRENT_DATE",
    "CURRENT_TIME",
    "CURRENT_TIMESTAMP",
    "CURRENT_USER",
    "END",
    "NULL",
    "SESSION_USER",
    "SYSTEM_USER",
    "USER",
];

/// Where a clause keyword goes: on a new line this far past its statement's indent, or on the current line.
#[derive(Debug, Clone, Copy)]
enum Place {
    Line(usize),
    Inline,
}

/// A keyword that opens `to` when the statement is in one of the `after` clauses, or in any clause for `None`.
struct Rule {
    words: &'static [&'static str],
    after: Option<&'static [Clause]>,
    to: Clause,
    place: Place,
}

const fn rule(words: &'static [&'static str], after: Option<&'static [Clause]>, to: Clause, place: Place) -> Rule {
    Rule { words, after, to, place }
}

const JOINED: Option<&[Clause]> = Some(&[From, Join, On]);
const MERGE_ACTION_DONE: Option<&[Clause]> = Some(&[MergeOn, Set, MergeDelete, Values]);
const LINE: Place = Place::Line(0);
const ACTION: Place = Place::Line(2);

/// The clauses, in the order they are tried. The first match wins.
const RULES: &[Rule] = &[
    rule(&["SELECT"], Some(&[Ctes, InsertTarget, Output, SetOp]), Select, LINE),
    rule(&["INSERT"], Some(&[Ctes]), InsertTarget, LINE),
    rule(&["UPDATE"], Some(&[Ctes]), UpdateTarget, LINE),
    rule(&["DELETE"], Some(&[Ctes]), DeleteHead, LINE),
    rule(&["MERGE"], Some(&[Ctes]), MergeTarget, LINE),
    rule(&["INTO"], Some(&[Select]), Into, LINE),
    rule(&["FROM"], Some(&[DeleteHead]), DeleteTarget, Place::Inline),
    rule(&["FROM"], None, From, LINE),
    rule(&["JOIN"], JOINED, Join, LINE),
    rule(&["INNER", "JOIN"], JOINED, Join, LINE),
    rule(&["LEFT", "OUTER", "JOIN"], JOINED, Join, LINE),
    rule(&["LEFT", "JOIN"], JOINED, Join, LINE),
    rule(&["RIGHT", "OUTER", "JOIN"], JOINED, Join, LINE),
    rule(&["RIGHT", "JOIN"], JOINED, Join, LINE),
    rule(&["FULL", "OUTER", "JOIN"], JOINED, Join, LINE),
    rule(&["FULL", "JOIN"], JOINED, Join, LINE),
    rule(&["CROSS", "JOIN"], JOINED, Join, LINE),
    rule(&["CROSS", "APPLY"], JOINED, Join, LINE),
    rule(&["OUTER", "APPLY"], JOINED, Join, LINE),
    rule(&["ON"], Some(&[Join]), On, Place::Inline),
    rule(&["ON"], Some(&[Using]), MergeOn, LINE),
    rule(&["WHERE"], None, Where, LINE),
    rule(&["GROUP", "BY"], None, GroupBy, LINE),
    rule(&["HAVING"], None, Having, LINE),
    rule(&["ORDER", "BY"], None, OrderBy, LINE),
    rule(&["OFFSET"], Some(&[OrderBy]), Offset, LINE),
    rule(&["FETCH"], Some(&[Offset]), Fetch, LINE),
    rule(&["UNION", "ALL"], None, SetOp, LINE),
    rule(&["UNION"], None, SetOp, LINE),
    rule(&["EXCEPT"], None, SetOp, LINE),
    rule(&["INTERSECT"], None, SetOp, LINE),
    rule(&["OPTION"], None, QueryOption, LINE),
    rule(&["OUTPUT"], Some(&[InsertTarget, Set, DeleteTarget, MergeOn, MergeDelete, Values]), Output, LINE),
    rule(&["DEFAULT", "VALUES"], Some(&[InsertTarget, Output]), Values, LINE),
    rule(&["VALUES"], Some(&[InsertTarget, Output]), Values, LINE),
    rule(&["DEFAULT", "VALUES"], Some(&[MergeInsert]), Values, ACTION),
    rule(&["VALUES"], Some(&[MergeInsert]), Values, ACTION),
    rule(&["SET"], Some(&[UpdateTarget]), Set, LINE),
    rule(&["SET"], Some(&[MergeUpdate]), Set, Place::Inline),
    rule(&["USING"], Some(&[MergeTarget]), Using, LINE),
    rule(&["WHEN", "MATCHED"], MERGE_ACTION_DONE, When, LINE),
    rule(&["WHEN", "NOT", "MATCHED"], MERGE_ACTION_DONE, When, LINE),
    rule(&["THEN"], Some(&[When]), Then, Place::Inline),
    rule(&["UPDATE"], Some(&[Then]), MergeUpdate, ACTION),
    rule(&["DELETE"], Some(&[Then]), MergeDelete, ACTION),
    rule(&["INSERT"], Some(&[Then]), MergeInsert, ACTION),
];

fn reserved(word: &str) -> bool {
    RESERVED.binary_search_by(|k| k.bytes().cmp(word.bytes().map(|b| b.to_ascii_uppercase()))).is_ok()
}

/// Which tokens are keywords: reserved words not next to a `.`. A reserved word cannot be a bare name, so its
/// case never matters, while any other word may name something in a case-sensitive collation.
fn keywords(tokens: &[Token<'_>]) -> Vec<bool> {
    let code: Vec<usize> = (0..tokens.len()).filter(|&i| !tokens[i].comment()).collect();
    let mut out = vec![false; tokens.len()];
    for (k, &i) in code.iter().enumerate() {
        let dot = |j: Option<&usize>| j.is_some_and(|&j| tokens[j].text == ".");
        out[i] = tokens[i].kind == Kind::Word
            && reserved(tokens[i].text)
            && !dot(k.checked_sub(1).and_then(|k| code.get(k)))
            && !dot(code.get(k + 1));
    }
    out
}

/// A `CREATE` or `ALTER` of a procedure, function, trigger, or view, whose body runs to the end of its batch.
fn defines_module(tokens: &[Token<'_>]) -> bool {
    let code: Vec<&Token<'_>> = tokens.iter().filter(|t| !t.comment()).collect();
    let word = |i: usize, w: &str| code.get(i).is_some_and(|t| t.word(w));
    (0..code.len()).any(|i| {
        let head = if word(i, "CREATE") && word(i + 1, "OR") && word(i + 2, "ALTER") { i + 3 } else { i + 1 };
        (word(i, "CREATE") || word(i, "ALTER")) && MODULES.iter().any(|m| word(head, m))
    })
}

pub(crate) fn format(text: &str) -> Result<String, Refusal> {
    if text.len() > 65_536 {
        return Err(Refusal::TooLarge);
    }
    let batches = split_batches(text).map_err(Refusal::Lex)?;
    if batches.is_empty() {
        return Err(Refusal::NoBatch);
    }
    let mut out = String::new();
    let mut at = 0;
    let mut laid_out = false;
    for span in batches {
        out.push_str(&text[at..span.start]);
        let batch = span.text(text);
        let tokens = lex(batch).map_err(Refusal::Lex)?;
        if defines_module(&tokens) {
            out.push_str(batch);
        } else {
            out.push_str(&format_batch(batch, &tokens)?);
            laid_out = true;
        }
        at = span.end;
    }
    if !laid_out {
        return Err(Refusal::OnlyModules);
    }
    out.push_str(&text[at..]);
    if signature(text)? != signature(&out)? {
        return Err(Refusal::ChangesTokens);
    }
    Ok(out)
}

type Signature<'a> = Vec<Vec<(Kind, Cow<'a, str>)>>;

/// Each batch's tokens, comments included, with keywords in one case.
fn signature(text: &str) -> Result<Signature<'_>, Refusal> {
    split_batches(text)
        .map_err(Refusal::Lex)?
        .into_iter()
        .map(|span| {
            let tokens = lex(span.text(text)).map_err(Refusal::Lex)?;
            Ok(tokens
                .iter()
                .zip(keywords(&tokens))
                .map(|(t, keyword)| {
                    (t.kind, if keyword { Cow::Owned(t.text.to_ascii_uppercase()) } else { Cow::Borrowed(t.text) })
                })
                .collect())
        })
        .collect()
}

/// Line breaks in a gap, counting `\r\n` once.
fn breaks(gap: &str) -> usize {
    gap.matches('\n').count() + gap.matches('\r').count() - gap.matches("\r\n").count()
}

fn format_batch(text: &str, tokens: &[Token<'_>]) -> Result<String, Refusal> {
    let keywords = keywords(tokens);
    let mut toks: Vec<Tok<'_>> = Vec::new();
    let mut notes: Vec<Note<'_>> = Vec::new();
    let mut end = 0;
    for (i, (token, &keyword)) in tokens.iter().zip(&keywords).enumerate() {
        let gap = &text[end..token.start];
        if gap.chars().any(|c| !matches!(c, ' ' | '\t' | '\r' | '\n')) {
            return Err(Refusal::Whitespace);
        }
        let newline = gap.contains(['\r', '\n']);
        let blank = breaks(gap) >= 2;
        if let Some(last) = notes.last_mut() {
            last.newline_after = newline;
        }
        end = token.end;
        if token.comment() {
            notes.push(Note {
                text: token.text,
                line: token.kind == Kind::LineComment,
                own_line: newline,
                blank,
                spaced: !gap.is_empty(),
                newline_after: false,
            });
            continue;
        }
        if token.kind == Kind::Symbol && !SYMBOLS.contains(&token.text) {
            return Err(Refusal::Symbol(token.text.to_owned()));
        }
        if token.text == "$"
            && !tokens.get(i + 1).is_some_and(|n| n.start == token.end && matches!(n.kind, Kind::Word | Kind::Number))
        {
            return Err(Refusal::Symbol(token.text.to_owned()));
        }
        if keyword && REFUSED_WORDS.iter().any(|w| token.word(w)) {
            return Err(Refusal::Syntax(token.text.to_owned()));
        }
        toks.push(Tok { token: *token, keyword, notes: std::mem::take(&mut notes), spaced: !gap.is_empty(), blank });
    }
    Layout::new(&toks).run(&notes)
}

/// A significant token with the comments and whitespace the input had before it.
struct Tok<'a> {
    token: Token<'a>,
    keyword: bool,
    notes: Vec<Note<'a>>,
    /// Whitespace directly before the token.
    spaced: bool,
    /// A blank line directly before the token.
    blank: bool,
}

struct Note<'a> {
    text: &'a str,
    line: bool,
    own_line: bool,
    blank: bool,
    spaced: bool,
    newline_after: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Branch {
    Then,
    /// The statement after the condition is complete, so an `ELSE` may follow.
    Done,
    Else,
}

#[derive(Debug, Clone, Copy)]
enum Frame {
    /// The batch, or a `BEGIN ... END` block, holding statements.
    Block {
        indent: usize,
        begin: bool,
    },
    If {
        indent: usize,
        branch: Branch,
    },
    /// A statement, or a subquery whose `)` goes at `close`. `clause_indent` is the indent of the line its
    /// current clause started.
    Query {
        indent: usize,
        close: Option<usize>,
        clause: Clause,
        clause_indent: usize,
        betweens: usize,
    },
    Inline,
    Case {
        indent: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sep {
    Tight,
    Space,
    Line(usize),
}

enum Step {
    Placed(Option<Sep>),
    /// The token starts another statement.
    Ends,
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
    frames: Vec<Frame>,
    out: Out,
    /// The indent of the line the next token starts, after a comma or an opening parenthesis.
    pending: Option<usize>,
    /// Tokens before this index belong to a keyword already placed.
    phrase_end: usize,
    starts_statement: bool,
}

impl<'a> Layout<'a> {
    fn new(toks: &'a [Tok<'a>]) -> Self {
        Layout {
            toks,
            frames: vec![Frame::Block { indent: 0, begin: false }],
            out: Out { text: String::new(), indent: 0 },
            pending: None,
            phrase_end: 0,
            starts_statement: false,
        }
    }

    fn run(mut self, trailing: &[Note<'_>]) -> Result<String, Refusal> {
        for i in 0..self.toks.len() {
            self.starts_statement = false;
            let own = self.advance(i)?;
            let pending = self.pending.take().map(Sep::Line);
            let mut sep = own.or(pending).unwrap_or_else(|| self.gap(i));
            if self.glued(i) {
                sep = Sep::Tight;
            }
            let tok = &self.toks[i];
            let sep = self.put_notes(&tok.notes, sep, tok.spaced);
            if self.starts_statement && tok.blank && matches!(sep, Sep::Line(_)) {
                self.blank_line();
            }
            let text = tok.token.text;
            let text = if tok.keyword { Cow::Owned(text.to_ascii_uppercase()) } else { Cow::Borrowed(text) };
            self.out.put(sep, &text);
            self.open(i, sep);
        }
        self.close()?;
        self.starts_statement = true;
        self.write_notes(trailing, self.cont());
        Ok(self.out.text)
    }

    fn text(&self, i: usize) -> &str {
        self.toks.get(i).map_or("", |t| t.token.text)
    }

    fn keyword(&self, i: usize, word: &str) -> bool {
        self.toks.get(i).is_some_and(|t| t.keyword && t.token.text.eq_ignore_ascii_case(word))
    }

    fn after_keyword(&self, i: usize, word: &str) -> bool {
        i.checked_sub(1).is_some_and(|p| self.keyword(p, word))
    }

    /// A word that is not next to a `.`, so it may be a keyword even when it is not reserved.
    fn bare_word(&self, i: usize, word: &str) -> bool {
        let dot = |j: Option<usize>| j.is_some_and(|j| self.text(j) == ".");
        self.toks.get(i).is_some_and(|t| t.token.word(word)) && !dot(i.checked_sub(1)) && !dot(Some(i + 1))
    }

    fn top_query(&mut self) -> Option<(&mut Clause, &mut usize, &mut usize)> {
        match self.frames.last_mut() {
            Some(Frame::Query { clause, clause_indent, betweens, .. }) => Some((clause, clause_indent, betweens)),
            _ => None,
        }
    }

    fn set_clause(&mut self, to: Clause, indent: Option<usize>) {
        if let Some((clause, clause_indent, betweens)) = self.top_query() {
            *clause = to;
            *betweens = 0;
            if let Some(indent) = indent {
                *clause_indent = indent;
            }
        }
    }

    fn advance(&mut self, i: usize) -> Result<Option<Sep>, Refusal> {
        match self.text(i) {
            ")" => return self.close_paren(),
            ";" => return self.semicolon(i),
            _ => {}
        }
        match self.frames.last().copied() {
            Some(Frame::Case { indent }) => Ok(if self.keyword(i, "WHEN") || self.keyword(i, "ELSE") {
                Some(Sep::Line(indent + 2))
            } else if self.keyword(i, "END") {
                self.frames.pop();
                Some(Sep::Line(indent))
            } else {
                None
            }),
            Some(Frame::Inline) => Ok(None),
            Some(Frame::Query { close: Some(_), .. }) => match self.query_token(i) {
                Step::Placed(sep) => Ok(sep),
                Step::Ends => Err(Refusal::Syntax(self.text(i).to_owned())),
            },
            _ => self.statement_level(i),
        }
    }

    fn close_paren(&mut self) -> Result<Option<Sep>, Refusal> {
        match self.frames.pop() {
            Some(Frame::Inline) => Ok(None),
            Some(Frame::Query { close: Some(close), .. }) => Ok(Some(Sep::Line(close))),
            _ => Err(Refusal::Unbalanced),
        }
    }

    fn semicolon(&mut self, i: usize) -> Result<Option<Sep>, Refusal> {
        match self.frames.last() {
            Some(Frame::Query { close: None, .. }) => self.end_statement(i, false)?,
            Some(Frame::Block { .. } | Frame::If { branch: Branch::Done, .. }) => {}
            _ => return Err(Refusal::Syntax(";".into())),
        }
        Ok(Some(Sep::Tight))
    }

    /// A word inside a statement: a clause keyword, a word that starts the next statement, or part of the
    /// current clause.
    fn query_token(&mut self, i: usize) -> Step {
        let Some(Frame::Query { indent, clause, clause_indent, betweens, .. }) = self.frames.last().copied() else {
            return Step::Placed(None);
        };
        if i < self.phrase_end {
            return Step::Placed(None);
        }
        if let Some(rule) = self.rule_at(i, clause) {
            self.phrase_end = i + rule.words.len();
            return Step::Placed(match rule.place {
                Place::Line(offset) => {
                    self.set_clause(rule.to, Some(indent + offset));
                    Some(Sep::Line(indent + offset))
                }
                Place::Inline => {
                    self.set_clause(rule.to, None);
                    None
                }
            });
        }
        if self.starter(i) {
            return Step::Ends;
        }
        let tok = &self.toks[i];
        let name = matches!(tok.token.kind, Kind::Ident | Kind::Variable | Kind::Reference)
            || tok.token.kind == Kind::Word && !tok.keyword;
        if clause == DeleteHead && name {
            self.set_clause(DeleteTarget, None);
        } else if self.keyword(i, "BETWEEN") {
            self.set_betweens(betweens + 1);
        } else if self.keyword(i, "AND") && betweens > 0 {
            self.set_betweens(betweens - 1);
        } else if (self.keyword(i, "AND") || self.keyword(i, "OR")) && clause.breaks_and_or() {
            return Step::Placed(Some(Sep::Line(clause_indent + 2)));
        }
        Step::Placed(None)
    }

    fn set_betweens(&mut self, n: usize) {
        if let Some((_, _, betweens)) = self.top_query() {
            *betweens = n;
        }
    }

    fn rule_at(&self, i: usize, clause: Clause) -> Option<&'static Rule> {
        RULES.iter().find(|r| {
            r.after.is_none_or(|after| after.contains(&clause))
                && r.words.iter().enumerate().all(|(k, w)| self.bare_word(i + k, w))
                && (reserved(r.words[0]) || i.checked_sub(1).is_some_and(|p| self.may_open(p)))
        })
    }

    /// Whether a word that is not reserved may open a clause after the token at `p`. After an operator, a comma,
    /// or `BY`, it is a name, such as a column named `output`.
    fn may_open(&self, p: usize) -> bool {
        let tok = &self.toks[p];
        !(self.keyword(p, "BY") || tok.token.kind == Kind::Symbol && tok.token.text != ")")
    }

    /// A reserved word that only starts a statement, or a `WITH` that starts CTEs.
    fn starter(&self, i: usize) -> bool {
        let tok = &self.toks[i];
        tok.keyword && STATEMENT_WORDS.iter().any(|w| tok.token.word(w)) && (!tok.token.word("WITH") || self.cte_at(i))
    }

    /// `WITH name AS (` or `WITH name (column, ...) AS (`.
    fn cte_at(&self, i: usize) -> bool {
        let name =
            |k: usize| self.toks.get(k).is_some_and(|t| matches!(t.token.kind, Kind::Word | Kind::Ident) && !t.keyword);
        if !name(i + 1) {
            return false;
        }
        let mut k = i + 2;
        if self.text(k) == "(" {
            k += 1;
            loop {
                if !name(k) {
                    return false;
                }
                k += 1;
                match self.text(k) {
                    "," => k += 1,
                    ")" => break,
                    _ => return false,
                }
            }
            k += 1;
        }
        self.keyword(k, "AS") && self.text(k + 1) == "("
    }

    fn statement_level(&mut self, i: usize) -> Result<Option<Sep>, Refusal> {
        loop {
            match self.frames.last().copied() {
                Some(Frame::Query { .. }) => match self.query_token(i) {
                    Step::Placed(sep) => return Ok(sep),
                    Step::Ends => self.end_statement(i, true)?,
                },
                Some(Frame::If { indent, branch: Branch::Done }) => {
                    self.frames.pop();
                    if self.keyword(i, "ELSE") {
                        self.frames.push(Frame::If { indent, branch: Branch::Else });
                        return Ok(Some(Sep::Line(indent)));
                    }
                    self.finished();
                }
                Some(Frame::If { indent, branch }) => {
                    let else_if = branch == Branch::Else && self.after_keyword(i, "ELSE");
                    return self.start(i, indent + 2, indent, else_if);
                }
                Some(Frame::Block { indent, begin }) => {
                    if self.keyword(i, "END") {
                        if !begin {
                            return Err(Refusal::Unmatched("END"));
                        }
                        self.frames.pop();
                        self.finished();
                        return Ok(Some(Sep::Line(indent)));
                    }
                    let inner = if begin { indent + 2 } else { indent };
                    return self.start(i, inner, inner, false);
                }
                _ => return Err(Refusal::Unbalanced),
            }
        }
    }

    /// Starts a statement at `indent`, or a `BEGIN` block at `block`. `else_if` keeps an `IF` on its `ELSE` line.
    fn start(&mut self, i: usize, indent: usize, block: usize, else_if: bool) -> Result<Option<Sep>, Refusal> {
        let tok = &self.toks[i];
        self.starts_statement = true;
        if self.keyword(i, "BEGIN") {
            if let Some(next) = self.toks.get(i + 1).filter(|n| NOT_A_BLOCK.iter().any(|w| n.token.word(w))) {
                return Err(Refusal::Statement(format!("{} {}", tok.token.text, next.token.text)));
            }
            self.frames.push(Frame::Block { indent: block, begin: true });
            return Ok(Some(Sep::Line(block)));
        }
        for word in ["ELSE", "END"] {
            if self.keyword(i, word) {
                return Err(Refusal::Unmatched(word));
            }
        }
        let Some(&(_, clause)) = STATEMENTS.iter().find(|(w, _)| tok.keyword && tok.token.word(w)) else {
            return Err(Refusal::Statement(tok.token.text.to_owned()));
        };
        if clause == Ctes && !self.cte_at(i) {
            return Err(Refusal::Syntax(tok.token.text.to_owned()));
        }
        let (indent, sep) =
            if else_if && clause == Condition { (block, Sep::Space) } else { (indent, Sep::Line(indent)) };
        self.frames.push(Frame::Query { indent, close: None, clause, clause_indent: indent, betweens: 0 });
        self.phrase_end = i + 1;
        Ok(Some(sep))
    }

    /// Ends the statement on top. `body` is true when the token at `i` starts the next one.
    fn end_statement(&mut self, i: usize, body: bool) -> Result<(), Refusal> {
        let Some(Frame::Query { indent, clause, .. }) = self.frames.pop() else {
            return Err(Refusal::Unbalanced);
        };
        if clause == Condition && body && !self.after_keyword(i, "IF") {
            self.frames.push(Frame::If { indent, branch: Branch::Then });
            return Ok(());
        }
        if clause.incomplete() {
            return Err(Refusal::Incomplete);
        }
        self.finished();
        Ok(())
    }

    /// A statement is complete, which may complete the `IF` it is a branch of.
    fn finished(&mut self) {
        loop {
            match self.frames.last_mut() {
                Some(Frame::If { branch: branch @ Branch::Then, .. }) => {
                    *branch = Branch::Done;
                    return;
                }
                Some(Frame::If { branch: Branch::Else, .. }) => {
                    self.frames.pop();
                }
                _ => return,
            }
        }
    }

    fn close(&mut self) -> Result<(), Refusal> {
        while let Some(frame) = self.frames.last().copied() {
            match frame {
                Frame::Query { close: None, .. } => self.end_statement(self.toks.len(), false)?,
                Frame::If { branch: Branch::Done, .. } => {
                    self.frames.pop();
                    self.finished();
                }
                Frame::Block { begin: false, .. } => return Ok(()),
                Frame::If { .. } => return Err(Refusal::Incomplete),
                _ => return Err(Refusal::Unbalanced),
            }
        }
        Ok(())
    }

    fn open(&mut self, i: usize, sep: Sep) {
        match self.text(i) {
            "(" => {
                let next = self.toks.get(i + 1).filter(|n| n.keyword);
                let subquery = next.and_then(|n| SUBQUERIES.iter().find(|(w, _)| n.token.word(w)));
                if let Some(&(_, clause)) = subquery {
                    let line = self.out.indent;
                    let indent = line + 2;
                    self.frames.push(Frame::Query {
                        indent,
                        close: Some(line),
                        clause,
                        clause_indent: indent,
                        betweens: 0,
                    });
                    self.pending = Some(indent);
                    self.phrase_end = i + 2;
                } else {
                    self.frames.push(Frame::Inline);
                }
            }
            // A comma that a line comment pushed onto its own line already starts the item's line.
            "," if !matches!(sep, Sep::Line(_)) => {
                if let Some(Frame::Query { clause, clause_indent, .. }) = self.frames.last() {
                    self.pending = Some(clause_indent + if *clause == Ctes { 0 } else { 2 });
                }
            }
            _ if self.keyword(i, "CASE") => {
                let indent = self.cont();
                self.frames.push(Frame::Case { indent });
            }
            _ => {}
        }
    }

    /// The indent of a line that continues the current item.
    fn cont(&self) -> usize {
        self.frames
            .iter()
            .rev()
            .find_map(|f| match *f {
                Frame::Query { clause_indent, .. } => Some(clause_indent + 2),
                Frame::Case { indent } => Some(indent + 4),
                Frame::If { indent, .. } => Some(indent + 2),
                Frame::Block { indent, begin } => Some(if begin { indent + 2 } else { indent }),
                Frame::Inline => None,
            })
            .unwrap_or(0)
    }

    /// Tokens the input had touching, where a gap could make SQL Server read them as different tokens, such
    /// as `$action` or `1a`.
    fn glued(&self, i: usize) -> bool {
        let Some(p) = i.checked_sub(1) else {
            return false;
        };
        let (a, b) = (&self.toks[p], &self.toks[i]);
        if b.spaced || !b.notes.is_empty() {
            return false;
        }
        let free = |t: &Tok<'_>| matches!(t.token.text, "(" | ")" | "," | ";" | ".");
        let symbol = |t: &Tok<'_>| t.token.kind == Kind::Symbol;
        a.token.text == "$"
            || symbol(a) && symbol(b) && !free(a) && !free(b) && !self.prefix(i)
            || !symbol(a) && !symbol(b)
    }

    fn gap(&self, i: usize) -> Sep {
        let Some(p) = i.checked_sub(1) else {
            return Sep::Tight;
        };
        let (a, b) = (self.text(p), self.text(i));
        let tight = matches!(b, "," | ")" | ";" | "." | "::")
            || matches!(a, "(" | "." | "$" | "::")
            || b == "(" && self.call(p)
            || self.prefix(p);
        if tight { Sep::Tight } else { Sep::Space }
    }

    /// A word whose `(` opens its arguments. A name keeps the spacing it had, since `t (a, b)` may be a column list.
    fn call(&self, i: usize) -> bool {
        let (tok, paren) = (&self.toks[i], &self.toks[i + 1]);
        match tok.token.kind {
            Kind::Word if tok.keyword => FUNCTION_KEYWORDS.iter().any(|w| tok.token.word(w)),
            Kind::Word | Kind::Ident | Kind::Variable | Kind::Reference => !paren.spaced,
            _ => false,
        }
    }

    fn prefix(&self, i: usize) -> bool {
        matches!(self.text(i), "-" | "+" | "~")
            && !i.checked_sub(1).is_some_and(|p| self.ends_operand(p))
            && self.starts_operand(i + 1)
    }

    fn ends_operand(&self, i: usize) -> bool {
        let Some(tok) = self.toks.get(i) else {
            return false;
        };
        match tok.token.kind {
            Kind::Ident | Kind::String | Kind::Number | Kind::Variable | Kind::Reference => true,
            Kind::Symbol => tok.token.text == ")",
            Kind::Word => !tok.keyword || VALUE_WORDS.iter().any(|w| tok.token.word(w)),
            Kind::LineComment | Kind::BlockComment => false,
        }
    }

    fn starts_operand(&self, i: usize) -> bool {
        let Some(tok) = self.toks.get(i) else {
            return false;
        };
        match tok.token.kind {
            Kind::Ident | Kind::String | Kind::Number | Kind::Variable | Kind::Reference => true,
            Kind::Symbol => matches!(tok.token.text, "(" | "$"),
            Kind::Word => {
                !tok.keyword
                    || tok.token.word("CASE")
                    || FUNCTION_KEYWORDS.iter().any(|w| tok.token.word(w))
                    || VALUE_WORDS.iter().any(|w| *w != "END" && tok.token.word(w))
            }
            Kind::LineComment | Kind::BlockComment => false,
        }
    }

    /// Keeps a blank line the input had between statements.
    fn blank_line(&mut self) {
        if !self.out.text.is_empty() {
            self.out.text.push('\n');
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

    /// Writes each comment where the input had it relative to its neighbors, and returns whether the last one
    /// ends its line.
    fn write_notes(&mut self, notes: &[Note<'_>], indent: usize) -> bool {
        let mut owed = false;
        for note in notes {
            if self.starts_statement && note.blank {
                self.blank_line();
            }
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
    use crate::text::text_commands_never_panic::{env_or, random, texts};

    const CTES: &[(&str, &str)] = &[
        (
            "with c as (select id, n from t) select * from c",
            "WITH c AS (\n  SELECT id,\n    n\n  FROM t\n)\nSELECT *\nFROM c",
        ),
        (
            "with a (id) as (select 1), b as (select id from a) select b.id from b join a on a.id = b.id",
            "WITH a (id) AS (\n  SELECT 1\n),\nb AS (\n  SELECT id\n  FROM a\n)\nSELECT b.id\nFROM b\nJOIN a ON a.id = b.id",
        ),
        (
            "WITH c AS (SELECT id FROM t) UPDATE t SET n = 1 FROM t JOIN c ON c.id = t.id",
            "WITH c AS (\n  SELECT id\n  FROM t\n)\nUPDATE t\nSET n = 1\nFROM t\nJOIN c ON c.id = t.id",
        ),
        (
            "WITH c AS (SELECT id FROM t) DELETE FROM c WHERE id > 1",
            "WITH c AS (\n  SELECT id\n  FROM t\n)\nDELETE FROM c\nWHERE id > 1",
        ),
    ];

    const JOINS: &[(&str, &str)] = &[
        (
            "select a.id from a inner join b on b.id = a.id left join c on c.id = a.id right outer join d on d.id = a.id full join e on e.id = a.id left outer join g on g.id = a.id and g.n = 1 cross join f",
            "SELECT a.id\nFROM a\nINNER JOIN b ON b.id = a.id\nLEFT JOIN c ON c.id = a.id\nRIGHT OUTER JOIN d ON d.id = a.id\nFULL JOIN e ON e.id = a.id\nLEFT OUTER JOIN g ON g.id = a.id\n  AND g.n = 1\nCROSS JOIN f",
        ),
        (
            "select o.id, l.n from dbo.orders o cross apply (select top (1) n from dbo.lines l where l.order_id = o.id order by n desc) l outer apply dbo.totals(o.id) t",
            "SELECT o.id,\n  l.n\nFROM dbo.orders o\nCROSS apply (\n  SELECT TOP (1) n\n  FROM dbo.lines l\n  WHERE l.order_id = o.id\n  ORDER BY n DESC\n) l\nOUTER apply dbo.totals(o.id) t",
        ),
    ];

    const SUBQUERIES_IN: &[(&str, &str)] = &[
        (
            "select x.id from (select id from t where n > 1) as x where x.id in (select id from u) and exists (select 1 from v where v.id = x.id) and x.n = (select max(n) from w)",
            "SELECT x.id\nFROM (\n  SELECT id\n  FROM t\n  WHERE n > 1\n) AS x\nWHERE x.id IN (\n  SELECT id\n  FROM u\n)\n  AND EXISTS (\n    SELECT 1\n    FROM v\n    WHERE v.id = x.id\n  )\n  AND x.n = (\n    SELECT max(n)\n    FROM w\n  )",
        ),
        (
            "select v.a from (values (1), (2)) as v(a) where v.a not in (1, 2)",
            "SELECT v.a\nFROM (\n  VALUES (1),\n    (2)\n) AS v(a)\nWHERE v.a NOT IN (1, 2)",
        ),
    ];

    const CASES: &[(&str, &str)] = &[(
        "select id, case status when 1 then N'open' when 2 then N'closed' end as label, case when total > 100 then case when vip = 1 then 'a' else 'b' end else 'c' end as tier from orders",
        "SELECT id,\n  CASE status\n    WHEN 1 THEN N'open'\n    WHEN 2 THEN N'closed'\n  END AS label,\n  CASE\n    WHEN total > 100 THEN CASE\n        WHEN vip = 1 THEN 'a'\n        ELSE 'b'\n      END\n    ELSE 'c'\n  END AS tier\nFROM orders",
    )];

    const MERGES: &[(&str, &str)] = &[
        (
            "merge into dbo.target with (holdlock) as t using dbo.source as s on t.id = s.id when matched and t.n <> s.n then update set n = s.n, changed = 1 when not matched by target then insert (id, n) values (s.id, s.n) when not matched by source then delete output $action, inserted.id, deleted.id;",
            "MERGE INTO dbo.target WITH (HOLDLOCK) AS t\nusing dbo.source AS s\nON t.id = s.id\nWHEN matched AND t.n <> s.n THEN\n  UPDATE SET n = s.n,\n    changed = 1\nWHEN NOT matched BY target THEN\n  INSERT (id, n)\n  VALUES (s.id, s.n)\nWHEN NOT matched BY source THEN\n  DELETE\noutput $action,\n  inserted.id,\n  deleted.id;",
        ),
        (
            "MERGE t USING (SELECT id FROM s) AS s ON t.id = s.id WHEN NOT MATCHED THEN INSERT DEFAULT VALUES OUTPUT inserted.id INTO @ids (id) OPTION (RECOMPILE);",
            "MERGE t\nUSING (\n  SELECT id\n  FROM s\n) AS s\nON t.id = s.id\nWHEN NOT MATCHED THEN\n  INSERT\n  DEFAULT VALUES\nOUTPUT inserted.id INTO @ids (id)\nOPTION (RECOMPILE);",
        ),
    ];

    const CONTROL_FLOW: &[(&str, &str)] = &[
        (
            "if @n > 10 select 'big' else if @n > 0 select 'small' else select 'none'",
            "IF @n > 10\n  SELECT 'big'\nELSE IF @n > 0\n  SELECT 'small'\nELSE\n  SELECT 'none'",
        ),
        (
            "if exists (select 1 from t where id = @id) begin update t set n = n + 1 where id = @id; end else begin insert t (id, n) values (@id, 1); end",
            "IF EXISTS (\n  SELECT 1\n  FROM t\n  WHERE id = @id\n)\nBEGIN\n  UPDATE t\n  SET n = n + 1\n  WHERE id = @id;\nEND\nELSE\nBEGIN\n  INSERT t (id, n)\n  VALUES (@id, 1);\nEND",
        ),
        (
            "IF @a = 1 IF @b = 1 SELECT 1; ELSE SELECT 2; SELECT 3",
            "IF @a = 1\n  IF @b = 1\n    SELECT 1;\n  ELSE\n    SELECT 2;\nSELECT 3",
        ),
        (
            "begin declare @n int; set @n = 1; begin select @n end end",
            "BEGIN\n  DECLARE @n int;\n  SET @n = 1;\n  BEGIN\n    SELECT @n\n  END\nEND",
        ),
    ];

    const VARIABLES: &[(&str, &str)] = &[(
        "declare @id int = 1, @name nvarchar(50) = N'Ada'; declare @t table (id int primary key, name nvarchar(50)); set nocount on; set @id += 1; set @name = (select top (1) name from dbo.people where id = @id)",
        "DECLARE @id int = 1,\n  @name nvarchar(50) = N'Ada';\nDECLARE @t TABLE (id int PRIMARY KEY, name nvarchar(50));\nSET nocount ON;\nSET @id += 1;\nSET @name = (\n  SELECT TOP (1) name\n  FROM dbo.people\n  WHERE id = @id\n)",
    )];

    const KEPT: &[(&str, &str)] = &[
        (
            "select top (10) percent with ties [order].[id], @x as v from dbo.[order] with (nolock, index(ix_order)) where name like N'%a%' order by id desc option (recompile, maxdop 1)",
            "SELECT TOP (10) PERCENT WITH ties [order].[id],\n  @x AS v\nFROM dbo.[order] WITH (nolock, INDEX(ix_order))\nWHERE name LIKE N'%a%'\nORDER BY id DESC\nOPTION (recompile, maxdop 1)",
        ),
        (
            "select id from t order by id desc offset @skip rows fetch next 10 rows only",
            "SELECT id\nFROM t\nORDER BY id DESC\noffset @skip rows\nFETCH next 10 rows only",
        ),
        (
            "select a from t union all select b from u except select c from v intersect select d from w",
            "SELECT a\nFROM t\nUNION ALL\nSELECT b\nFROM u\nEXCEPT\nSELECT c\nFROM v\nINTERSECT\nSELECT d\nFROM w",
        ),
        (
            "SELECT o.id, COUNT(*) AS n, ROW_NUMBER() OVER (PARTITION BY o.customer_id ORDER BY o.id) AS rn FROM dbo.orders AS o GROUP BY o.id, o.customer_id HAVING COUNT(*) > 1 AND SUM(o.qty) BETWEEN 1 AND 9",
            "SELECT o.id,\n  COUNT(*) AS n,\n  ROW_NUMBER() OVER (PARTITION BY o.customer_id ORDER BY o.id) AS rn\nFROM dbo.orders AS o\nGROUP BY o.id,\n  o.customer_id\nHAVING COUNT(*) > 1\n  AND SUM(o.qty) BETWEEN 1 AND 9",
        ),
        (
            "select geometry::Point(1, 2, 4326).STAsText(), $1.50, {{orders}}.id, 1a, 0x1G, a||b, -1, - -1, ~x, a - -b from {{orders}}",
            "SELECT geometry::Point(1, 2, 4326).STAsText(),\n  $1.50,\n  {{orders}}.id,\n  1a,\n  0x1G,\n  a || b,\n  -1,\n  - -1,\n  ~x,\n  a - -b\nFROM {{orders}}",
        ),
        (
            "select name, type, status, output, offset, matched from t order by offset",
            "SELECT name,\n  type,\n  status,\n  output,\n  offset,\n  matched\nFROM t\nORDER BY offset",
        ),
    ];

    const DML: &[(&str, &str)] = &[
        (
            "insert into dbo.t (id,n) values (1,N'hello'),(2,@n);",
            "INSERT INTO dbo.t (id, n)\nVALUES (1, N'hello'),\n  (2, @n);",
        ),
        ("INSERT #t DEFAULT VALUES;", "INSERT #t\nDEFAULT VALUES;"),
        (
            "Insert dbo.t(id,n) Select id,n From dbo.s Where id>@id Order by id;",
            "INSERT dbo.t(id, n)\nSELECT id,\n  n\nFROM dbo.s\nWHERE id > @id\nORDER BY id;",
        ),
        (
            "update t set n=@n,id=id+1 output inserted.id from dbo.t t where id=2;",
            "UPDATE t\nSET n = @n,\n  id = id + 1\noutput inserted.id\nFROM dbo.t t\nWHERE id = 2;",
        ),
        (
            "DELETE TOP (5) FROM #t OUTPUT deleted.* WHERE id = @id;",
            "DELETE TOP (5) FROM #t\nOUTPUT deleted.*\nWHERE id = @id;",
        ),
        (
            "delete t from dbo.t t join dbo.s s on t.id=s.id where s.n=1;",
            "DELETE t\nFROM dbo.t t\nJOIN dbo.s s ON t.id = s.id\nWHERE s.n = 1;",
        ),
        (
            "update t set n=(select max(n) from s) delete from t insert t values (1) select * into #c from t",
            "UPDATE t\nSET n = (\n  SELECT max(n)\n  FROM s\n)\nDELETE FROM t\nINSERT t\nVALUES (1)\nSELECT *\nINTO #c\nFROM t",
        ),
    ];

    const COMMENTED: &[(&str, &str)] = &[
        (
            "-- header\nselect  [旅]]人] ,N'go\nGO\nit''s', @value from [Sales].[T] /* keep\n GO */ where id=1 order by [旅]]人] desc; -- end\nGO -- separator\nselect  \"MixedCase\", lower(N'A') from #Temp;\n",
            "-- header\nSELECT [旅]]人],\n  N'go\nGO\nit''s',\n  @value\nFROM [Sales].[T] /* keep\n GO */\nWHERE id = 1\nORDER BY [旅]]人] DESC; -- end\nGO -- separator\nSELECT \"MixedCase\",\n  lower(N'A')\nFROM #Temp;\n",
        ),
        (
            "insert /* target */ into [旅]]人] values (N'😀',-1,1e-3,@旅); -- keep\nGO -- batch\nDELETE /* keep */ FROM #t WHERE n=-@n;",
            "INSERT /* target */ INTO [旅]]人]\nVALUES (N'😀', -1, 1e-3, @旅); -- keep\nGO -- batch\nDELETE /* keep */ FROM #t\nWHERE n = -@n;",
        ),
        (
            "-- load\ndeclare @n int = 1; -- counter\n\n/* main */\nselect a, -- first\n  b from t\nwhere a = 1\n  -- trailing\n\n\nupdate t set n = 2",
            "-- load\nDECLARE @n int = 1; -- counter\n\n/* main */\nSELECT a, -- first\n  b\nFROM t\nWHERE a = 1\n-- trailing\n\nUPDATE t\nSET n = 2",
        ),
    ];

    const MODULE_SCRIPT: (&str, &str) = (
        "select  1\nGO\ncreate or alter procedure dbo.p as\nbegin\n  select   1 -- as written\nend\nGO\nselect  a,b from t",
        "SELECT 1\nGO\ncreate or alter procedure dbo.p as\nbegin\n  select   1 -- as written\nend\nGO\nSELECT a,\n  b\nFROM t",
    );

    fn groups() -> [&'static [(&'static str, &'static str)]; 10] {
        [CTES, JOINS, SUBQUERIES_IN, CASES, MERGES, CONTROL_FLOW, VARIABLES, KEPT, DML, COMMENTED]
    }

    /// Each batch's tokens, comments included, where only a reserved word away from a `.` may change case. It is
    /// written apart from `signature`, so a change there cannot loosen what these tests hold the formatter to.
    fn tokens_of(text: &str) -> Vec<Vec<(Kind, String)>> {
        split_batches(text)
            .unwrap()
            .iter()
            .map(|span| {
                let tokens = lex(span.text(text)).unwrap();
                let code: Vec<&Token<'_>> = tokens.iter().filter(|t| !t.comment()).collect();
                tokens
                    .iter()
                    .map(|t| {
                        let at = code.iter().position(|c| c.start == t.start);
                        let dotted = at.is_some_and(|k| {
                            k.checked_sub(1).is_some_and(|p| code[p].text == ".")
                                || code.get(k + 1).is_some_and(|n| n.text == ".")
                        });
                        let upper = t.text.to_ascii_uppercase();
                        let keyword = t.kind == Kind::Word && RESERVED.contains(&upper.as_str()) && !dotted;
                        (t.kind, if keyword { upper } else { t.text.to_owned() })
                    })
                    .collect()
            })
            .collect()
    }

    fn check(input: &str, expected: &str) {
        let formatted = format(input).unwrap_or_else(|e| panic!("{e:?} for {input:?}"));
        assert_eq!(formatted, expected, "{input:?}");
        assert_eq!(tokens_of(&formatted), tokens_of(input), "{input:?}");
        assert_eq!(format(&formatted).as_deref(), Ok(expected), "formatting twice: {input:?}");
    }

    #[test]
    fn ctes_indent_each_body_and_start_the_statement_they_feed_on_its_own_line() {
        CTES.iter().for_each(|(i, e)| check(i, e));
    }

    #[test]
    fn each_join_and_apply_starts_a_line_and_its_on_conditions_break_at_and() {
        JOINS.iter().for_each(|(i, e)| check(i, e));
    }

    #[test]
    fn subqueries_in_from_where_in_and_exists_indent_inside_their_parentheses() {
        SUBQUERIES_IN.iter().for_each(|(i, e)| check(i, e));
    }

    #[test]
    fn case_puts_each_branch_on_its_own_line() {
        CASES.iter().for_each(|(i, e)| check(i, e));
    }

    #[test]
    fn merge_puts_each_when_on_a_line_and_indents_its_action() {
        MERGES.iter().for_each(|(i, e)| check(i, e));
    }

    #[test]
    fn if_else_and_begin_end_indent_their_statements() {
        CONTROL_FLOW.iter().for_each(|(i, e)| check(i, e));
    }

    #[test]
    fn declare_and_set_are_statements_of_their_own() {
        VARIABLES.iter().for_each(|(i, e)| check(i, e));
    }

    #[test]
    fn top_offset_fetch_hints_options_and_glued_tokens_stay_intact() {
        KEPT.iter().for_each(|(i, e)| check(i, e));
    }

    #[test]
    fn dml_lays_out_with_or_without_semicolons() {
        DML.iter().for_each(|(i, e)| check(i, e));
    }

    #[test]
    fn comments_strings_go_lines_and_blank_lines_between_statements_stay() {
        COMMENTED.iter().for_each(|(i, e)| check(i, e));
    }

    #[test]
    fn a_module_batch_keeps_its_text_and_the_batches_around_it_format() {
        check(MODULE_SCRIPT.0, MODULE_SCRIPT.1);
        assert_eq!(format("ALTER VIEW v AS select  1"), Err(Refusal::OnlyModules));
        let shared = "select  1\nGO\nselect 1\nCREATE TRIGGER t ON x AFTER INSERT AS select  1";
        assert_eq!(
            format(shared).as_deref(),
            Ok("SELECT 1\nGO\nselect 1\nCREATE TRIGGER t ON x AFTER INSERT AS select  1")
        );
    }

    #[test]
    fn unsupported_text_is_refused_with_its_reason() {
        let refusals: &[(&str, Refusal)] = &[
            ("BEGIN TRAN; UPDATE t SET a = 1; COMMIT", Refusal::Statement("BEGIN TRAN".into())),
            ("BEGIN TRY SELECT 1 END TRY BEGIN CATCH THROW; END CATCH", Refusal::Statement("BEGIN TRY".into())),
            ("EXEC dbo.p", Refusal::Statement("EXEC".into())),
            ("SELECT 1; THROW 50000, 'x', 1", Refusal::Statement("THROW".into())),
            ("WHILE @n > 0 SET @n -= 1", Refusal::Statement("WHILE".into())),
            ("CREATE TABLE t (id int)", Refusal::Statement("CREATE".into())),
            ("(SELECT 1) UNION (SELECT 2)", Refusal::Statement("(".into())),
            ("INSERT t EXEC dbo.p", Refusal::Incomplete),
            ("SELECT name FROM t FOR XML PATH('')", Refusal::Syntax("FOR".into())),
            ("DECLARE c CURSOR FOR SELECT 1", Refusal::Syntax("FOR".into())),
            ("SELECT * FROM t PIVOT (MAX(v) FOR k IN ([a])) p", Refusal::Syntax("PIVOT".into())),
            ("WITH XMLNAMESPACES ('urn:x' AS x) SELECT 1", Refusal::Syntax("WITH".into())),
            ("SELECT (1; UPDATE t SET x = 1)", Refusal::Syntax(";".into())),
            ("SELECT (SELECT 1 UPDATE t SET x = 1)", Refusal::Syntax("UPDATE".into())),
            ("ELSE SELECT 1", Refusal::Unmatched("ELSE")),
            ("BEGIN SELECT 1 ELSE SELECT 2 END", Refusal::Unmatched("ELSE")),
            ("SELECT 1 END", Refusal::Unmatched("END")),
            ("BEGIN SELECT 1", Refusal::Unbalanced),
            ("SELECT CASE WHEN a = 1 THEN 2", Refusal::Unbalanced),
            ("SELECT (CASE WHEN a = 1 THEN 2)", Refusal::Unbalanced),
            ("SELECT (1", Refusal::Unbalanced),
            ("SELECT 1)", Refusal::Unbalanced),
            ("IF @n = 1", Refusal::Incomplete),
            ("IF @n = 1 SELECT 1 ELSE", Refusal::Incomplete),
            ("IF SELECT 1", Refusal::Incomplete),
            ("UPDATE t", Refusal::Incomplete),
            ("INSERT INTO t; SELECT 1", Refusal::Incomplete),
            ("WITH c AS (SELECT 1)", Refusal::Incomplete),
            ("MERGE t USING s ON t.id = s.id WHEN MATCHED THEN", Refusal::Incomplete),
            ("SELECT $$pg$$", Refusal::Symbol("$".into())),
            ("SELECT a ? b", Refusal::Symbol("?".into())),
            ("SELECT\u{a0}1", Refusal::Whitespace),
            ("SELECT\u{2028}1", Refusal::Whitespace),
            ("SELECT a, go FROM t", Refusal::ChangesTokens),
            ("-- comments only", Refusal::NoBatch),
        ];
        for (text, refusal) in refusals {
            assert_eq!(format(text).as_ref(), Err(refusal), "{text:?}");
        }
        for text in ["SELECT 'open", "SELECT 1\nGO 2", "SELECT 1 /* open", ":setvar x 1\nSELECT 1"] {
            assert!(matches!(format(text), Err(Refusal::Lex(_))), "{text:?}");
        }
        assert_eq!(format(&format!("SELECT '{}'", "x".repeat(65_536))), Err(Refusal::TooLarge));
    }

    fn respaced(text: &str, next: &mut impl FnMut() -> u64) -> String {
        const SPACES: &[&str] = &[" ", "  ", "\t", "\n", "\r\n", " \n  "];
        const NEWLINES: &[&str] = &["\n", "\r\n", " \n\t"];
        let pick = |options: &[&'static str], n: u64| options[(n % options.len() as u64) as usize];
        let mut out = String::new();
        let mut at = 0;
        for span in split_batches(text).unwrap() {
            out.push_str(&text[at..span.start]);
            let batch = span.text(text);
            let mut end = 0;
            let mut after_line_comment = false;
            for t in lex(batch).unwrap() {
                let gap = &batch[end..t.start];
                out.push_str(if gap.is_empty() || breaks(gap) >= 2 {
                    gap
                } else if after_line_comment {
                    pick(NEWLINES, next())
                } else {
                    pick(SPACES, next())
                });
                if t.kind == Kind::Word && reserved(t.text) {
                    out.extend(t.text.chars().map(|c| {
                        if next().is_multiple_of(2) { c.to_ascii_lowercase() } else { c.to_ascii_uppercase() }
                    }));
                } else {
                    out.push_str(t.text);
                }
                after_line_comment = t.kind == Kind::LineComment;
                end = t.end;
            }
            at = span.end;
        }
        out.push_str(&text[at..]);
        out
    }

    /// Respaces every corpus text and changes the case of its reserved words. Each version must format to the
    /// same tokens, and a text without comments to the same layout, since comments keep their own lines.
    #[test]
    fn any_spacing_or_keyword_case_formats_to_the_same_tokens_and_layout() {
        let mut next = random();
        let rounds = env_or("L1_DATA_FUZZ_ITERATIONS", 40usize);
        for &(text, expected) in groups().into_iter().flatten().chain([&MODULE_SCRIPT]) {
            let comments = lex(text).unwrap().iter().any(Token::comment);
            for _ in 0..rounds {
                let variant = respaced(text, &mut next);
                let formatted = format(&variant).unwrap_or_else(|e| panic!("{e:?} for {variant:?}"));
                assert_eq!(tokens_of(&formatted), tokens_of(&variant), "{variant:?}");
                if !comments {
                    assert_eq!(formatted, expected, "{variant:?}");
                }
            }
        }
    }

    /// Replaces, inserts, and deletes random tokens in the corpus, so the formatter sees near-valid batches it has
    /// no golden for. Whatever it accepts must keep every token and come back unchanged when formatted again.
    #[test]
    fn whatever_formats_keeps_every_token_and_formats_to_itself() {
        const EXTRA: &[&str] = &[
            "go", "\nGO\n", "--c\n", "/*c*/", "$", ";", "(", ")", ",", ".", "begin", "end", "else", "if", "case",
            "when", "then", "with", "as", "on", "select", "set", "matched", "output", "apply", "-", "'s'", "N'x'",
            "[a]]b]", "@v", "#t", "0x1", "1e3", "{{t}}", "::", "=", "||",
        ];
        let corpus: Vec<&str> = groups().into_iter().flatten().map(|(text, _)| *text).collect();
        let mut palette: Vec<String> = EXTRA.iter().map(|s| (*s).to_owned()).collect();
        for text in &corpus {
            palette.extend(lex(text).unwrap().iter().map(|t| t.text.to_owned()));
        }
        let mut next = random();
        let (mut accepted, mut tried) = (0, 0);
        let mutated = (0..env_or("L1_DATA_FUZZ_ITERATIONS", 4_000usize)).map(|_| {
            let tokens = lex(corpus[(next() % corpus.len() as u64) as usize]).unwrap();
            let mut parts: Vec<String> = tokens.iter().map(|t| t.text.to_owned()).collect();
            for _ in 0..=next() % 3 {
                let at = (next() % (parts.len() as u64 + 1)) as usize;
                let token = palette[(next() % palette.len() as u64) as usize].clone();
                match next() % 3 {
                    0 if at < parts.len() => parts[at] = token,
                    1 if at < parts.len() => drop(parts.remove(at)),
                    _ => parts.insert(at, token),
                }
            }
            parts.join(if next().is_multiple_of(2) { " " } else { "\n" })
        });
        for text in mutated.chain(texts(2_000)) {
            tried += 1;
            let Ok(formatted) = format(&text) else {
                continue;
            };
            accepted += 1;
            assert_eq!(tokens_of(&formatted), tokens_of(&text), "{text:?}");
            assert_eq!(format(&formatted).as_ref(), Ok(&formatted), "formatting twice: {text:?}");
        }
        assert!(accepted * 10 > tried, "only {accepted} of {tried} texts formatted");
    }

    #[test]
    fn reserved_words_are_sorted_for_binary_search() {
        assert!(RESERVED.is_sorted());
        assert!(reserved("select") && reserved("Try_Convert") && !reserved("matched") && !reserved("selec"));
    }
}
