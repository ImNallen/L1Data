use std::ops::Range;

use super::keywords::{RESERVED, TYPE_OR_FUNCTION_NAME};
use super::lex::{LexKind, lex};
use super::relations::{VETTED, VETTED_SIZES};
use super::split::split_statements;
use super::tsql;
use crate::model::{DatabaseKind, Expect, Qualifier, RelationSource, ScopeRelation, TableName};
use crate::text::quote_if_needed;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Class {
    Word,
    Name,
    Str,
    LineComment,
    BlockComment,
    Number,
    Param,
    Open,
    Close,
    Comma,
    Dot,
    Semi,
    Star,
    /// A notebook cell reference, `{{name}}`.
    Reference,
    Other,
}

#[derive(Debug, Clone)]
pub(super) struct Lexeme {
    pub class: Class,
    pub span: Range<usize>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Scope {
    pub from_byte: usize,
    pub expect: Expect,
    pub qualifier: Option<Qualifier>,
    pub relations: Vec<ScopeRelation>,
    pub ctes: Vec<String>,
    pub keywords: Vec<&'static str>,
    pub functions: Vec<&'static str>,
}

/// `None` inside a string, a comment, a quoted name, a number, or a parameter.
pub(crate) fn scope_at(text: &str, at: usize, kind: DatabaseKind) -> Option<Scope> {
    let lexemes = match kind {
        DatabaseKind::Postgres => postgres_statement(text, at),
        DatabaseKind::SqlServer => tsql_statement(text, at),
    };
    Walk::new(text, &lexemes, at, kind)?.run()
}

/// The statement `split_statements` puts the cursor in. A cursor past a statement's end belongs
/// to it until a `;`, so `FROM orders o JOIN customers c ON ` still reads as one statement.
fn postgres_statement(text: &str, at: usize) -> Vec<Lexeme> {
    let (start, end) = match split_statements(text).iter().rev().find(|s| s.start <= at) {
        Some(s) if at <= s.end => (s.start, s.end),
        Some(s) => {
            let gap = &text[s.end..at];
            let ws = gap.len() - gap.trim_start().len();
            if gap[ws..].starts_with(';') { (s.end + ws + 1, at) } else { (s.start, at) }
        }
        None => (0, at),
    };
    postgres_lexemes(text, start, end)
}

pub(super) fn map_lexemes(text: &str, kind: DatabaseKind) -> Vec<Lexeme> {
    match kind {
        DatabaseKind::Postgres => postgres_lexemes(text, 0, text.len()),
        DatabaseKind::SqlServer => tsql::completion_lexemes(text, 0..text.len()),
    }
}

fn postgres_lexemes(text: &str, start: usize, end: usize) -> Vec<Lexeme> {
    lex(&text[start..end])
        .into_iter()
        .filter_map(|l| {
            let class = match l.kind {
                LexKind::Space => return None,
                LexKind::LineComment => Class::LineComment,
                LexKind::BlockComment => Class::BlockComment,
                LexKind::Word => Class::Word,
                LexKind::QuotedIdent => Class::Name,
                LexKind::String | LexKind::DollarString => Class::Str,
                LexKind::Number => Class::Number,
                LexKind::Param => Class::Param,
                LexKind::Operator if &text[start + l.span.start..start + l.span.end] == "*" => Class::Star,
                LexKind::Dot => Class::Dot,
                LexKind::Comma => Class::Comma,
                LexKind::Open => Class::Open,
                LexKind::Close => Class::Close,
                LexKind::Semicolon => Class::Semi,
                LexKind::Reference => Class::Reference,
                LexKind::Operator | LexKind::OpenBracket | LexKind::CloseBracket | LexKind::Other => Class::Other,
            };
            Some(Lexeme { class, span: start + l.span.start..start + l.span.end })
        })
        .collect()
}

/// The statement around the cursor inside its `GO` batch.
fn tsql_statement(text: &str, at: usize) -> Vec<Lexeme> {
    tsql::completion_lexemes(text, tsql::statement_around(text, at))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Role {
    From,
    Join,
    Using,
    Into,
    Update,
    Table,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Slot {
    /// A relation comes next.
    Expect,
    /// Right after a relation, where an alias or the next clause may follow.
    Named,
    As,
    Aliased,
    /// After `LEFT`, `CROSS`, and the like, before `JOIN`.
    JoinWord,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Clause {
    Start,
    Cte,
    Select,
    Relation {
        role: Role,
        slot: Slot,
    },
    On {
        tree_start: usize,
        target: usize,
        fresh: bool,
    },
    Where,
    GroupBy,
    Having,
    OrderBy,
    Limit,
    Set,
    Values,
    Returning,
    /// Only these keywords can come next: `BY` after `GROUP`, `INTO` after `INSERT`.
    Next(&'static [&'static str]),
    Other,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Stage {
    From,
    Where,
    GroupBy,
    Having,
    OrderBy,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Origin {
    Statement,
    Derived,
    CteBody,
    CteColumns,
    ParenthesizedJoin,
    ColumnAliases,
    Other,
}

#[derive(Debug, Clone)]
struct Rel {
    written: Option<String>,
    /// The alias or name that a qualifier matches.
    folded: Option<String>,
    source: RelationSource,
    role: Role,
    column_aliases: Vec<String>,
}

type Output = (Vec<String>, Vec<TableName>);

#[derive(Debug, Clone)]
struct Cte {
    key: String,
    written: String,
    columns: Vec<String>,
    star: Vec<TableName>,
    aliases: Vec<String>,
}

enum Item {
    Named(String),
    Star(Option<String>),
    Unnamed,
}

struct Frame {
    parent: Option<usize>,
    query: bool,
    origin: Origin,
    clause: Clause,
    rels: Vec<Rel>,
    ctes: Vec<Cte>,
    items: Vec<Item>,
    /// By index into `Walk::sig`.
    item_lexemes: Vec<usize>,
    /// For a CTE's column list.
    group_names: Vec<String>,
    pending_cte: Option<(String, String, Option<Vec<String>>)>,
    recursive: bool,
    /// A set operator or an `INSERT`'s query after the cursor ends the part of the statement the cursor is in.
    sealed: bool,
    seen: bool,
    /// The first branch of a set operation names the output.
    first_output: Option<Output>,
    /// The enclosing frame's clause when this one opened.
    outer: Clause,
    /// For a `LATERAL` or `APPLY` subquery, the enclosing frame's FROM items it can see: those to its left, and for
    /// `APPLY` only those of its own join tree.
    lateral: Option<Range<usize>>,
    tree_start: usize,
    ctes_before: usize,
    joined: Option<usize>,
    /// A `MERGE`, whose `INTO` target its condition and actions can name.
    merge: bool,
    /// The FROM items are a `DELETE`'s target, which T-SQL cannot alias there.
    delete_target: bool,
    /// A T-SQL `DELETE` named its target before `FROM`, as in `DELETE o FROM orders o`, so the FROM items take aliases.
    delete_named: Option<String>,
    /// The statement is a `DELETE`, whose `OUTPUT` sees only `deleted`.
    deleting: bool,
    /// A `USING` or `NATURAL` join merges columns, so a `*` lists them in an order the reply cannot say. An unnamed
    /// column in front of the `*` keeps a column alias list from renaming by position.
    merged_join: bool,
    /// The `INTO` is a T-SQL `SELECT ... INTO`, so a FROM comes next.
    select_into: bool,
}

impl Frame {
    fn new(parent: Option<usize>, query: bool, origin: Origin, outer: Clause) -> Self {
        Frame {
            parent,
            query,
            origin,
            clause: if query { Clause::Start } else { Clause::Other },
            rels: Vec::new(),
            ctes: Vec::new(),
            items: Vec::new(),
            item_lexemes: Vec::new(),
            group_names: Vec::new(),
            pending_cte: None,
            recursive: false,
            sealed: false,
            seen: false,
            first_output: None,
            outer,
            lateral: None,
            tree_start: 0,
            ctes_before: 0,
            joined: None,
            merge: false,
            delete_target: false,
            delete_named: None,
            deleting: false,
            merged_join: false,
            select_into: false,
        }
    }
}

struct Snapshot {
    frame: usize,
    clause: Clause,
    seen: bool,
}

struct Walk<'a> {
    text: &'a str,
    kind: DatabaseKind,
    sig: Vec<&'a Lexeme>,
    from: usize,
    /// The first lexeme of the qualifier, or of the word being typed: where the cursor's context is read.
    context: usize,
    /// The qualifier and the word being typed, which the walk skips.
    typed: Range<usize>,
    parts: Vec<String>,
    /// The cursor's word starts its line after a lexeme a statement can end on, so it may begin the next statement.
    line_start: bool,
    frames: Vec<Frame>,
    stack: Vec<usize>,
    snapshot: Option<Snapshot>,
}

impl<'a> Walk<'a> {
    fn new(text: &'a str, lexemes: &'a [Lexeme], at: usize, kind: DatabaseKind) -> Option<Self> {
        for l in lexemes.iter().filter(|l| l.span.start < at && at <= l.span.end) {
            let inside = at < l.span.end;
            let open = || !closed(kind, &text[l.span.clone()]);
            let blocked = match l.class {
                Class::LineComment | Class::Number | Class::Param => true,
                Class::Str | Class::Name | Class::BlockComment => inside || open(),
                _ => false,
            };
            if blocked {
                return None;
            }
        }
        let sig: Vec<&Lexeme> =
            lexemes.iter().filter(|l| !matches!(l.class, Class::LineComment | Class::BlockComment)).collect();
        let typed = sig.iter().position(|l| l.class == Class::Word && l.span.start < at && at <= l.span.end);
        let (from, word_at) = match typed {
            Some(p) => (sig[p].span.start, p),
            None => (at, sig.partition_point(|l| l.span.end <= at)),
        };
        let mut context = word_at;
        let mut parts = Vec::new();
        while context >= 1 && sig[context - 1].class == Class::Dot {
            if context < 2 || !matches!(sig[context - 2].class, Class::Word | Class::Name) {
                return None;
            }
            parts.insert(0, fold(kind, text, sig[context - 2]));
            context -= 2;
        }
        let line = &text[text[..from].rfind('\n').map_or(0, |n| n + 1)..from];
        let ends_a_statement = context.checked_sub(1).is_some_and(|p| match sig[p].class {
            Class::Word => !CONTINUED.contains(&text[sig[p].span.clone()].to_ascii_uppercase().as_str()),
            class => matches!(class, Class::Name | Class::Close | Class::Number | Class::Str | Class::Star),
        });
        let line_start = parts.is_empty() && line.trim().is_empty() && ends_a_statement;
        let typed_span = context..word_at + usize::from(typed.is_some());
        Some(Walk {
            text,
            kind,
            sig,
            from,
            context,
            typed: typed_span,
            parts,
            line_start,
            frames: vec![Frame::new(None, true, Origin::Statement, Clause::Start)],
            stack: vec![0],
            snapshot: None,
        })
    }

    fn word(&self, i: usize) -> Option<String> {
        let l = self.sig.get(i)?;
        (l.class == Class::Word && !self.typed.contains(&i)).then(|| self.text[l.span.clone()].to_ascii_uppercase())
    }

    fn is_word(&self, i: usize, w: &str) -> bool {
        self.word(i).as_deref() == Some(w)
    }

    fn top(&mut self) -> &mut Frame {
        let id = *self.stack.last().expect("the statement frame is never popped");
        &mut self.frames[id]
    }

    fn run(mut self) -> Option<Scope> {
        let mut i = 0;
        while i < self.sig.len() {
            if i == self.context {
                self.take_snapshot();
            }
            if self.typed.contains(&i) {
                i += 1;
                continue;
            }
            if i < self.context {
                self.top().seen = true;
            }
            i = self.step(i);
        }
        if self.snapshot.is_none() {
            self.take_snapshot();
        }
        Some(self.finish())
    }

    fn take_snapshot(&mut self) {
        let frame = *self.stack.last().unwrap();
        let f = &self.frames[frame];
        self.snapshot = Some(Snapshot { frame, clause: f.clause, seen: f.seen });
    }

    fn step(&mut self, i: usize) -> usize {
        let class = self.sig[i].class;
        match class {
            Class::Open => {
                self.open(i);
                return i + 1;
            }
            Class::Close => {
                self.close(i);
                return i + 1;
            }
            _ => {}
        }
        let id = *self.stack.last().unwrap();
        if !self.frames[id].query {
            if matches!(class, Class::Word | Class::Name) {
                let name = fold(self.kind, self.text, self.sig[i]);
                self.frames[id].group_names.push(name);
            }
            return i + 1;
        }
        if let Clause::On { tree_start, target, fresh: true } = self.frames[id].clause {
            self.frames[id].clause = Clause::On { tree_start, target, fresh: false };
        }
        let qualified = i
            .checked_sub(1)
            .is_some_and(|p| self.sig[p].class == Class::Dot && self.sig[p].span.end == self.sig[i].span.start);
        if let Some(w) = self.word(i).filter(|_| !qualified)
            && let Some(next) = self.enter_clause(i, &w)
        {
            return next;
        }
        let clause = self.frames[id].clause;
        match (clause, class) {
            (Clause::Select, Class::Comma) => self.end_item(),
            (Clause::Select, _) => self.top().item_lexemes.push(i),
            (
                Clause::Relation { role: Role::From | Role::Join | Role::Using, .. } | Clause::On { .. },
                Class::Comma,
            ) => {
                let f = self.top();
                f.joined = None;
                f.tree_start = f.rels.len();
                f.clause = Clause::Relation { role: Role::From, slot: Slot::Expect };
            }
            (Clause::Relation { role, slot: Slot::Expect }, Class::Word | Class::Name) => {
                return self.relation(i, role);
            }
            (Clause::Relation { role, slot: Slot::Expect }, Class::Param) => {
                let written = self.text[self.sig[i].span.clone()].to_owned();
                let source = RelationSource::Derived { columns: Vec::new(), star: Vec::new() };
                self.push_rel(Rel {
                    folded: Some(written.clone()),
                    written: Some(written),
                    source,
                    role,
                    column_aliases: Vec::new(),
                });
                self.top().clause = Clause::Relation { role, slot: Slot::Named };
            }
            (Clause::Relation { slot: Slot::Named, .. }, Class::Word) if self.never_an_alias(i) => {
                self.top().clause = Clause::Other
            }
            (Clause::Relation { role, slot: Slot::Named | Slot::As }, Class::Word | Class::Name) => {
                let written = self.text[self.sig[i].span.clone()].to_owned();
                let folded = fold(self.kind, self.text, self.sig[i]);
                let f = self.top();
                if let Some(start) = f.joined.take() {
                    let (mut columns, mut star, mut known) = (Vec::new(), Vec::new(), true);
                    for rel in f.rels.drain(start.min(f.rels.len())..) {
                        known &= expand(&rel, &mut columns, &mut star);
                    }
                    let source = if known {
                        RelationSource::Derived { columns, star }
                    } else {
                        RelationSource::Derived { columns: Vec::new(), star: Vec::new() }
                    };
                    f.rels.push(Rel { written: None, folded: None, source, role, column_aliases: Vec::new() });
                }
                if !f.sealed
                    && let Some(rel) = f.rels.last_mut()
                {
                    rel.written = Some(written);
                    rel.folded = Some(folded);
                }
                f.clause = Clause::Relation { role, slot: Slot::Aliased };
            }
            (Clause::Cte, Class::Word) if self.is_word(i, "RECURSIVE") => self.top().recursive = true,
            (Clause::Next(&["FROM"]), Class::Word | Class::Name) => {
                let name = fold(self.kind, self.text, self.sig[i]);
                self.top().delete_named = Some(name);
            }
            (Clause::Cte, Class::Word | Class::Name)
                if !self.word(i).is_some_and(|w| ["AS", "NOT", "MATERIALIZED"].contains(&w.as_str())) =>
            {
                let written = self.text[self.sig[i].span.clone()].to_owned();
                let key = fold(self.kind, self.text, self.sig[i]);
                self.top().pending_cte = Some((key, written, None));
            }
            _ => {}
        }
        i + 1
    }

    fn enter_clause(&mut self, i: usize, w: &str) -> Option<usize> {
        let clause = self.top().clause;
        let prev = i.checked_sub(1).and_then(|p| self.word(p));
        let next_opens = self.sig.get(i + 1).is_some_and(|l| l.class == Class::Open) && !self.typed.contains(&(i + 1));
        let named = matches!(clause, Clause::Relation { slot: Slot::Named | Slot::Aliased, .. });
        let in_from =
            matches!(clause, Clause::Relation { role: Role::From | Role::Join | Role::Using, .. } | Clause::On { .. });
        let to = match w {
            "SELECT" => {
                if self.top().rels.iter().any(|r| r.role == Role::Into) {
                    self.seal_if_passed();
                }
                Clause::Select
            }
            _ if clause == Clause::Select && prev.as_deref() == Some("AS") => return None,
            "FROM" if prev.as_deref() == Some("DISTINCT") => return Some(i + 1),
            "FROM" => {
                let f = self.top();
                f.delete_target = clause == Clause::Next(&["FROM"]) && f.delete_named.is_none();
                Clause::Relation { role: Role::From, slot: Slot::Expect }
            }
            "JOIN" | "APPLY" => Clause::Relation { role: Role::Join, slot: Slot::Expect },
            "LEFT" | "RIGHT" | "FULL" | "INNER" | "CROSS" | "NATURAL" | "OUTER" if in_from && !next_opens => {
                self.top().merged_join |= w == "NATURAL";
                Clause::Relation { role: Role::Join, slot: Slot::JoinWord }
            }
            "ON" if clause == Clause::Select => return None,
            "GROUP" if prev.as_deref() == Some("WITHIN") => return None,
            "ON" => match clause {
                Clause::Relation { role: Role::From | Role::Join | Role::Using, slot: Slot::Named | Slot::Aliased }
                    if !self.top().rels.is_empty() =>
                {
                    Clause::On { tree_start: self.top().tree_start, target: self.top().rels.len() - 1, fresh: true }
                }
                _ => Clause::Other,
            },
            "USING" if matches!(clause, Clause::Relation { role: Role::Join, .. }) && named => {
                self.top().merged_join = true;
                Clause::Other
            }
            "USING" => Clause::Relation { role: Role::Using, slot: Slot::Expect },
            "WHERE" => Clause::Where,
            "GROUP" | "ORDER" => Clause::Next(&["BY"]),
            "BY" if prev.as_deref() == Some("GROUP") => Clause::GroupBy,
            "BY" if prev.as_deref() == Some("ORDER") => Clause::OrderBy,
            "HAVING" => Clause::Having,
            "LIMIT" | "OFFSET" | "FETCH" => Clause::Limit,
            "WINDOW" => Clause::Other,
            "UNION" | "INTERSECT" | "EXCEPT" => {
                if clause == Clause::Select {
                    self.end_item();
                }
                let id = *self.stack.last().unwrap();
                if self.frames[id].first_output.is_none() {
                    self.frames[id].first_output = Some(self.output(id));
                }
                self.leave_part(|_| true);
                Clause::Start
            }
            "RETURNING" | "OUTPUT" => Clause::Returning,
            "SET" => Clause::Set,
            "VALUES" => Clause::Values,
            "INTO" => {
                self.top().select_into = clause == Clause::Select;
                Clause::Relation { role: Role::Into, slot: Slot::Expect }
            }
            "UPDATE" if matches!(prev.as_deref(), Some("FOR" | "ON" | "KEY")) => Clause::Other,
            "UPDATE" => Clause::Relation { role: Role::Update, slot: Slot::Expect },
            "TABLE" => Clause::Relation { role: Role::Table, slot: Slot::Expect },
            "INSERT" => Clause::Next(&["INTO"]),
            "MERGE" => {
                self.top().merge = true;
                if self.is_word(i + 1, "INTO") {
                    Clause::Next(&["INTO"])
                } else {
                    Clause::Relation { role: Role::Into, slot: Slot::Expect }
                }
            }
            "DELETE" => {
                let f = self.top();
                f.delete_named = None;
                f.deleting = true;
                Clause::Next(&["FROM"])
            }
            "TOP"
                if matches!(
                    clause,
                    Clause::Relation { role: Role::Update, slot: Slot::Expect } | Clause::Next(&["FROM"])
                ) && next_opens =>
            {
                let close = self.matching_close(i + 1);
                return Some(if self.is_word(close + 1, "PERCENT") { close + 2 } else { close + 1 });
            }
            "WITH" if clause == Clause::Start => Clause::Cte,
            "WITH" if named && self.is_word(i + 1, "ORDINALITY") => return Some(i + 2),
            "WITH" if matches!(clause, Clause::Relation { role: Role::Into, .. }) && !next_opens => {
                self.seal_if_passed();
                Clause::Cte
            }
            "WITH" if named && next_opens => {
                let skip_to = self.matching_close(i + 1);
                return Some(skip_to + 1);
            }
            "AS" => {
                if let Clause::Relation { role, slot: Slot::Named } = clause {
                    self.top().clause = Clause::Relation { role, slot: Slot::As };
                    return Some(i + 1);
                }
                return None;
            }
            "LATERAL" | "ONLY" if matches!(clause, Clause::Relation { slot: Slot::Expect, .. }) => return Some(i + 1),
            "PIVOT" | "UNPIVOT"
                if self.kind == DatabaseKind::SqlServer
                    && (named || matches!(clause, Clause::On { .. }))
                    && next_opens =>
            {
                let f = self.top();
                f.joined = None;
                if !f.sealed && f.rels.len() > f.tree_start {
                    f.rels.truncate(f.tree_start);
                    f.rels.push(Rel {
                        written: None,
                        folded: None,
                        source: RelationSource::Derived { columns: Vec::new(), star: Vec::new() },
                        role: Role::From,
                        column_aliases: Vec::new(),
                    });
                }
                f.clause = Clause::Relation { role: Role::From, slot: Slot::Named };
                return Some(self.matching_close(i + 1) + 1);
            }
            _ => return None,
        };
        if clause == Clause::Select {
            self.end_item();
        }
        let f = self.top();
        f.joined = None;
        let merge_source =
            matches!(to, Clause::Relation { role: Role::Using, .. }) && f.rels.iter().any(|r| r.role == Role::Into);
        if matches!(to, Clause::Relation { role: Role::From | Role::Using, .. }) && !merge_source {
            f.tree_start = f.rels.len();
        }
        f.clause = to;
        Some(i + 1)
    }

    /// Past a set operator, or past an `INSERT` target into its query, the relations `leaving` picks are out of the
    /// cursor's sight. When the cursor came before, nothing after joins its part.
    fn leave_part(&mut self, leaving: impl Fn(&Rel) -> bool) {
        let cursor_passed = self.snapshot.is_some();
        let f = self.top();
        if cursor_passed {
            f.sealed = true;
        } else {
            f.rels.retain(|r| !leaving(r));
        }
    }

    /// Past an `INSERT` target into its query: when the cursor came before, nothing after joins its part. The target stays,
    /// and `out_of_sight` hides it wherever the query cannot name it.
    fn seal_if_passed(&mut self) {
        if self.snapshot.is_some() {
            self.top().sealed = true;
        }
    }

    fn push_rel(&mut self, rel: Rel) {
        let f = self.top();
        if !f.sealed {
            f.rels.push(rel);
        }
    }

    fn matching_close(&self, open: usize) -> usize {
        let mut depth = 0usize;
        for (j, l) in self.sig.iter().enumerate().skip(open) {
            match l.class {
                Class::Open => depth += 1,
                Class::Close => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return j;
                    }
                }
                _ => {}
            }
        }
        self.sig.len()
    }

    fn relation(&mut self, i: usize, role: Role) -> usize {
        let mut parts = vec![Some(i)];
        let mut j = i + 1;
        while j + 1 < self.sig.len()
            && self.sig[j].class == Class::Dot
            && !self.typed.contains(&j)
            && !self.typed.contains(&(j + 1))
        {
            match self.sig[j + 1].class {
                Class::Word | Class::Name => parts.push(Some(j + 1)),
                Class::Dot if self.kind == DatabaseKind::SqlServer => parts.push(None),
                _ => break,
            }
            j += if parts.last() == Some(&None) { 1 } else { 2 };
        }
        let last = parts.last().copied().flatten().unwrap_or(i);
        let written = self.text[self.sig[last].span.clone()].to_owned();
        let name = fold(self.kind, self.text, self.sig[last]);
        let schema = parts.len().checked_sub(2).and_then(|k| parts[k]).map(|k| fold(self.kind, self.text, self.sig[k]));
        let function = matches!(role, Role::From | Role::Join | Role::Using)
            && self.sig.get(j).is_some_and(|l| l.class == Class::Open)
            && !self.typed.contains(&j);
        let other_database = self.kind == DatabaseKind::SqlServer && parts.len() >= 3;
        let cte = if function || other_database || schema.is_some() { None } else { self.cte(&name) };
        let column_aliases = cte.as_ref().map(|c| c.aliases.clone()).unwrap_or_default();
        let source = if function || other_database {
            RelationSource::Derived { columns: Vec::new(), star: Vec::new() }
        } else if let Some(cte) = cte {
            RelationSource::Derived { columns: cte.columns, star: cte.star }
        } else {
            RelationSource::Table { schema, name: name.clone() }
        };
        self.top().joined = None;
        self.push_rel(Rel { written: Some(written), folded: Some(name), source, role, column_aliases });
        self.top().clause = Clause::Relation { role, slot: Slot::Named };
        j
    }

    /// A reserved word right after a relation, such as `FOR` or `OPTION`, that the server never reads as its alias.
    fn never_an_alias(&self, i: usize) -> bool {
        let word = &self.text[self.sig[i].span.clone()];
        match self.kind {
            DatabaseKind::Postgres => {
                let word = word.to_ascii_lowercase();
                RESERVED.contains(&word.as_str()) || TYPE_OR_FUNCTION_NAME.contains(&word.as_str())
            }
            DatabaseKind::SqlServer => tsql::RESERVED.iter().any(|r| r.eq_ignore_ascii_case(word)),
        }
    }

    fn cte(&self, name: &str) -> Option<Cte> {
        let mut child = None;
        let mut id = Some(*self.stack.last().unwrap());
        while let Some(f) = id {
            if let Some(c) = self.frames[f].ctes.iter().find(|c| same(self.kind, &c.key, name)) {
                return Some(c.clone());
            }
            if let Some(c) =
                child.and_then(|body| self.cte_naming_itself(f, body)).filter(|c| same(self.kind, &c.key, name))
            {
                return Some(c);
            }
            child = Some(f);
            id = self.frames[f].parent;
        }
        None
    }

    fn cte_naming_itself(&self, frame: usize, body: usize) -> Option<Cte> {
        let f = &self.frames[frame];
        let (key, written, explicit) = f.pending_cte.as_ref()?;
        if self.frames[body].origin != Origin::CteBody || !(f.recursive || self.kind == DatabaseKind::SqlServer) {
            return None;
        }
        let (columns, star) = self.frames[body].first_output.clone().unwrap_or_default();
        Some(Cte {
            key: key.clone(),
            written: written.clone(),
            columns,
            star,
            aliases: explicit.clone().unwrap_or_default(),
        })
    }

    fn open(&mut self, i: usize) {
        let parent = *self.stack.last().unwrap();
        let next = (i + 1..self.sig.len()).find(|j| !self.typed.contains(j));
        let first = next.and_then(|j| self.word(j));
        let clause = self.frames[parent].clause;
        let cte_body = clause == Clause::Cte
            && self.frames[parent].pending_cte.is_some()
            && i.checked_sub(1).is_some_and(|p| self.is_word(p, "AS") || self.is_word(p, "MATERIALIZED"));
        let query = matches!(first.as_deref(), Some("SELECT" | "WITH" | "VALUES"))
            || cte_body && matches!(first.as_deref(), Some("INSERT" | "UPDATE" | "DELETE" | "MERGE"));
        let names_a_relation =
            next.is_some_and(|j| matches!(self.sig[j].class, Class::Word | Class::Name | Class::Open));
        let renames = self.frames[parent]
            .rels
            .last()
            .is_some_and(|r| self.kind == DatabaseKind::Postgres || matches!(r.source, RelationSource::Derived { .. }));
        let origin = match clause {
            Clause::Relation { slot: Slot::Aliased, .. } if !query && renames => Origin::ColumnAliases,
            Clause::Relation { slot: Slot::Expect, .. } if query => Origin::Derived,
            Clause::Relation { role: role @ (Role::From | Role::Join | Role::Using), slot: Slot::Expect }
                if names_a_relation =>
            {
                self.frames[parent].clause = Clause::Relation { role, slot: Slot::Aliased };
                Origin::ParenthesizedJoin
            }
            Clause::Cte if cte_body => Origin::CteBody,
            Clause::Cte if self.frames[parent].pending_cte.is_some() => Origin::CteColumns,
            _ => Origin::Other,
        };
        if let Clause::Relation { role, slot: Slot::Expect } = clause
            && query
        {
            self.frames[parent].clause = Clause::Relation { role, slot: Slot::Named };
        }
        if self.frames[parent].clause == Clause::Select {
            self.frames[parent].item_lexemes.push(i);
        }
        let lateral = origin == Origin::Derived
            && i.checked_sub(1).is_some_and(|p| self.is_word(p, "LATERAL") || self.is_word(p, "APPLY"));
        let mut frame = Frame::new(Some(parent), query || origin == Origin::ParenthesizedJoin, origin, clause);
        if origin == Origin::ParenthesizedJoin {
            frame.clause = Clause::Relation { role: Role::From, slot: Slot::Expect };
        }
        let p = &self.frames[parent];
        let start = if self.kind == DatabaseKind::SqlServer { p.tree_start } else { 0 };
        frame.lateral = lateral.then_some(start..p.rels.len());
        frame.ctes_before = p.ctes.len();
        self.frames.push(frame);
        self.stack.push(self.frames.len() - 1);
    }

    fn close(&mut self, i: usize) {
        if self.stack.len() == 1 {
            return;
        }
        let id = self.stack.pop().unwrap();
        let parent = *self.stack.last().unwrap();
        if self.frames[id].query && self.frames[id].clause == Clause::Select {
            let saved = self.stack.clone();
            self.stack.push(id);
            self.end_item();
            self.stack = saved;
        }
        match self.frames[id].origin {
            Origin::Derived => {
                let (columns, star) = self.output(id);
                self.push_rel(Rel {
                    written: None,
                    folded: None,
                    source: RelationSource::Derived { columns, star },
                    role: Role::From,
                    column_aliases: Vec::new(),
                });
            }
            Origin::CteColumns => {
                let names = std::mem::take(&mut self.frames[id].group_names);
                if let Some(p) = self.frames[parent].pending_cte.as_mut() {
                    p.2 = Some(names);
                }
            }
            Origin::CteBody => {
                let (columns, star) = self.output(id);
                if let Some((key, written, explicit)) = self.frames[parent].pending_cte.take() {
                    self.frames[parent].ctes.push(Cte {
                        key,
                        written,
                        columns,
                        star,
                        aliases: explicit.unwrap_or_default(),
                    });
                }
            }
            Origin::ColumnAliases => {
                let names = std::mem::take(&mut self.frames[id].group_names);
                let p = &mut self.frames[parent];
                if let Some(rel) = p.rels.last_mut().filter(|_| !p.sealed) {
                    let kept = rel.column_aliases.iter().skip(names.len()).cloned().collect::<Vec<_>>();
                    rel.column_aliases = [names, kept].concat();
                }
            }
            Origin::ParenthesizedJoin => {
                let start = self.frames[parent].rels.len();
                for rel in self.frames[id].rels.clone() {
                    self.push_rel(rel);
                }
                let p = &mut self.frames[parent];
                if let Clause::Relation { role, .. } = p.clause {
                    p.clause = Clause::Relation { role, slot: Slot::Named };
                }
                p.joined = (!p.sealed).then_some(start);
            }
            Origin::Statement | Origin::Other => {}
        }
        if self.frames[parent].clause == Clause::Select {
            self.frames[parent].item_lexemes.push(i);
        }
    }

    fn end_item(&mut self) {
        let lexemes = std::mem::take(&mut self.top().item_lexemes);
        let mut lead = 0;
        loop {
            let opens = |k: usize| lexemes.get(k).is_some_and(|&i| self.sig[i].class == Class::Open);
            lead += match lexemes.get(lead).and_then(|&i| self.word(i)).as_deref() {
                Some("DISTINCT" | "ALL" | "PERCENT") => 1,
                Some("ON") if opens(lead + 1) => 3,
                Some("TOP") if opens(lead + 1) => 3,
                Some("TOP") => 2,
                Some("WITH") if lexemes.get(lead + 1).is_some_and(|&i| self.is_word(i, "TIES")) => 2,
                _ => break,
            };
        }
        if lead < lexemes.len() {
            let item = self.item(&lexemes[lead..]);
            self.top().items.push(item);
        }
    }

    /// The output column a select item names, as the server names it.
    /// Only a bare or qualified column and an alias name an output column exactly. Any other expression stays unnamed,
    /// since the server's name for it, such as `btrim` for `trim(...)`, is not in the text.
    fn item(&self, item: &[usize]) -> Item {
        let n = item.len();
        let class = |k: usize| self.sig[item[k]].class;
        let text = |k: usize| &self.text[self.sig[item[k]].span.clone()];
        let named = |k: usize| matches!(class(k), Class::Word | Class::Name);
        let name = |k: usize| Item::Named(fold(self.kind, self.text, self.sig[item[k]]));
        let pg = self.kind == DatabaseKind::Postgres;
        if let Some(k) = (0..n).rev().find(|&k| class(k) == Class::Other && text(k) == "::")
            && k + 1 + self.cast_type_len(&item[k + 1..]) == n
        {
            return self.item(&item[..k]);
        }
        match n {
            0 => Item::Unnamed,
            _ if !pg && n >= 3 && named(0) && text(1) == "=" => name(0),
            1 if class(0) == Class::Star => Item::Star(None),
            _ if class(n - 1) == Class::Star && n >= 3 && class(n - 2) == Class::Dot && named(n - 3) => {
                Item::Star(Some(fold(self.kind, self.text, self.sig[item[n - 3]])))
            }
            _ if named(n - 1)
                && self.word(item[n - 1]).is_some_and(|w| ["END", "NULL", "TRUE", "FALSE"].contains(&w.as_str())) =>
            {
                Item::Unnamed
            }
            _ if n % 2 == 1 && (0..n).all(|k| if k % 2 == 0 { named(k) } else { class(k) == Class::Dot }) => {
                name(n - 1)
            }
            _ if n >= 2 && named(n - 1) && self.is_word(item[n - 2], "AS") => name(n - 1),
            _ if n >= 2
                && named(n - 1)
                && !(class(n - 1) == Class::Word && self.never_an_alias(item[n - 1]))
                && self.simple_operand(&item[..n - 1]) =>
            {
                name(n - 1)
            }
            _ => Item::Unnamed,
        }
    }

    /// An operand a bare alias can follow without being read as part of it: a column, a call such as `count(*)`, a
    /// parenthesized expression, a literal, or one of these cast with `::`.
    fn simple_operand(&self, toks: &[usize]) -> bool {
        let class = |k: usize| self.sig[toks[k]].class;
        let n = toks.len();
        if let Some(k) =
            (0..n).rev().find(|&k| class(k) == Class::Other && &self.text[self.sig[toks[k]].span.clone()] == "::")
            && k + 1 + self.cast_type_len(&toks[k + 1..]) == n
        {
            return self.simple_operand(&toks[..k]);
        }
        let dotted = |m: usize| {
            m % 2 == 1
                && (0..m).all(|k| {
                    if k % 2 == 0 { matches!(class(k), Class::Word | Class::Name) } else { class(k) == Class::Dot }
                })
        };
        match n {
            0 => false,
            1 => {
                matches!(class(0), Class::Word | Class::Name | Class::Str | Class::Number)
                    && !(class(0) == Class::Word && self.never_an_alias(toks[0]))
            }
            2 => class(0) == Class::Open && class(1) == Class::Close,
            _ => dotted(n) || class(n - 2) == Class::Open && class(n - 1) == Class::Close && dotted(n - 2),
        }
    }

    fn cast_type_len(&self, tail: &[usize]) -> usize {
        let word = |j: usize| tail.get(j).and_then(|&i| self.word(i));
        let is = |j: usize, class: Class| tail.get(j).is_some_and(|&i| self.sig[i].class == class);
        let text = |j: usize| tail.get(j).map_or("", |&i| &self.text[self.sig[i].span.clone()]);
        if !is(0, Class::Word) && !is(0, Class::Name) {
            return 0;
        }
        let mut j = 1;
        while is(j, Class::Dot) && (is(j + 1, Class::Word) || is(j + 1, Class::Name)) {
            j += 2;
        }
        let first = word(j - 1).unwrap_or_default();
        let then = |j: usize, words: &[&str]| words.iter().enumerate().all(|(k, w)| word(j + k).as_deref() == Some(w));
        match first.as_str() {
            "DOUBLE" if then(j, &["PRECISION"]) => j += 1,
            "CHARACTER" | "CHAR" | "BIT" if then(j, &["VARYING"]) => j += 1,
            _ => {}
        }
        if is(j, Class::Open) && is(j + 1, Class::Close) {
            j += 2;
        }
        if matches!(first.as_str(), "TIME" | "TIMESTAMP")
            && (then(j, &["WITH", "TIME", "ZONE"]) || then(j, &["WITHOUT", "TIME", "ZONE"]))
        {
            j += 3;
        }
        while text(j) == "[" && text(j + 1) == "]" {
            j += 2;
        }
        j
    }

    fn output(&self, id: usize) -> Output {
        let f = &self.frames[id];
        if let Some(first) = &f.first_output {
            return first.clone();
        }
        let mut columns = Vec::new();
        let mut star = Vec::new();
        let mut known = true;
        for item in &f.items {
            match item {
                Item::Named(n) => columns.push(n.clone()),
                Item::Star(None) => {
                    if f.merged_join {
                        columns.push(String::new());
                    }
                    f.rels.iter().for_each(|r| known &= expand(r, &mut columns, &mut star));
                }
                Item::Star(Some(q)) => {
                    if let Some(r) = f.rels.iter().find(|r| r.folded.as_deref().is_some_and(|k| same(self.kind, k, q)))
                    {
                        known &= expand(r, &mut columns, &mut star);
                    }
                }
                Item::Unnamed => columns.push(String::new()),
            }
        }
        if known { (columns, star) } else { (Vec::new(), Vec::new()) }
    }

    fn finish(self) -> Scope {
        let snap = self.snapshot.as_ref().expect("run takes a snapshot");
        let mut chain = Vec::new();
        let mut id = Some(snap.frame);
        while let Some(f) = id {
            chain.push(f);
            id = self.frames[f].parent;
        }
        let mut rels: Vec<&Rel> = Vec::new();
        let mut child: Option<&Frame> = None;
        for &f in &chain {
            let frame_rels = &self.frames[f].rels;
            let clause = child.map_or(snap.clause, |c| c.outer);
            let visible = match (child.map(|c| (c.origin, c.lateral.clone())), clause) {
                (Some((Origin::Derived, Some(left))), _) => frame_rels.get(left).unwrap_or_default(),
                (Some((Origin::Derived | Origin::CteBody | Origin::ParenthesizedJoin, None)), _) => &[],
                (_, Clause::On { tree_start, target, .. }) => {
                    frame_rels.get(tree_start.min(target)..(target + 1).min(frame_rels.len())).unwrap_or_default()
                }
                _ => &frame_rels[..],
            };
            let shadowed = |r: &Rel| {
                child.is_some()
                    && r.folded.as_deref().is_some_and(|k| {
                        rels.iter().any(|inner| inner.folded.as_deref().is_some_and(|i| same(self.kind, i, k)))
                    })
            };
            let frame = &self.frames[f];
            let outer: Vec<&Rel> =
                visible.iter().filter(|r| !self.out_of_sight(r, frame, clause) && !shadowed(r)).collect();
            rels.extend(outer);
            child = Some(&self.frames[f]);
        }
        let column_list_clause = self.frames[snap.frame].outer;
        let forward: Vec<&Cte> =
            chain.iter().filter(|&&f| self.frames[f].recursive).flat_map(|&f| self.frames[f].ctes.iter()).collect();
        let defined_later = |t: &TableName| {
            t.schema.is_none().then(|| forward.iter().find(|c| same(self.kind, &c.key, &t.name))).flatten()
        };
        let quoted = |names: &[String]| names.iter().map(|n| quote_if_needed(self.kind, n)).collect::<Vec<_>>();
        let relations = rels
            .iter()
            .map(|r| {
                let mut rel = (*r).clone();
                if !renames_known_columns(&rel) && !rel.column_aliases.is_empty() {
                    rel.source = RelationSource::Derived { columns: Vec::new(), star: Vec::new() };
                    rel.column_aliases.clear();
                }
                if let RelationSource::Derived { columns, star } = &mut rel.source
                    && star.is_empty()
                    && !rel.column_aliases.is_empty()
                {
                    let aliases = std::mem::take(&mut rel.column_aliases);
                    *columns = aliases.iter().chain(columns.iter().skip(aliases.len())).cloned().collect();
                }
                if let RelationSource::Table { schema, name } = &rel.source
                    && let Some(cte) = defined_later(&TableName { schema: schema.clone(), name: name.clone() })
                {
                    rel.source = RelationSource::Derived { columns: cte.columns.clone(), star: cte.star.clone() };
                    if rel.column_aliases.is_empty() {
                        rel.column_aliases = cte.aliases.clone();
                    }
                }
                if let RelationSource::Derived { columns, star } = &mut rel.source {
                    let (mut more, mut tables) = (Vec::new(), Vec::new());
                    for t in std::mem::take(star) {
                        match defined_later(&t) {
                            Some(cte) => {
                                more.extend(cte.columns.iter().cloned());
                                tables.extend(cte.star.iter().cloned());
                            }
                            None => tables.push(t),
                        }
                    }
                    columns.extend(more);
                    *star = tables;
                }
                let in_column_list =
                    rel.role == Role::Into && matches!(column_list_clause, Clause::Relation { role: Role::Into, .. });
                ScopeRelation {
                    reference: if in_column_list { None } else { rel.written },
                    source: match rel.source {
                        RelationSource::Derived { columns, star } => {
                            let named: Vec<String> = columns.into_iter().filter(|c| !c.is_empty()).collect();
                            RelationSource::Derived { columns: quoted(&named), star }
                        }
                        table => table,
                    },
                    column_aliases: quoted(&rel.column_aliases),
                }
            })
            .collect();
        let mut ctes: Vec<String> = Vec::new();
        let cte_naming_itself = chain.windows(2).filter_map(|w| self.cte_naming_itself(w[1], w[0]));
        let defined = chain.iter().enumerate().flat_map(|(k, &f)| {
            let frame = &self.frames[f];
            let body = k.checked_sub(1).map(|c| &self.frames[chain[c]]).filter(|b| b.origin == Origin::CteBody);
            let seen = match body {
                Some(_) if frame.recursive => frame.ctes.len(),
                Some(b) if self.kind == DatabaseKind::SqlServer => b.ctes_before + 1,
                Some(b) => b.ctes_before,
                None => frame.ctes.len(),
            };
            let visible = &frame.ctes[..seen.min(frame.ctes.len())];
            visible.iter().cloned()
        });
        for c in cte_naming_itself.chain(defined) {
            if !ctes.contains(&c.written) {
                ctes.push(c.written);
            }
        }
        let (relations, qualifier) = match self.output_tables(snap) {
            Some(tables) => self.output_scope(tables),
            None => (relations, (!self.parts.is_empty()).then(|| self.qualifier(&rels))),
        };
        let frame = &self.frames[snap.frame];
        let (mut expect, mut keywords) =
            if frame.query { self.expect(snap, frame) } else { self.expect_in_group(snap, frame) };
        if let (Expect::JoinCondition { target }, Clause::On { tree_start, .. }) = (&mut expect, snap.clause) {
            let before = frame.rels.get(tree_start..*target as usize).unwrap_or_default();
            *target = before.iter().filter(|r| !self.out_of_sight(r, frame, snap.clause)).count() as u16;
        }
        if self.line_start && matches!(expect, Expect::Keyword | Expect::Alias | Expect::Expression) {
            keywords.extend(if self.kind == DatabaseKind::Postgres { PG_STATEMENTS } else { TSQL_STATEMENTS });
        }
        let functions = match expect {
            Expect::Expression | Expect::JoinCondition { .. } => match self.kind {
                DatabaseKind::Postgres => {
                    VETTED.iter().chain(VETTED_SIZES).copied().filter(|f| !PG_TYPE_CALLS.contains(f)).collect()
                }
                DatabaseKind::SqlServer => TSQL_FUNCTIONS.to_vec(),
            },
            Expect::Keyword | Expect::Alias | Expect::Relation => Vec::new(),
        };
        let mut seen = std::collections::HashSet::new();
        keywords.retain(|k| seen.insert(*k));
        Scope { from_byte: self.from, expect, qualifier, relations, ctes, keywords, functions }
    }

    /// In a T-SQL `OUTPUT`, the pseudo-tables it may name, `inserted` and `deleted`, with the table they mirror.
    fn output_tables(&self, snap: &Snapshot) -> Option<Vec<(&'static str, RelationSource)>> {
        let here = &self.frames[snap.frame];
        let (id, clause) = if here.query { (snap.frame, snap.clause) } else { (here.parent?, here.outer) };
        if self.kind != DatabaseKind::SqlServer || clause != Clause::Returning {
            return None;
        }
        let f = &self.frames[id];
        let named = |r: &&Rel| {
            let table = matches!(&r.source, RelationSource::Table { name, .. } if f.delete_named.as_deref().is_some_and(|n| same(self.kind, n, name)));
            r.role != Role::Update
                && (table
                    || r.folded.as_deref().zip(f.delete_named.as_deref()).is_some_and(|(a, b)| same(self.kind, a, b)))
        };
        let target = match &f.delete_named {
            Some(_) => f.rels.iter().find(|r| named(r)),
            None => f
                .rels
                .iter()
                .find(|r| matches!(r.role, Role::Into | Role::Update) || r.role == Role::From && f.delete_target),
        };
        let source = target.map(|t| {
            let stands_for = f.rels.iter().find(|o| {
                t.role == Role::Update && o.role != Role::Update && o.folded.is_some() && o.folded == t.folded
            });
            stands_for.unwrap_or(t).source.clone()
        });
        let names: &[&'static str] = match target.map(|t| t.role) {
            _ if f.deleting => &["deleted"],
            Some(Role::Into) if !f.merge => &["inserted"],
            _ => &["inserted", "deleted"],
        };
        let empty = RelationSource::Derived { columns: Vec::new(), star: Vec::new() };
        Some(names.iter().map(|&n| (n, source.clone().unwrap_or_else(|| empty.clone()))).collect())
    }

    /// `inserted.` and `deleted.` list the target's columns; elsewhere `OUTPUT` offers the two names and no bare column.
    fn output_scope(&self, tables: Vec<(&'static str, RelationSource)>) -> (Vec<ScopeRelation>, Option<Qualifier>) {
        let empty = RelationSource::Derived { columns: Vec::new(), star: Vec::new() };
        let relation = |name: &str, source: RelationSource| ScopeRelation {
            reference: Some(name.to_owned()),
            source,
            column_aliases: Vec::new(),
        };
        match self.parts.as_slice() {
            [] => (tables.iter().map(|(n, _)| relation(n, empty.clone())).collect(), None),
            [one] => match tables.into_iter().find(|(n, _)| n.eq_ignore_ascii_case(one)) {
                Some((n, source)) => (vec![relation(n, source)], Some(Qualifier::Relation { index: 0 })),
                None => (Vec::new(), Some(Qualifier::Name { parts: self.parts.clone() })),
            },
            _ => (Vec::new(), Some(Qualifier::Name { parts: self.parts.clone() })),
        }
    }

    /// A relation the statement names that the cursor's clause cannot refer to: an `INSERT`, `SELECT INTO`, or
    /// `CREATE TABLE AS` target outside its column list, and a T-SQL `UPDATE` target that a FROM item stands for.
    fn out_of_sight(&self, rel: &Rel, frame: &Frame, clause: Clause) -> bool {
        let names =
            |o: &Rel| rel.folded.as_deref().zip(o.folded.as_deref()).is_some_and(|(a, b)| same(self.kind, a, b));
        let same_table = |o: &Rel| match (&rel.source, &o.source) {
            (RelationSource::Table { schema: None, name }, RelationSource::Table { name: other, .. }) => {
                same(self.kind, name, other)
            }
            _ => false,
        };
        match rel.role {
            Role::Into => {
                !frame.merge && !matches!(clause, Clause::Relation { role: Role::Into, .. } | Clause::Returning)
            }
            Role::Table => !matches!(clause, Clause::Relation { role: Role::Table, .. }),
            Role::Update => {
                matches!(&rel.source, RelationSource::Table { schema: None, .. })
                    && frame.rels.iter().any(|o| {
                        o.role != Role::Update
                            && o.folded.is_some()
                            && (names(o) || self.kind == DatabaseKind::SqlServer && same_table(o))
                    })
            }
            Role::From | Role::Join | Role::Using => {
                clause == Clause::Returning && !frame.merge && frame.rels.iter().any(|o| o.role == Role::Into)
            }
        }
    }

    fn qualifier(&self, rels: &[&Rel]) -> Qualifier {
        let index = match self.parts.as_slice() {
            [one] => rels.iter().position(|r| r.folded.as_deref().is_some_and(|k| same(self.kind, k, one))),
            [schema, name] => rels.iter().position(|r| {
                r.written.is_some()
                    && matches!(&r.source, RelationSource::Table { schema: Some(s), name: n } if same(self.kind, s, schema) && same(self.kind, n, name))
                    && r.folded.as_deref().is_some_and(|k| same(self.kind, k, name))
            }),
            _ => None,
        };
        match index.and_then(|i| u16::try_from(i).ok()) {
            Some(index) => Qualifier::Relation { index },
            None => Qualifier::Name { parts: self.parts.clone() },
        }
    }

    fn expect(&self, snap: &Snapshot, frame: &Frame) -> (Expect, Vec<&'static str>) {
        let pg = self.kind == DatabaseKind::Postgres;
        let expression = |mut kw: Vec<&'static str>| {
            kw.extend(EXPRESSION_WORDS);
            kw.extend(if pg { PG_EXPRESSION_WORDS } else { TSQL_EXPRESSION_WORDS });
            (Expect::Expression, kw)
        };
        match snap.clause {
            Clause::Start if frame.origin == Origin::Statement && !snap.seen => {
                (Expect::Keyword, if pg { PG_STATEMENTS.to_vec() } else { TSQL_STATEMENTS.to_vec() })
            }
            Clause::Start => (Expect::Keyword, vec!["SELECT"]),
            Clause::Cte => {
                let after_body = frame.pending_cte.is_none() && !frame.ctes.is_empty();
                (Expect::Keyword, if after_body { MAIN_VERBS.to_vec() } else { vec!["AS"] })
            }
            Clause::Select => {
                let mut kw = vec!["FROM", "AS", "DISTINCT"];
                if !pg {
                    kw.extend(["TOP", "INTO"]);
                }
                expression(kw)
            }
            Clause::Relation { slot: Slot::Expect, .. } => (Expect::Relation, Vec::new()),
            Clause::Relation { slot: Slot::As, .. } => (Expect::Alias, Vec::new()),
            Clause::Relation { slot: Slot::JoinWord, .. } => (Expect::Keyword, vec!["JOIN", "OUTER JOIN"]),
            Clause::Relation { role, slot: slot @ (Slot::Named | Slot::Aliased) } => {
                let kw = match role {
                    Role::Join => [if pg { &["ON", "USING"][..] } else { &["ON"][..] }, &self.after_from()].concat(),
                    Role::From | Role::Using => self.after_from(),
                    Role::Into if frame.select_into => [&["FROM"][..], &self.clauses_after(Stage::From)].concat(),
                    Role::Into => vec!["VALUES", "SELECT", "DEFAULT VALUES"],
                    Role::Update => vec!["SET"],
                    Role::Table => Vec::new(),
                };
                let aliased = match role {
                    Role::From | Role::Join | Role::Using => {
                        !(self.kind == DatabaseKind::SqlServer && frame.delete_target)
                    }
                    Role::Update => pg,
                    Role::Into => frame.merge,
                    Role::Table => false,
                };
                let expect = if slot == Slot::Named && aliased { Expect::Alias } else { Expect::Keyword };
                (expect, kw)
            }
            Clause::On { target, fresh, .. } => {
                let (_, kw) = expression([&["AND", "OR"][..], &self.after_from()].concat());
                match u16::try_from(target) {
                    Ok(target) if fresh => (Expect::JoinCondition { target }, kw),
                    _ => (Expect::Expression, kw),
                }
            }
            Clause::Where => {
                let mut kw = vec!["AND", "OR", "NOT", "IN", "IS NULL", "IS NOT NULL", "LIKE", "BETWEEN", "EXISTS"];
                if pg {
                    kw.push("ILIKE");
                }
                kw.extend(self.clauses_after(Stage::Where));
                expression(kw)
            }
            Clause::GroupBy => expression(self.clauses_after(Stage::GroupBy)),
            Clause::Having => expression([&["AND", "OR"][..], &self.clauses_after(Stage::Having)].concat()),
            Clause::OrderBy => {
                let mut kw = vec!["ASC", "DESC"];
                if pg {
                    kw.extend(["NULLS FIRST", "NULLS LAST"]);
                }
                kw.extend(self.clauses_after(Stage::OrderBy));
                expression(kw)
            }
            Clause::Set => {
                expression(if pg { vec!["WHERE", "FROM", "RETURNING"] } else { vec!["WHERE", "FROM", "OUTPUT"] })
            }
            Clause::Values | Clause::Returning => expression(Vec::new()),
            Clause::Next(words) => (Expect::Keyword, words.to_vec()),
            Clause::Limit | Clause::Other => (Expect::Keyword, Vec::new()),
        }
    }

    fn expect_in_group(&self, snap: &Snapshot, frame: &Frame) -> (Expect, Vec<&'static str>) {
        let lead: Vec<&'static str> = if snap.seen { Vec::new() } else { vec!["SELECT"] };
        match (frame.origin, frame.outer) {
            (Origin::CteColumns, _) => (Expect::Keyword, Vec::new()),
            (_, Clause::Relation { slot: Slot::Expect, .. }) => (Expect::Relation, lead),
            (_, Clause::Cte) => (Expect::Keyword, lead),
            _ => {
                let mut kw = lead;
                kw.extend(EXPRESSION_WORDS);
                kw.extend(if self.kind == DatabaseKind::Postgres {
                    PG_EXPRESSION_WORDS
                } else {
                    TSQL_EXPRESSION_WORDS
                });
                (Expect::Expression, kw)
            }
        }
    }

    fn after_from(&self) -> Vec<&'static str> {
        let mut kw = vec!["JOIN", "LEFT JOIN", "INNER JOIN", "RIGHT JOIN", "FULL JOIN", "CROSS JOIN"];
        if self.kind == DatabaseKind::SqlServer {
            kw.extend(["CROSS APPLY", "OUTER APPLY"]);
        }
        kw.extend(self.clauses_after(Stage::From));
        kw
    }

    fn clauses_after(&self, stage: Stage) -> Vec<&'static str> {
        let pg = self.kind == DatabaseKind::Postgres;
        let mut kw: Vec<&'static str> = [
            (Stage::Where, "WHERE"),
            (Stage::GroupBy, "GROUP BY"),
            (Stage::Having, "HAVING"),
            (Stage::OrderBy, "ORDER BY"),
        ]
        .into_iter()
        .filter(|(s, _)| *s > stage)
        .map(|(_, k)| k)
        .collect();
        if pg {
            kw.extend(["LIMIT", "OFFSET"]);
        } else if stage == Stage::OrderBy {
            kw.push("OFFSET");
        }
        if stage < Stage::OrderBy {
            kw.extend(["UNION", "UNION ALL", "INTERSECT", "EXCEPT"]);
        }
        kw
    }
}

/// Adds what `rel` gives a `*`. False when a column alias list renames columns that only the catalog knows, which an
/// output of names plus whole tables cannot say, so the caller offers no columns rather than wrong ones.
fn expand(rel: &Rel, columns: &mut Vec<String>, star: &mut Vec<TableName>) -> bool {
    match &rel.source {
        RelationSource::Table { .. } | RelationSource::Derived { .. }
            if !rel.column_aliases.is_empty()
                && !matches!(&rel.source, RelationSource::Derived { star, .. } if star.is_empty()) =>
        {
            false
        }
        RelationSource::Table { schema, name } => {
            star.push(TableName { schema: schema.clone(), name: name.clone() });
            true
        }
        RelationSource::Derived { columns: c, star: s } => {
            columns.extend(rel.column_aliases.iter().chain(c.iter().skip(rel.column_aliases.len())).cloned());
            star.extend(s.iter().cloned());
            true
        }
    }
}

/// A column alias list renames by position, so it is right only over columns whose order is known: a derived output
/// of names alone, or of whole tables alone, which the catalog lists in order.
fn renames_known_columns(rel: &Rel) -> bool {
    match &rel.source {
        RelationSource::Derived { columns, star } => star.is_empty() || columns.is_empty(),
        RelationSource::Table { .. } => true,
    }
}

pub(super) fn closed(kind: DatabaseKind, lexeme: &str) -> bool {
    match kind {
        DatabaseKind::Postgres => lex(&format!("{lexeme} x")).first().is_some_and(|l| l.span.end == lexeme.len()),
        DatabaseKind::SqlServer => tsql::lexeme_closed(lexeme),
    }
}

/// A name as the server resolves it: PostgreSQL folds the ASCII letters of an unquoted name to lower
/// case, and a quoted name keeps its case with its doubled quotes undone.
fn fold(kind: DatabaseKind, text: &str, l: &Lexeme) -> String {
    let raw = &text[l.span.clone()];
    if l.class != Class::Name {
        return if kind == DatabaseKind::Postgres { raw.to_ascii_lowercase() } else { raw.to_owned() };
    }
    let raw = raw.strip_prefix("U&").or_else(|| raw.strip_prefix("u&")).unwrap_or(raw);
    let (close, doubled) = if raw.starts_with('[') { (']', "]]") } else { ('"', "\"\"") };
    let inner = &raw[1..];
    let inner = inner.strip_suffix(close).unwrap_or(inner);
    inner.replace(doubled, &doubled[..1])
}

fn same(kind: DatabaseKind, a: &str, b: &str) -> bool {
    match kind {
        DatabaseKind::Postgres => a == b,
        DatabaseKind::SqlServer => a.to_lowercase() == b.to_lowercase(),
    }
}

const PG_STATEMENTS: &[&str] = &[
    "SELECT",
    "WITH",
    "INSERT INTO",
    "UPDATE",
    "DELETE FROM",
    "VALUES",
    "TABLE",
    "EXPLAIN",
    "CREATE",
    "ALTER",
    "DROP",
    "TRUNCATE",
    "BEGIN",
    "COMMIT",
    "ROLLBACK",
    "SET",
    "SHOW",
    "GRANT",
    "REVOKE",
    "COPY",
    "VACUUM",
    "ANALYZE",
];

const TSQL_STATEMENTS: &[&str] = &[
    "SELECT",
    "WITH",
    "INSERT INTO",
    "UPDATE",
    "DELETE FROM",
    "MERGE INTO",
    "DECLARE",
    "SET",
    "EXEC",
    "IF",
    "BEGIN",
    "BEGIN TRANSACTION",
    "COMMIT",
    "ROLLBACK",
    "CREATE",
    "ALTER",
    "DROP",
    "TRUNCATE TABLE",
    "PRINT",
    "GRANT",
    "REVOKE",
];

/// Words a statement cannot end on.
const CONTINUED: &[&str] = &[
    "ALL", "AND", "AS", "BETWEEN", "BY", "CASE", "DISTINCT", "ELSE", "FROM", "HAVING", "IN", "INTO", "IS", "JOIN",
    "LIKE", "NOT", "ON", "OR", "SELECT", "SET", "THEN", "TOP", "UNION", "UPDATE", "USING", "VALUES", "WHEN", "WHERE",
    "WITH",
];

const MAIN_VERBS: &[&str] = &["SELECT", "INSERT INTO", "UPDATE", "DELETE FROM"];

const EXPRESSION_WORDS: &[&str] = &[
    "CASE", "WHEN", "THEN", "ELSE", "END", "AND", "OR", "NOT", "NULL", "IS", "IN", "LIKE", "BETWEEN", "EXISTS", "CAST",
    "COALESCE", "NULLIF", "DISTINCT", "AS",
];

const PG_EXPRESSION_WORDS: &[&str] =
    &["TRUE", "FALSE", "ILIKE", "GREATEST", "LEAST", "EXTRACT", "INTERVAL", "CURRENT_DATE", "CURRENT_TIMESTAMP"];

/// Built-ins named like a type, so a bare `time(x)` reads as a type and only a quoted `"time"(x)` calls them.
const PG_TYPE_CALLS: &[&str] = &["interval", "numeric", "time", "timestamp", "varchar"];

const TSQL_EXPRESSION_WORDS: &[&str] = &["CURRENT_TIMESTAMP"];

const TSQL_FUNCTIONS: &[&str] = &[
    "ABS",
    "ACOS",
    "APPROX_COUNT_DISTINCT",
    "ASCII",
    "ASIN",
    "ATAN",
    "ATN2",
    "AVG",
    "CEILING",
    "CHAR",
    "CHARINDEX",
    "CHECKSUM",
    "CHOOSE",
    "CONCAT",
    "CONCAT_WS",
    "CONVERT",
    "COS",
    "COT",
    "COUNT",
    "COUNT_BIG",
    "CUME_DIST",
    "DATALENGTH",
    "DATEADD",
    "DATEDIFF",
    "DATEDIFF_BIG",
    "DATEFROMPARTS",
    "DATENAME",
    "DATEPART",
    "DATETIME2FROMPARTS",
    "DATETRUNC",
    "DAY",
    "DB_NAME",
    "DEGREES",
    "DENSE_RANK",
    "DIFFERENCE",
    "EOMONTH",
    "ERROR_MESSAGE",
    "ERROR_NUMBER",
    "EXP",
    "FIRST_VALUE",
    "FLOOR",
    "FORMAT",
    "GETDATE",
    "GETUTCDATE",
    "GREATEST",
    "GROUPING",
    "HASHBYTES",
    "IIF",
    "ISDATE",
    "ISJSON",
    "ISNULL",
    "ISNUMERIC",
    "JSON_ARRAY",
    "JSON_MODIFY",
    "JSON_OBJECT",
    "JSON_QUERY",
    "JSON_VALUE",
    "LAG",
    "LAST_VALUE",
    "LEAD",
    "LEAST",
    "LEFT",
    "LEN",
    "LOG",
    "LOG10",
    "LOWER",
    "LTRIM",
    "MAX",
    "MIN",
    "MONTH",
    "NCHAR",
    "NEWID",
    "NTILE",
    "OBJECT_ID",
    "OBJECT_NAME",
    "PARSE",
    "PATINDEX",
    "PERCENT_RANK",
    "PERCENTILE_CONT",
    "PERCENTILE_DISC",
    "PI",
    "POWER",
    "QUOTENAME",
    "RADIANS",
    "RAND",
    "RANK",
    "REPLACE",
    "REPLICATE",
    "REVERSE",
    "RIGHT",
    "ROUND",
    "ROW_NUMBER",
    "RTRIM",
    "SCHEMA_NAME",
    "SCOPE_IDENTITY",
    "SIGN",
    "SIN",
    "SOUNDEX",
    "SPACE",
    "SQRT",
    "SQUARE",
    "STDEV",
    "STDEVP",
    "STR",
    "STRING_AGG",
    "STUFF",
    "SUBSTRING",
    "SUM",
    "SWITCHOFFSET",
    "SYSDATETIME",
    "SYSDATETIMEOFFSET",
    "SYSUTCDATETIME",
    "TAN",
    "TODATETIMEOFFSET",
    "TRANSLATE",
    "TRIM",
    "TRY_CAST",
    "TRY_CONVERT",
    "TRY_PARSE",
    "UNICODE",
    "UPPER",
    "USER_NAME",
    "VAR",
    "VARP",
    "YEAR",
];

#[cfg(test)]
mod tests {
    use super::super::keywords::NEVER_A_FUNCTION;
    use super::*;
    use DatabaseKind::{Postgres, SqlServer};

    fn at(kind: DatabaseKind, marked: &str) -> Option<Scope> {
        let cursor = marked.find('|').expect("a | marks the cursor");
        let text = marked.replacen('|', "", 1);
        scope_at(&text, cursor, kind)
    }

    fn pg(marked: &str) -> Scope {
        at(Postgres, marked).unwrap_or_else(|| panic!("{marked:?} offers nothing"))
    }

    fn ms(marked: &str) -> Scope {
        at(SqlServer, marked).unwrap_or_else(|| panic!("{marked:?} offers nothing"))
    }

    fn table(schema: Option<&str>, name: &str) -> RelationSource {
        RelationSource::Table { schema: schema.map(str::to_owned), name: name.to_owned() }
    }

    fn refs(scope: &Scope) -> Vec<(Option<&str>, &RelationSource)> {
        scope.relations.iter().map(|r| (r.reference.as_deref(), &r.source)).collect()
    }

    fn named_columns(scope: &Scope) -> Vec<(Option<&str>, Vec<String>)> {
        let names = |r: &ScopeRelation| match &r.source {
            RelationSource::Derived { columns, .. } => {
                r.column_aliases.iter().chain(columns.iter().skip(r.column_aliases.len())).cloned().collect()
            }
            RelationSource::Table { .. } => r.column_aliases.clone(),
        };
        scope.relations.iter().map(|r| (r.reference.as_deref(), names(r))).collect()
    }

    #[test]
    fn each_clause_expects_what_can_stand_there() {
        let cases: &[(&str, Expect)] = &[
            ("|", Expect::Keyword),
            ("sel|", Expect::Keyword),
            ("select |", Expect::Expression),
            ("select a, |", Expect::Expression),
            ("select * from |", Expect::Relation),
            ("select * from orders, |", Expect::Relation),
            ("select * from orders o join |", Expect::Relation),
            ("select * from orders o |", Expect::Keyword),
            ("select * from orders o left |", Expect::Keyword),
            ("select * from orders o join customers c on |", Expect::JoinCondition { target: 1 }),
            ("select * from orders o join customers c on c.id = |", Expect::Expression),
            ("select * from orders where |", Expect::Expression),
            ("select * from orders where a = 1 and |", Expect::Expression),
            ("select * from orders group |", Expect::Keyword),
            ("select * from orders group by |", Expect::Expression),
            ("select status from orders group by status having |", Expect::Expression),
            ("select * from orders order by |", Expect::Expression),
            ("insert into |", Expect::Relation),
            ("insert |", Expect::Keyword),
            ("update |", Expect::Relation),
            ("update orders set |", Expect::Expression),
            ("delete from orders where |", Expect::Expression),
            ("select * from orders as |", Expect::Alias),
        ];
        for (marked, expect) in cases {
            assert_eq!(pg(marked).expect, *expect, "{marked:?}");
        }
    }

    #[test]
    fn keywords_name_the_clauses_that_can_follow() {
        assert_eq!(pg("|").keywords[..3], ["SELECT", "WITH", "INSERT INTO"]);
        assert_eq!(pg("select * from orders group |").keywords, ["BY"]);
        assert_eq!(pg("insert |").keywords, ["INTO"]);
        assert_eq!(pg("select * from orders o left |").keywords, ["JOIN", "OUTER JOIN"]);
        assert!(pg("select * from |").keywords.is_empty());
        let after_join = pg("select * from orders o join customers c |").keywords;
        assert_eq!(after_join[..2], ["ON", "USING"]);
        assert!(after_join.contains(&"WHERE") && after_join.contains(&"LIMIT"));
        let after_from = pg("select * from orders o |").keywords;
        assert!(!after_from.contains(&"ON"));
        assert!(after_from.contains(&"LEFT JOIN") && after_from.contains(&"GROUP BY"));
        let after_where = pg("select * from orders where a = 1 |").keywords;
        assert!(after_where.contains(&"AND") && after_where.contains(&"ORDER BY") && !after_where.contains(&"WHERE"));
        let after_order = pg("select * from orders order by id |").keywords;
        assert!(
            after_order.contains(&"DESC")
                && after_order.contains(&"LIMIT")
                && !after_order.contains(&"GROUP BY")
                && !after_order.contains(&"UNION")
        );
        assert_eq!(pg("update orders |").keywords, ["SET"]);
    }

    #[test]
    fn functions_come_only_where_an_expression_can_stand() {
        assert!(pg("select date_|").functions.contains(&"date_trunc"));
        assert!(pg("select * from orders where |").functions.contains(&"date_part"));
        assert!(pg("select * from |").functions.is_empty());
        assert!(pg("|").functions.is_empty());
        assert!(ms("SELECT DATE|").functions.contains(&"DATEADD"));
    }

    #[test]
    fn every_offered_built_in_is_called_by_its_bare_name() {
        let offered = pg("select |").functions;
        assert!(!offered.iter().any(|f| PG_TYPE_CALLS.contains(f)));
        let keywords: Vec<_> = offered.iter().filter(|f| NEVER_A_FUNCTION.contains(f)).collect();
        assert_eq!(
            keywords,
            [&"json_object"],
            "a keyword-named built-in needs quoting unless it has its own call syntax"
        );
    }

    #[test]
    fn the_word_being_typed_starts_the_replaced_range() {
        assert_eq!(pg("select * from ord|").from_byte, 14);
        assert_eq!(pg("select * from |").from_byte, 14);
        assert_eq!(pg("select o.cu| from orders o").from_byte, 9);
        assert_eq!(pg("select * from ord|ers").from_byte, 14);
    }

    #[test]
    fn relations_carry_their_alias_as_written_and_their_folded_name() {
        let s = pg("SELECT * FROM Orders O JOIN public.\"Line Items\" AS li ON |");
        assert_eq!(refs(&s), [(Some("O"), &table(None, "orders")), (Some("li"), &table(Some("public"), "Line Items"))]);
        assert_eq!(s.expect, Expect::JoinCondition { target: 1 });
        let s = pg("select * from orders, lab.type_zoo where |");
        assert_eq!(
            refs(&s),
            [(Some("orders"), &table(None, "orders")), (Some("type_zoo"), &table(Some("lab"), "type_zoo"))]
        );
    }

    #[test]
    fn relations_after_the_cursor_are_in_scope() {
        let s = pg("select o.| from orders o join customers c on c.id = o.customer_id");
        assert_eq!(refs(&s), [(Some("o"), &table(None, "orders")), (Some("c"), &table(None, "customers"))]);
        assert_eq!(s.qualifier, Some(Qualifier::Relation { index: 0 }));
        assert_eq!(s.expect, Expect::Expression);
    }

    #[test]
    fn a_qualifier_names_a_relation_in_scope_or_falls_back_to_its_names() {
        assert_eq!(pg("select * from orders o where o.|").qualifier, Some(Qualifier::Relation { index: 0 }));
        assert_eq!(pg("select * from orders o where O.st|").qualifier, Some(Qualifier::Relation { index: 0 }));
        assert_eq!(pg("select * from orders where orders.|").qualifier, Some(Qualifier::Relation { index: 0 }));
        assert_eq!(
            pg("select * from public.orders where public.orders.|").qualifier,
            Some(Qualifier::Relation { index: 0 })
        );
        assert_eq!(
            pg("select * from orders o where orders.|").qualifier,
            Some(Qualifier::Name { parts: vec!["orders".into()] })
        );
        let s = pg("select * from Lab.|");
        assert_eq!((s.expect, s.qualifier), (Expect::Relation, Some(Qualifier::Name { parts: vec!["lab".into()] })));
        assert_eq!(pg("select * from \"Lab\".|").qualifier, Some(Qualifier::Name { parts: vec!["Lab".into()] }));
        assert!(refs(&pg("select * from lab.|")).is_empty());
    }

    #[test]
    fn a_cte_is_a_relation_with_the_columns_its_body_names() {
        let s = pg("with recent as (select * from orders) select * from rec|");
        assert_eq!((s.expect, s.ctes.clone()), (Expect::Relation, vec!["recent".to_owned()]));
        let s = pg(
            "with recent as (select * from orders), totals(cust, n) as (select customer_id, count(*) from orders group by 1) select r.| from recent r, totals t",
        );
        assert_eq!(
            refs(&s)[0],
            (
                Some("r"),
                &RelationSource::Derived {
                    columns: vec![],
                    star: vec![TableName { schema: None, name: "orders".into() }]
                }
            )
        );
        assert_eq!(named_columns(&s)[1], (Some("t"), vec!["cust".to_owned(), "n".to_owned()]));
        assert_eq!(s.ctes, ["recent", "totals"]);
        let s = pg("with x(a) as (select id, total from orders) select x.| from x");
        assert_eq!(named_columns(&s), [(Some("x"), vec!["a".to_owned(), "total".to_owned()])]);
        let s = pg("with x(a, b) as (select * from orders) select x.| from x");
        assert_eq!(
            (s.relations[0].column_aliases.clone(), &s.relations[0].source),
            (
                vec!["a".to_owned(), "b".to_owned()],
                &RelationSource::Derived {
                    columns: vec![],
                    star: vec![TableName { schema: None, name: "orders".into() }]
                }
            )
        );
        assert_eq!(
            named_columns(&pg("with x(a, b) as (select 1, 2) select * from x y(c) where |")),
            [(Some("y"), vec!["c".to_owned(), "b".to_owned()])]
        );
        let s = pg("with x as (select o.id, o.total as amount, count(*) n from orders o) select * from x where |");
        assert_eq!(
            refs(&s),
            [(
                Some("x"),
                &RelationSource::Derived { columns: vec!["id".into(), "amount".into(), "n".into()], star: vec![] }
            )]
        );
        assert_eq!(pg("with recent as (select 1) |").keywords, MAIN_VERBS);
    }

    #[test]
    fn a_recursive_cte_refers_to_itself_inside_its_body() {
        for s in [
            pg("with recursive t as (select 1 as n union all select t.n + 1 from t where t.|) select * from t"),
            pg("with recursive t(n) as (select 1 union all select n + 1 from t where t.|) select * from t"),
            ms("WITH t(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM t WHERE t.|) SELECT * FROM t"),
        ] {
            assert_eq!(
                (named_columns(&s), s.qualifier.clone(), s.ctes.clone()),
                (vec![(Some("t"), vec!["n".to_owned()])], Some(Qualifier::Relation { index: 0 }), vec!["t".to_owned()])
            );
        }
        let s = pg("with recursive tree as (select 1 as n union all select n + 1 from tr|");
        assert_eq!((s.expect, s.ctes), (Expect::Relation, vec!["tree".to_owned()]));
        assert_eq!(pg("with a as (select 1), b as (select * from |), c as (select 1) select 1").ctes, ["a"]);
        assert_eq!(
            pg("with recursive a as (select 1), b as (select * from |), c as (select 1) select 1").ctes,
            ["a", "b", "c"]
        );
        let s = pg("with recursive a as (select * from b), b as (select id from orders) select a.| from a");
        assert_eq!(refs(&s), [(Some("a"), &RelationSource::Derived { columns: vec!["id".into()], star: vec![] })]);
        let s = pg("with recursive a as (select 1), b as (select id from orders) select x.| from b x");
        assert_eq!(refs(&s), [(Some("x"), &RelationSource::Derived { columns: vec!["id".into()], star: vec![] })]);
        let s = pg("with a as (select * from b), b as (select id from orders) select a.| from a");
        assert_eq!(
            refs(&s),
            [(
                Some("a"),
                &RelationSource::Derived { columns: vec![], star: vec![TableName { schema: None, name: "b".into() }] }
            )]
        );
        let s = pg("with t as (select 1 as n union all select n + 1 from t where t.|) select * from t");
        assert_eq!((refs(&s), s.ctes.is_empty()), (vec![(Some("t"), &table(None, "t"))], true));
    }

    #[test]
    fn a_subquery_sees_its_own_relations_then_the_enclosing_ones() {
        let s = pg("select * from orders o where exists (select 1 from customers c where c.id = o.|)");
        assert_eq!(refs(&s), [(Some("c"), &table(None, "customers")), (Some("o"), &table(None, "orders"))]);
        assert_eq!(s.qualifier, Some(Qualifier::Relation { index: 1 }));
        let s = pg("select t.| from (select id, total from orders) t");
        assert_eq!(
            refs(&s),
            [(Some("t"), &RelationSource::Derived { columns: vec!["id".into(), "total".into()], star: vec![] })]
        );
        assert_eq!(s.qualifier, Some(Qualifier::Relation { index: 0 }));
        let s = pg("select * from orders o join (select * from customers c where c.|) x on true");
        assert_eq!(refs(&s)[0], (Some("c"), &table(None, "customers")));
        assert_eq!(pg("select * from (|").expect, Expect::Relation);
        assert_eq!(pg("select count(|) from orders").expect, Expect::Expression);
        assert_eq!(refs(&pg("insert into orders (|")), [(None, &table(None, "orders"))]);
    }

    #[test]
    fn a_lateral_subquery_sees_the_from_items_to_its_left() {
        let (customers, orders) = (table(None, "customers"), table(None, "orders"));
        let both = vec![(Some("c"), &customers), (Some("o"), &orders)];
        for s in [
            pg("select * from orders o, lateral (select * from customers c where c.id = o.|) x"),
            pg("select * from orders o join lateral (select * from customers c where c.id = o.|) x on true"),
            pg("select * from orders o, lateral (select * from customers c where c.id = o.|) x, products p"),
            ms("SELECT * FROM orders o CROSS APPLY (SELECT * FROM customers c WHERE c.id = o.|) x"),
            ms("SELECT * FROM orders o OUTER APPLY (SELECT * FROM customers c WHERE c.id = o.|) x"),
        ] {
            assert_eq!((refs(&s), s.qualifier.clone()), (both.clone(), Some(Qualifier::Relation { index: 1 })));
        }
        assert_eq!(
            refs(&pg("select * from a, orders o join lateral (select * from customers c where c.id = o.|) x on true"))
                .len(),
            3
        );
        assert_eq!(
            refs(&ms("SELECT * FROM a, orders o CROSS APPLY (SELECT * FROM customers c WHERE c.id = o.|) x")),
            both
        );
        let s = pg("select * from orders o, (select * from customers c where c.id = o.|) x");
        assert_eq!(
            (refs(&s), s.qualifier.clone()),
            (vec![(Some("c"), &table(None, "customers"))], Some(Qualifier::Name { parts: vec!["o".into()] }))
        );
    }

    #[test]
    fn a_from_subquery_inside_a_correlated_subquery_sees_the_outer_query() {
        let s =
            pg("select * from orders o where exists (select * from (select * from customers c where c.id = o.|) d)");
        assert_eq!(
            (refs(&s), s.qualifier.clone()),
            (
                vec![(Some("c"), &table(None, "customers")), (Some("o"), &table(None, "orders"))],
                Some(Qualifier::Relation { index: 1 })
            )
        );
        let s = pg(
            "select * from orders o where exists (select * from products p, (select * from customers c where c.|) d)",
        );
        assert_eq!(refs(&s), [(Some("c"), &table(None, "customers")), (Some("o"), &table(None, "orders"))]);
    }

    #[test]
    fn a_set_operation_scopes_each_part_to_its_own_relations() {
        assert_eq!(refs(&pg("select a from t1 union select b from t2 where t2.|")), [(Some("t2"), &table(None, "t2"))]);
        assert_eq!(refs(&pg("select t1.| from t1 union select b from t2")), [(Some("t1"), &table(None, "t1"))]);
    }

    #[test]
    fn nothing_is_offered_inside_strings_comments_quoted_names_or_numbers() {
        for marked in [
            "select 'ab|c' from orders",
            "select 'abc|",
            "select 'it''s|",
            "select * from orders -- note|",
            "select * from orders /* note| */",
            "select * from orders /* open|",
            "select $$ body|",
            "select \"Col|",
            "select 12|",
            "select $1|",
            "-- just a comment|",
            "select 1; -- after|",
        ] {
            assert!(at(Postgres, marked).is_none(), "{marked:?}");
        }
        assert!(at(Postgres, "select 'abc'|").is_some());
        assert!(at(Postgres, "select * from orders /* x */ |").is_some());
    }

    #[test]
    fn the_statement_under_the_cursor_is_the_one_read() {
        let s = pg("select * from customers;\nselect * from orders o where o.|");
        assert_eq!(refs(&s), [(Some("o"), &table(None, "orders"))]);
        let s = pg("select * from orders o where o.|;\nselect * from customers c");
        assert_eq!(refs(&s), [(Some("o"), &table(None, "orders"))]);
        let s = pg("select * from orders;\n|");
        assert_eq!((s.expect, s.relations.len()), (Expect::Keyword, 0));
        let s = pg("select * from orders o join customers c on\n  |");
        assert_eq!(s.expect, Expect::JoinCondition { target: 1 });
    }

    #[test]
    fn tsql_reads_brackets_and_splits_statements_without_semicolons() {
        let s = ms("SELECT * FROM dbo.[Order Lines] AS ol JOIN Sales.Customers c ON |");
        assert_eq!(
            refs(&s),
            [(Some("ol"), &table(Some("dbo"), "Order Lines")), (Some("c"), &table(Some("Sales"), "Customers"))]
        );
        assert_eq!(s.expect, Expect::JoinCondition { target: 1 });
        let s = ms(
            "DECLARE @n int = 1\nSELECT * FROM customers c\nSELECT * FROM orders o WHERE o.|\nUPDATE orders SET total = 0",
        );
        assert_eq!(refs(&s), [(Some("o"), &table(None, "orders"))]);
        assert_eq!(s.qualifier, Some(Qualifier::Relation { index: 0 }));
        let s = ms("SELECT * FROM Orders O WHERE o.|");
        assert_eq!(s.qualifier, Some(Qualifier::Relation { index: 0 }));
        let s = ms("SELECT * FROM customers\nGO\nSELECT * FROM orders WHERE |");
        assert_eq!(refs(&s), [(Some("orders"), &table(None, "orders"))]);
        let s = ms("INSERT INTO archive SELECT * FROM orders o WHERE o.|");
        assert_eq!(refs(&s), [(Some("o"), &table(None, "orders"))]);
        let s = ms("WITH r AS (SELECT * FROM orders) SELECT * FROM r WHERE r.|");
        assert_eq!(s.qualifier, Some(Qualifier::Relation { index: 0 }));
        let s = ms("SELECT * FROM orders o WITH (NOLOCK) JOIN customers c ON |");
        assert_eq!(s.expect, Expect::JoinCondition { target: 1 });
        assert!(ms("SELECT * FROM orders o |").keywords.contains(&"CROSS APPLY"));
        assert!(!ms("SELECT * FROM orders o |").keywords.contains(&"LIMIT"));
        assert!(ms("SELECT |").keywords.contains(&"TOP"));
    }

    #[test]
    fn an_alias_in_a_later_union_branch_leaves_the_cursors_branch_alone() {
        let s = pg("select o.| from orders o union all select 1 from customers c");
        assert_eq!(
            (refs(&s), s.qualifier.clone()),
            (vec![(Some("o"), &table(None, "orders"))], Some(Qualifier::Relation { index: 0 }))
        );
        let s = ms("SELECT c.| FROM orders c UNION ALL SELECT o.id FROM customers o");
        assert_eq!(refs(&s), [(Some("c"), &table(None, "orders"))]);
    }

    #[test]
    fn only_the_relations_the_server_lets_the_cursor_see_are_in_scope() {
        assert_eq!(
            refs(&pg("with x as (select | from orders) select * from customers c")),
            [(Some("orders"), &table(None, "orders"))]
        );
        assert_eq!(
            refs(&pg("select * from orders o join (select * from customers c where c.|) x on true")),
            [(Some("c"), &table(None, "customers"))]
        );
        let s = pg("select * from orders o join customers c on | join addresses a on a.customer_id = c.id");
        assert_eq!(refs(&s), [(Some("o"), &table(None, "orders")), (Some("c"), &table(None, "customers"))]);
        let s = pg("insert into archive select * from orders o join customers c on |");
        assert_eq!((refs(&s).len(), s.expect), (2, Expect::JoinCondition { target: 1 }));
        assert_eq!(refs(&pg("insert into orders (id, |) select 1 from customers")), [(None, &table(None, "orders"))]);
        let s = ms("INSERT INTO archive SELECT * FROM orders o\nSELECT * FROM customers c WHERE c.|");
        assert_eq!(refs(&s), [(Some("c"), &table(None, "customers"))]);
    }

    #[test]
    fn a_join_condition_sees_only_its_own_join_tree() {
        let (o, c) = (table(None, "orders"), table(None, "customers"));
        for s in [
            pg("select * from a, orders o join customers c on |"),
            ms("SELECT * FROM a, orders o JOIN customers c ON |"),
        ] {
            assert_eq!(
                (refs(&s), s.expect),
                (vec![(Some("o"), &o), (Some("c"), &c)], Expect::JoinCondition { target: 1 })
            );
        }
        let s = pg("select * from orders o join customers c on c.id = o.customer_id, products p join reviews r on |");
        assert_eq!(
            (refs(&s), s.expect),
            (
                vec![(Some("p"), &table(None, "products")), (Some("r"), &table(None, "reviews"))],
                Expect::JoinCondition { target: 1 }
            )
        );
        let s = pg("select * from orders o join customers c on true, products p where |");
        assert_eq!(refs(&s).len(), 3);
        for s in [
            pg("select * from a, orders o join customers c on (|) join addresses d on true"),
            pg("select * from a, orders o join customers c on c.id in (select 1 where |) join addresses d on true"),
        ] {
            let names: Vec<_> = refs(&s).iter().map(|r| r.0).collect();
            assert!(
                names.ends_with(&[Some("o"), Some("c")]) && !names.contains(&Some("a")) && !names.contains(&Some("d")),
                "{names:?}"
            );
        }
        let s = ms("UPDATE o SET o.x = 1 FROM orders o JOIN customers c ON |");
        assert_eq!((refs(&s), s.expect), (vec![(Some("o"), &o), (Some("c"), &c)], Expect::JoinCondition { target: 1 }));
        for s in [
            pg("update orders t set status = 'x' from customers a join products p on |"),
            pg("delete from orders t using customers a join products p on |"),
        ] {
            assert_eq!(
                (refs(&s).iter().map(|r| r.0).collect::<Vec<_>>(), s.expect),
                (vec![Some("a"), Some("p")], Expect::JoinCondition { target: 1 })
            );
        }
        let s = ms("MERGE INTO orders t USING customers s ON |");
        assert_eq!((refs(&s), s.expect), (vec![(Some("t"), &o), (Some("s"), &c)], Expect::JoinCondition { target: 1 }));
    }

    #[test]
    fn a_parenthesized_join_adds_its_relations_to_the_query() {
        let (o, c, a) = (table(None, "orders"), table(None, "customers"), table(None, "addresses"));
        for s in [
            pg("select * from (orders o join customers c on |)"),
            ms("SELECT * FROM (orders o JOIN customers c ON |)"),
            pg("select * from products p, (orders o join customers c on |)"),
        ] {
            assert_eq!(
                (refs(&s), s.expect),
                (vec![(Some("o"), &o), (Some("c"), &c)], Expect::JoinCondition { target: 1 })
            );
        }
        let s = pg("select c.| from (orders o join customers c on c.id = o.customer_id)");
        assert_eq!(
            (refs(&s), s.qualifier.clone()),
            (vec![(Some("o"), &o), (Some("c"), &c)], Some(Qualifier::Relation { index: 1 }))
        );
        for s in [
            pg("select * from ((orders o join customers c on c.id = o.customer_id) join addresses a on |)"),
            pg("select * from (orders o join customers c on true) join addresses a on |"),
        ] {
            assert_eq!(
                (refs(&s), s.expect),
                (vec![(Some("o"), &o), (Some("c"), &c), (Some("a"), &a)], Expect::JoinCondition { target: 2 })
            );
        }
        assert_eq!(pg("select * from (orders o join |").expect, Expect::Relation);
        let s = pg("select j.| from (orders o join customers c on o.customer_id = c.id) as j");
        let joined = RelationSource::Derived {
            columns: vec![],
            star: vec![
                TableName { schema: None, name: "orders".into() },
                TableName { schema: None, name: "customers".into() },
            ],
        };
        assert_eq!(
            (refs(&s), s.qualifier.clone()),
            (vec![(Some("j"), &joined)], Some(Qualifier::Relation { index: 0 }))
        );
        let s = pg("select * from (orders o join customers c on true) j join addresses a on |");
        assert_eq!(
            (refs(&s), s.expect),
            (vec![(Some("j"), &joined), (Some("a"), &a)], Expect::JoinCondition { target: 1 })
        );
    }

    #[test]
    fn a_column_alias_list_renames_the_first_columns() {
        let aliases = |s: &Scope| s.relations.iter().map(|r| r.column_aliases.clone()).collect::<Vec<_>>();
        let s = pg("select * from orders o(a, \"B\") where o.|");
        assert_eq!(
            (refs(&s), aliases(&s)),
            (vec![(Some("o"), &table(None, "orders"))], vec![vec!["a".to_owned(), "\"B\"".to_owned()]])
        );
        let s = pg("select * from unnest(array[1]) as u(x) where u.|");
        assert_eq!(named_columns(&s), [(Some("u"), vec!["x".to_owned()])]);
        let s = ms("SELECT * FROM (VALUES (1, 2)) AS v(a, b) WHERE v.|");
        assert_eq!(named_columns(&s), [(Some("v"), vec!["a".to_owned(), "b".to_owned()])]);
        assert_eq!(aliases(&ms("SELECT * FROM orders o (NOLOCK) WHERE o.|")), [Vec::<String>::new()]);
        let s = pg("with x as (select * from generate_series(1, 3) g(n)) select * from x where |");
        assert_eq!(refs(&s), [(Some("x"), &RelationSource::Derived { columns: vec!["n".into()], star: vec![] })]);
    }

    #[test]
    fn tsql_update_through_an_alias_resolves_to_its_from_item() {
        let s = ms("UPDATE o SET o.| FROM orders o");
        assert_eq!(
            (refs(&s), s.qualifier.clone()),
            (vec![(Some("o"), &table(None, "orders"))], Some(Qualifier::Relation { index: 0 }))
        );
        let s = ms("DECLARE @t TABLE (id int)\nSELECT * FROM @t t WHERE t.|");
        assert_eq!(refs(&s), [(Some("t"), &RelationSource::Derived { columns: vec![], star: vec![] })]);
    }

    #[test]
    fn a_word_that_starts_a_line_may_start_the_next_statement() {
        assert!(pg("select * from orders o\n\nsel|").keywords.contains(&"SELECT"));
        assert!(ms("SELECT * FROM orders o WHERE o.id = 1\nUPD|").keywords.contains(&"UPDATE"));
        assert!(!pg("select a,\n  b|").keywords.contains(&"SELECT"));
        assert!(!pg("select a from orders where\n  |").keywords.contains(&"INSERT INTO"));
    }

    #[test]
    fn an_alias_typed_right_after_a_relation_is_not_completed_unasked() {
        assert_eq!(pg("select * from orders o|").expect, Expect::Alias);
        assert_eq!(pg("select * from orders |").expect, Expect::Alias);
        assert_eq!(pg("select * from orders as |").expect, Expect::Alias);
        assert_eq!(pg("select * from orders as wh|").expect, Expect::Alias);
        assert_eq!(pg("select * from orders o |").expect, Expect::Keyword);
    }

    #[test]
    fn a_word_after_a_relation_may_be_its_alias_and_the_keywords_that_follow_are_listed() {
        for (s, keyword) in [
            (pg("select * from orders wh|"), "WHERE"),
            (pg("select * from order_items li|"), "LIMIT"),
            (pg("select * from orders ord|"), "ORDER BY"),
            (pg("select * from orders where|"), "WHERE"),
            (ms("SELECT * FROM orders WH|"), "WHERE"),
            (ms("SELECT * FROM orders ORD|"), "ORDER BY"),
        ] {
            assert_eq!((s.expect, s.keywords.contains(&keyword)), (Expect::Alias, true), "{keyword}");
        }
        assert_eq!(pg("select * from orders o join customers c whe|").expect, Expect::Keyword);
    }

    #[test]
    fn derived_columns_follow_the_names_the_server_gives() {
        let derived =
            |marked: &str, kind| match at(kind, marked).map(|s| s.relations.into_iter().next().map(|r| r.source)) {
                Some(Some(RelationSource::Derived { columns, star })) => {
                    (columns, star.into_iter().map(|t| t.name).collect::<Vec<_>>())
                }
                other => panic!("{marked:?} gave {other:?}"),
            };
        assert_eq!(
            derived("with x as (select distinct * from orders) select * from x where |", Postgres),
            (vec![], vec!["orders".into()])
        );
        assert_eq!(
            derived("with x as (select distinct on (id) * from orders) select * from x where |", Postgres),
            (vec![], vec!["orders".into()])
        );
        assert_eq!(
            derived(
                "with x as (select id::text, count(*), a + b, total n from orders) select * from x where |",
                Postgres
            )
            .0,
            ["id", "n"]
        );
        let calls = "with x as (select count(*), sum(total) over (), pg_catalog.abs(total), cast(id as text), trim(both from status), status collate \"C\", total::numeric n from orders) select * from x where |";
        assert_eq!(derived(calls, Postgres).0, ["n"]);
        let operators = "with x as (select rank() over w, sum(total) over w, id isnull, created_at at time zone tz, interval '1' day, count(*) c, (total + 1) t, o.id oid from orders o window w as (order by id)) select * from x where |";
        assert_eq!(derived(operators, Postgres).0, ["c", "t", "oid"]);
        let casts = "with x as (select total::double precision, id::numeric(10,2), t::timestamp(3) with time zone, a::int[], b::pg_catalog.int4, c::text d, e::numeric as f from orders) select * from x where |";
        assert_eq!(derived(casts, Postgres).0, ["total", "id", "t", "a", "b", "d", "f"]);
        assert_eq!(
            derived("WITH x AS (SELECT TOP 5 amount = o.total, o.id FROM orders o) SELECT * FROM x WHERE |", SqlServer)
                .0,
            ["amount", "id"]
        );
        assert_eq!(
            derived(
                "with x as (select id, name from a union select id, title from b) select * from x where |",
                Postgres
            )
            .0,
            ["id", "name"]
        );
        assert_eq!(
            derived("with x as (update orders set total = 0 returning id) select * from x where |", Postgres),
            (vec![], vec![])
        );
        assert_eq!(pg("with x as (update orders set total = 0 returning id) select * from x where |").ctes, ["x"]);
    }

    #[test]
    fn derived_columns_come_as_sql_writes_them() {
        let s = pg("with x as (select 1 as \"Total\", 2 as \"order\", 3 n) select * from x where |");
        assert_eq!(
            refs(&s),
            [(
                Some("x"),
                &RelationSource::Derived {
                    columns: vec!["\"Total\"".into(), "\"order\"".into(), "n".into()],
                    star: vec![]
                }
            )]
        );
        let s = ms("WITH x AS (SELECT 1 AS [Total], 2 AS [order]) SELECT * FROM x WHERE |");
        assert_eq!(
            refs(&s),
            [(Some("x"), &RelationSource::Derived { columns: vec!["Total".into(), "[order]".into()], star: vec![] })]
        );
    }

    #[test]
    fn a_group_reads_the_clause_it_opened_in_not_the_text_after_the_cursor() {
        let s = pg("select coalesce(|) from ");
        assert_eq!((s.expect, s.functions.contains(&"abs")), (Expect::Expression, true));
        assert_eq!(pg("select * from (|) x where a = 1").expect, Expect::Relation);
        let s = pg("select t1.| from t1 union select b from t2 as x");
        assert_eq!(
            (refs(&s), s.qualifier.clone()),
            (vec![(Some("t1"), &table(None, "t1"))], Some(Qualifier::Relation { index: 0 }))
        );
    }

    #[test]
    fn an_inner_alias_hides_the_outer_relation_of_that_name() {
        let s = pg("select * from orders o where exists (select * from customers o join addresses a on |)");
        assert_eq!(
            (refs(&s), s.expect),
            (
                vec![(Some("o"), &table(None, "customers")), (Some("a"), &table(None, "addresses"))],
                Expect::JoinCondition { target: 1 }
            )
        );
        assert_eq!(
            refs(&pg("select * from orders o where exists (select 1 from customers c where c.id = o.|)")).len(),
            2
        );
    }

    #[test]
    fn a_label_after_as_is_a_name_even_when_it_is_a_keyword() {
        assert_eq!(pg("select 1 as order, | from orders").expect, Expect::Expression);
        let s = pg("with x as (select 1 as order) select * from x where |");
        assert_eq!(
            refs(&s),
            [(Some("x"), &RelationSource::Derived { columns: vec!["\"order\"".into()], star: vec![] })]
        );
    }

    #[test]
    fn a_tsql_name_in_another_database_offers_no_catalog_columns() {
        let s = ms("SELECT o.| FROM reporting.dbo.orders o");
        assert_eq!(refs(&s), [(Some("o"), &RelationSource::Derived { columns: vec![], star: vec![] })]);
    }

    #[test]
    fn a_reserved_word_after_a_table_is_not_its_alias() {
        let orders = table(None, "orders");
        for s in [
            pg("select orders.| from orders for update"),
            pg("select orders.| from orders tablesample system (10)"),
            ms("SELECT orders.| FROM orders FOR JSON AUTO"),
            ms("SELECT orders.| FROM orders OPTION (RECOMPILE)"),
        ] {
            assert_eq!(
                (refs(&s), s.qualifier.clone()),
                (vec![(Some("orders"), &orders)], Some(Qualifier::Relation { index: 0 }))
            );
        }
        assert_eq!(refs(&pg("select f| from orders for update")), [(Some("orders"), &orders)]);
        assert!(
            !refs(&ms("SELECT * FROM orders PIVOT (SUM(total) FOR status IN ([a])) p WHERE |"))
                .iter()
                .any(|r| r.0 == Some("PIVOT"))
        );
        let s = pg("select s.| from unnest(array[1]) with ordinality s(v, i)");
        assert_eq!(
            (s.qualifier.clone(), named_columns(&s)),
            (Some(Qualifier::Relation { index: 0 }), vec![(Some("s"), vec!["v".to_owned(), "i".to_owned()])])
        );
        assert_eq!(refs(&ms("SELECT t.| FROM shop..orders t"))[0].0, Some("t"));
        assert_eq!(refs(&ms("SELECT orders.| FROM shop..orders"))[0].0, Some("orders"));
    }

    #[test]
    fn a_target_is_offered_only_where_the_server_lets_it_be_named() {
        assert!(refs(&pg("insert into orders values (|)")).is_empty());
        assert_eq!(refs(&pg("insert into orders (|")), [(None, &table(None, "orders"))]);
        assert_eq!(refs(&ms("SELECT o.id INTO #t FROM orders o WHERE |")), [(Some("o"), &table(None, "orders"))]);
        assert_eq!(
            refs(&pg("create table t as select * from orders o where |")),
            [(Some("o"), &table(None, "orders"))]
        );
        assert_eq!(
            refs(&ms("UPDATE orders SET status = 'x' FROM orders o WHERE |")),
            [(Some("o"), &table(None, "orders"))]
        );
        assert_eq!(refs(&pg("update orders set status = 'x' from orders o where |")).len(), 2);
        assert_eq!(refs(&ms("MERGE INTO orders t USING customers s ON |")).len(), 2);
        assert!(!ms("SELECT |").functions.contains(&"STRING_SPLIT"));
    }

    #[test]
    fn an_unnamed_output_column_keeps_its_place_for_a_rename() {
        assert_eq!(
            named_columns(&pg("select r.| from (select 1, id from orders) r(a)")),
            [(Some("r"), vec!["a".to_owned(), "id".to_owned()])]
        );
        assert_eq!(
            named_columns(&pg("select r.| from (select 1, id from orders) r")),
            [(Some("r"), vec!["id".to_owned()])]
        );
        assert_eq!(
            named_columns(&pg("with x(a, b) as (select 1, 2) select x.| from x")),
            [(Some("x"), vec!["a".to_owned(), "b".to_owned()])]
        );
        let s = pg("select r.| from (select 1, * from orders) r(a)");
        assert_eq!(
            (&s.relations[0].source, s.relations[0].column_aliases.len()),
            (&RelationSource::Derived { columns: vec![], star: vec![] }, 0)
        );
    }

    #[test]
    fn a_rename_over_columns_of_unknown_order_offers_no_columns() {
        let nothing = RelationSource::Derived { columns: vec![], star: vec![] };
        for marked in [
            "select r.| from (select * from orders o(oid)) r",
            "select r.| from (select *, 1 as x from orders) r(a)",
            "with x(a) as (select *, 1 as extra from orders) select x.| from x",
            "with x as (select * from orders o(oid)) select x.| from x",
        ] {
            let s = pg(marked);
            assert_eq!((&s.relations[0].source, s.relations[0].column_aliases.len()), (&nothing, 0), "{marked:?}");
        }
        let s = pg("select r.| from (select * from orders) r(a)");
        assert_eq!(named_columns(&s), [(Some("r"), vec!["a".to_owned()])]);
        assert_eq!(
            s.relations[0].source,
            RelationSource::Derived { columns: vec![], star: vec![TableName { schema: None, name: "orders".into() }] }
        );
    }

    #[test]
    fn a_word_after_a_table_is_an_alias_only_where_the_grammar_allows_one() {
        for s in [
            ms("UPDATE orders se|"),
            ms("DELETE FROM orders wh|"),
            ms("INSERT INTO orders va|"),
            pg("insert into orders va|"),
            ms("SELECT * INTO newt |"),
        ] {
            assert_eq!(s.expect, Expect::Keyword);
        }
        assert_eq!(ms("UPDATE orders se|").keywords, ["SET"]);
        assert_eq!(ms("SELECT * INTO newt |").keywords[0], "FROM");
        assert!(!ms("SELECT * INTO newt |").keywords.contains(&"VALUES"));
        for s in [
            pg("select * from orders wh|"),
            ms("SELECT * FROM orders o JOIN customers c|"),
            pg("update orders o|"),
            pg("delete from orders o|"),
            pg("insert into orders as o|"),
            ms("UPDATE o SET total = 0 FROM orders o|"),
            ms("MERGE INTO orders t|"),
            ms("MERGE INTO orders t USING customers s|"),
        ] {
            assert_eq!(s.expect, Expect::Alias);
        }
    }

    #[test]
    fn a_pivot_is_one_relation_with_no_known_columns() {
        let nothing = RelationSource::Derived { columns: vec![], star: vec![] };
        let s = ms("SELECT p.| FROM (SELECT status, total FROM orders) s PIVOT (MAX(total) FOR status IN ([a])) p");
        assert_eq!(
            (refs(&s), s.qualifier.clone()),
            (vec![(Some("p"), &nothing)], Some(Qualifier::Relation { index: 0 }))
        );
        let s = ms("SELECT | FROM orders PIVOT (MAX(total) FOR status IN ([a])) p");
        assert_eq!(refs(&s), [(Some("p"), &nothing)]);
        let s = ms("SELECT | FROM orders o UNPIVOT (v FOR k IN (a, b)) u");
        assert_eq!(refs(&s), [(Some("u"), &nothing)]);
    }

    #[test]
    fn a_locking_clause_names_no_relation() {
        for marked in [
            "select f| from orders for no key update nowait",
            "select f| from orders for update skip locked",
            "select f| from orders for key share of orders nowait",
        ] {
            assert_eq!(refs(&pg(marked)), [(Some("orders"), &table(None, "orders"))], "{marked:?}");
        }
    }

    #[test]
    fn an_insert_column_list_offers_columns_but_no_reference() {
        let s = pg("insert into orders (|");
        assert_eq!(s.relations.iter().map(|r| r.reference.clone()).collect::<Vec<_>>(), [None]);
        assert_eq!(s.relations[0].source, table(None, "orders"));
    }

    #[test]
    fn a_tsql_delete_or_update_takes_aliases_in_its_from_but_not_on_its_bare_target() {
        for s in [ms("DELETE FROM orders wh|"), ms("DELETE orders wh|"), ms("UPDATE orders se|")] {
            assert_eq!(s.expect, Expect::Keyword);
        }
        for s in [
            ms("DELETE wh FROM warehouses wh|"),
            ms("DELETE o FROM orders o|"),
            ms("DELETE orders FROM orders o|"),
            ms("DELETE FROM orders FROM orders o|"),
            ms("UPDATE x SET total = 0 FROM orders x|"),
            ms("MERGE orders t|"),
            ms("MERGE INTO orders t|"),
        ] {
            assert_eq!(s.expect, Expect::Alias);
        }
        let s = ms("MERGE orders t USING customers s ON |");
        assert_eq!(
            (refs(&s).iter().map(|r| r.0).collect::<Vec<_>>(), s.expect),
            (vec![Some("t"), Some("s")], Expect::JoinCondition { target: 1 })
        );
    }

    #[test]
    fn a_rename_over_a_using_or_natural_join_offers_no_columns() {
        let nothing = RelationSource::Derived { columns: vec![], star: vec![] };
        for marked in [
            "select r.| from (select * from orders join customers using (id)) r(oid)",
            "select r.| from (select * from orders natural join customers) r(oid)",
        ] {
            let s = pg(marked);
            assert_eq!((&s.relations[0].source, s.relations[0].column_aliases.len()), (&nothing, 0), "{marked:?}");
        }
        let s = pg("select r.| from (select * from orders join customers using (id)) r");
        assert_eq!(named_columns(&s)[0].0, Some("r"));
        assert!(matches!(&s.relations[0].source, RelationSource::Derived { star, .. } if star.len() == 2));
    }

    #[test]
    fn a_pivot_hides_its_whole_table_source() {
        let nothing = RelationSource::Derived { columns: vec![], star: vec![] };
        for marked in [
            "SELECT | FROM orders o JOIN customers n ON n.id = o.customer_id PIVOT (MAX(total) FOR status IN ([a])) p",
            "SELECT | FROM orders o CROSS JOIN customers n PIVOT (MAX(total) FOR status IN ([a])) p",
        ] {
            assert_eq!(refs(&ms(marked)), [(Some("p"), &nothing)], "{marked:?}");
        }
        let s = ms(
            "SELECT | FROM regions g, orders o JOIN customers n ON n.id = o.customer_id PIVOT (MAX(total) FOR status IN ([a])) p",
        );
        assert_eq!(refs(&s).iter().map(|r| r.0).collect::<Vec<_>>(), [Some("g"), Some("p")]);
    }

    #[test]
    fn a_returning_clause_sees_only_the_insert_target() {
        let s = pg("insert into orders (id) select id from customers returning |");
        assert_eq!(refs(&s), [(Some("orders"), &table(None, "orders"))]);
        assert_eq!(
            refs(&pg("insert into orders (id) select c.| from customers c")),
            [(Some("c"), &table(None, "customers"))]
        );
        assert_eq!(refs(&pg("insert into orders (id, |) select 1 from customers")).len(), 1);
    }

    #[test]
    fn a_cast_with_no_operand_names_nothing_and_does_not_panic() {
        let s = pg("with x as (select ::int y from orders) select * from x where |");
        assert_eq!(refs(&s), [(Some("x"), &RelationSource::Derived { columns: vec![], star: vec![] })]);
        assert!(at(Postgres, "select ::int x from orders where |").is_some());
    }

    #[test]
    fn a_tsql_top_before_the_bare_target_keeps_it_unaliased() {
        for s in [
            ms("DELETE TOP (1) FROM orders wh|"),
            ms("DELETE TOP (1) PERCENT FROM orders wh|"),
            ms("UPDATE TOP (1) orders se|"),
            ms("UPDATE TOP (10) PERCENT orders se|"),
        ] {
            assert_eq!(s.expect, Expect::Keyword);
        }
        assert_eq!(ms("UPDATE TOP (1) orders se|").keywords, ["SET"]);
        assert_eq!(
            refs(&ms("UPDATE TOP (1) orders SET total = 0 WHERE |")),
            [(Some("orders"), &table(None, "orders"))]
        );
        assert_eq!(ms("DELETE TOP (1) o FROM orders o|").expect, Expect::Alias);
    }

    #[test]
    fn a_tsql_delete_output_offers_deleted_with_the_named_target() {
        let orders = table(None, "orders");
        for marked in [
            "DELETE o OUTPUT deleted.| FROM orders o",
            "DELETE FROM orders OUTPUT deleted.| WHERE id = 1",
            "DELETE TOP (1) o OUTPUT deleted.| FROM orders o",
            "DELETE orders OUTPUT deleted.| FROM orders",
        ] {
            let s = ms(marked);
            assert_eq!(
                (refs(&s), s.qualifier.clone()),
                (vec![(Some("deleted"), &orders)], Some(Qualifier::Relation { index: 0 })),
                "{marked:?}"
            );
        }
        let names =
            ms("DELETE o OUTPUT | FROM orders o").relations.into_iter().map(|r| r.reference).collect::<Vec<_>>();
        assert_eq!(names, [Some("deleted".to_owned())]);
        assert!(ms("DELETE o OUTPUT inserted.| FROM orders o").relations.is_empty());
    }

    #[test]
    fn a_tsql_output_offers_only_inserted_and_deleted() {
        let names = |s: &Scope| s.relations.iter().map(|r| r.reference.clone().unwrap_or_default()).collect::<Vec<_>>();
        let s = ms("INSERT INTO tickets (code) OUTPUT | VALUES ('x')");
        assert_eq!(
            (names(&s), &s.relations[0].source),
            (vec!["inserted".to_owned()], &RelationSource::Derived { columns: vec![], star: vec![] })
        );
        let s = ms("INSERT INTO tickets (code) OUTPUT inserted.| VALUES ('x')");
        assert_eq!(
            (refs(&s), s.qualifier.clone()),
            (vec![(Some("inserted"), &table(None, "tickets"))], Some(Qualifier::Relation { index: 0 }))
        );
        assert_eq!(names(&ms("DELETE FROM tickets OUTPUT | WHERE id = 1")), ["deleted"]);
        assert_eq!(names(&ms("UPDATE tickets SET code = 'y' OUTPUT |")), ["inserted", "deleted"]);
        let s = ms("UPDATE t SET code = 'y' OUTPUT deleted.| FROM tickets t");
        assert_eq!(refs(&s), [(Some("deleted"), &table(None, "tickets"))]);
        assert!(ms("INSERT INTO tickets (code) OUTPUT tickets.| VALUES ('x')").relations.is_empty());
        assert_eq!(
            refs(&pg("insert into orders (id) values (1) returning |")),
            [(Some("orders"), &table(None, "orders"))]
        );
    }

    #[test]
    fn a_keyword_named_column_after_a_dot_is_not_a_clause() {
        for marked in [
            "select * from orders o where o.order = |",
            "select * from orders o where o.offset > |",
            "select o.from, | from orders o",
        ] {
            assert_eq!(pg(marked).expect, Expect::Expression, "{marked:?}");
        }
    }

    #[test]
    fn within_group_keeps_the_select_list() {
        assert_eq!(
            pg("select percentile_cont(0.5) within group (order by total), | from orders o").expect,
            Expect::Expression
        );
    }

    #[test]
    fn postgres_folds_only_ascii_letters_of_an_unquoted_name() {
        assert_eq!(refs(&pg("select * from CAFÉ c where c.|")), [(Some("c"), &table(None, "cafÉ"))]);
        assert_eq!(pg("select * from ÄBC where äbc.|").qualifier, Some(Qualifier::Name { parts: vec!["äbc".into()] }));
        assert_eq!(refs(&ms("SELECT * FROM CAFÉ c WHERE c.|")), [(Some("c"), &table(None, "CAFÉ"))]);
    }

    #[test]
    fn a_tsql_set_option_ends_its_statement() {
        let s = ms("UPDATE customers SET name = 'x'\nSET XACT_ABORT ON\nSELECT * FROM orders o WHERE o.|");
        assert_eq!(
            (refs(&s), s.qualifier.clone()),
            (vec![(Some("o"), &table(None, "orders"))], Some(Qualifier::Relation { index: 0 }))
        );
        let s = ms(
            "SELECT * FROM customers c\nUPDATE orders SET status = 1\nSET NOCOUNT ON\nSELECT * FROM orders o JOIN products p ON |",
        );
        assert_eq!(
            (refs(&s), s.expect),
            (
                vec![(Some("o"), &table(None, "orders")), (Some("p"), &table(None, "products"))],
                Expect::JoinCondition { target: 1 }
            )
        );
        let s = ms("SET NOCOUNT ON\nSELECT * FROM orders o WHERE o.|");
        assert_eq!(refs(&s), [(Some("o"), &table(None, "orders"))]);
        let s = ms("UPDATE o SET status = 1, total = o.| FROM orders o");
        assert_eq!((refs(&s).len(), s.expect), (1, Expect::Expression));
    }

    #[test]
    fn a_cte_that_opens_a_block_or_a_body_is_in_scope() {
        for marked in [
            "CREATE PROCEDURE p AS\nBEGIN\n  WITH recent AS (SELECT * FROM orders) SELECT * FROM rec|",
            "CREATE VIEW v AS WITH recent AS (SELECT * FROM orders) SELECT * FROM rec|",
            "IF 1 = 1 BEGIN WITH recent AS (SELECT * FROM orders) SELECT * FROM rec|",
            "BEGIN TRY WITH recent AS (SELECT * FROM orders) SELECT * FROM rec|",
            "SELECT 1;\nWITH recent AS (SELECT * FROM orders) SELECT * FROM rec|",
        ] {
            assert_eq!(ms(marked).ctes, ["recent"], "{marked:?}");
        }
        let s = ms("WITH x AS (SELECT id FROM orders) INSERT INTO t (id) SELECT x.| FROM x");
        assert_eq!((s.ctes.clone(), s.qualifier), (vec!["x".to_owned()], Some(Qualifier::Relation { index: 0 })));
        let s = pg("insert into orders (status) with recent as (select * from orders) select * from rec|");
        assert_eq!((s.expect, s.ctes), (Expect::Relation, vec!["recent".to_owned()]));
        let s = ms("BEGIN TRAN t WITH MARK 'm'\nSELECT * FROM orders o WHERE o.|");
        assert_eq!(refs(&s), [(Some("o"), &table(None, "orders"))]);
        let s = ms("SELECT * FROM orders o WITH (NOLOCK) WHERE o.|");
        assert_eq!(refs(&s), [(Some("o"), &table(None, "orders"))]);
    }

    #[test]
    fn tsql_offers_nothing_inside_its_own_quotes_and_comments() {
        for marked in [
            "SELECT N'ab|",
            "SELECT 'a''|",
            "SELECT [Col|",
            "SELECT 1 -- x|",
            "SELECT 1 /* a /* b */ c| */",
            "SELECT @na|",
        ] {
            assert!(at(SqlServer, marked).is_none(), "{marked:?}");
        }
        assert!(at(SqlServer, "SELECT [Col]|").is_some());
    }
}
