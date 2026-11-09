//! Notebook cell references. `{{title}}` in a cell's SQL stands for the result of the cell with that title in the
//! same notebook. Both dialect lexers read a reference as one lexeme wherever code can stand, so strings, comments,
//! quoted names, and dollar quotes hide one. Before a run, `expand` turns each reference into a CTE built from the
//! named cell's query, so the server runs one statement.
//!
//! The CTE is named `{{title}}`, quoted, so it cannot shadow a table that the statement or another cell reads under
//! the cell's title. A reference that starts a FROM item without an alias gets the title as its alias, so
//! `revenue.total` still reads.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::Range;

use super::lex::{self, LexKind, Strings};
use super::scope::{Class, Lexeme, map_lexemes};
use super::{StmtSpan, split_statements, tsql};
use crate::model::{CellSource, DatabaseKind, RunError, RunTarget};
use crate::params::{self, ExecutableScript};
use crate::text::quote_ident_in;

/// The end of the reference at `i`, when one starts there: `{{`, a title of characters other than `}` and line
/// breaks, then `}}`.
pub(crate) fn end_at(text: &str, i: usize) -> Option<usize> {
    let rest = text.get(i..)?.strip_prefix("{{")?;
    let close = rest.find(['}', '\n', '\r'])?;
    (rest[close..].starts_with("}}") && !rest[..close].trim().is_empty()).then_some(i + close + 4)
}

/// The title a reference names, without the spaces inside its braces.
fn title(reference: &str) -> &str {
    reference[2..reference.len() - 2].trim()
}

/// Each reference in `text`, with the title it names.
pub(crate) fn find(text: &str, kind: DatabaseKind) -> Vec<(Range<usize>, &str)> {
    map_lexemes(text, kind)
        .into_iter()
        .filter(|l| l.class == Class::Reference)
        .map(|l| (l.span.clone(), title(&text[l.span])))
        .collect()
}

/// Where the title being typed starts, when `at` follows a `{{` on its line that no `}` closes before `at`.
pub(crate) fn typing_at(text: &str, at: usize, kind: DatabaseKind) -> Option<usize> {
    let line = text[..at].rfind(['\n', '\r']).map_or(0, |n| n + 1);
    if !text[line..at].contains("{{") {
        return None;
    }
    let lexemes = map_lexemes(text, kind);
    let brace = |l: &Lexeme| l.class == Class::Other && &text[l.span.clone()] == "{";
    let open = lexemes
        .iter()
        .enumerate()
        .rev()
        .find(|&(i, l)| {
            line <= l.span.start
                && l.span.start + 2 <= at
                && (l.class == Class::Reference || brace(l) && lexemes.get(i + 1).is_some_and(|n| brace(n) && n.span.start == l.span.end))
        })
        .map(|(_, l)| l.span.start)?;
    let typed = &text[open + 2..at];
    if typed.contains('}') {
        return None;
    }
    Some(open + 2 + typed.len() - typed.trim_start_matches([' ', '\t']).len())
}

const PG_VERBS: &[&str] = &["SELECT", "WITH", "VALUES", "TABLE", "INSERT", "UPDATE", "DELETE", "MERGE"];
const TSQL_VERBS: &[&str] = &["SELECT", "WITH", "INSERT", "UPDATE", "DELETE", "MERGE"];
/// The words that start the statement after a WITH list.
const MAIN_VERBS: &[&str] = &["SELECT", "INSERT", "UPDATE", "DELETE", "MERGE", "VALUES", "TABLE"];
const MODULE: &str = "A reference cannot go inside a view, procedure, function, or trigger definition.";
const CONTROL: &str = "Put BEGIN and END around a statement after IF, ELSE, or WHILE to use a reference in it.";
const STRINGS: &str = "The references read differently with standard_conforming_strings off. Write backslashes in E'...' strings.";

/// Rewrites every statement or batch of `script` that holds a reference, and returns the cells it inlined in the order
/// their CTEs first appear. `ranges` keep pointing at the user's document, and a rewritten unit counts as changed, so
/// the server's error positions, which point into the rewritten SQL, are dropped.
pub(crate) fn expand(script: ExecutableScript, cells: &[CellSource], kind: DatabaseKind) -> Result<(ExecutableScript, Vec<CellSource>), RunError> {
    if !script.text.contains("{{") {
        return Ok((script, Vec::new()));
    }
    let mut resolver = Resolver { cells, kind, bodies: HashMap::new() };
    let mut used = Vec::new();
    let units = script
        .spans
        .iter()
        .map(|span| rewrite(span.text(&script.text), &mut resolver, &mut used))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|message| RunError::Reference { message })?;
    if used.is_empty() {
        return Ok((script, Vec::new()));
    }
    let mut text = String::new();
    let mut spans = Vec::with_capacity(units.len());
    for (span, unit) in script.spans.iter().zip(&units) {
        let start = text.len();
        text.push_str(unit.as_deref().unwrap_or_else(|| span.text(&script.text)));
        spans.push(StmtSpan { start, end: text.len() });
        text.push('\n');
    }
    let changed = script.changed.iter().zip(&units).map(|(changed, unit)| *changed || unit.is_some()).collect();
    let sources = used.iter().map(|t| cells[resolver.bodies[t].cell].clone()).collect();
    Ok((ExecutableScript { text, spans, ranges: script.ranges, changed }, sources))
}

/// A referenced cell's query, ready for a WITH list.
struct Body {
    cell: usize,
    /// The titles it references, in the order they first appear.
    deps: Vec<String>,
    /// Its WITH entries, `name AS (...)`. The last is the cell's own. SQL Server allows no WITH inside a CTE, so a body
    /// that starts with one moves those CTEs ahead of its own.
    entries: Vec<String>,
    /// The folded names of the CTEs a SQL Server body moved out of its own WITH.
    hoisted: Vec<String>,
    /// The unqualified tables and views the body reads, folded, which a CTE of the same name would shadow.
    relations: HashSet<String>,
}

struct Resolver<'a> {
    cells: &'a [CellSource],
    kind: DatabaseKind,
    bodies: HashMap<String, Body>,
}

impl Resolver<'_> {
    /// The titles `roots` reach, each after the ones it references.
    fn closure(&mut self, roots: impl IntoIterator<Item = String>) -> Result<Vec<String>, String> {
        let mut order = Vec::new();
        for root in roots {
            self.visit(root, &mut Vec::new(), &mut order)?;
        }
        Ok(order)
    }

    fn visit(&mut self, title: String, path: &mut Vec<String>, order: &mut Vec<String>) -> Result<(), String> {
        if order.contains(&title) {
            return Ok(());
        }
        if let Some(at) = path.iter().position(|p| *p == title) {
            return Err(cycle(&path[at..]));
        }
        if !self.bodies.contains_key(&title) {
            let body = body(self.cells, &title, self.kind)?;
            self.bodies.insert(title.clone(), body);
        }
        let deps = self.bodies[&title].deps.clone();
        path.push(title.clone());
        for dep in deps {
            self.visit(dep, path, order)?;
        }
        path.pop();
        order.push(title);
        Ok(())
    }
}

fn cycle(path: &[String]) -> String {
    match path {
        [first, through @ ..] if !through.is_empty() => format!("{first} refers back to itself through {}.", and_list(through)),
        _ => format!("{} refers back to itself.", path[0]),
    }
}

fn and_list(names: &[String]) -> String {
    match names {
        [init @ .., last] if !init.is_empty() => format!("{} and {last}", init.join(", ")),
        _ => names.join(""),
    }
}

/// The lexemes of `text` other than comments.
fn code(text: &str, kind: DatabaseKind) -> Vec<Lexeme> {
    map_lexemes(text, kind).into_iter().filter(|l| !matches!(l.class, Class::LineComment | Class::BlockComment)).collect()
}

/// A PostgreSQL tab may turn `standard_conforming_strings` off, and then a backslash can end a string where the
/// standard reading does not, so a reference must sit in code under both readings.
fn same_under_both_string_modes(text: &str, kind: DatabaseKind) -> Result<(), String> {
    if kind == DatabaseKind::SqlServer || !text.contains('\\') {
        return Ok(());
    }
    let references = |strings| lex::lex_strings(text, strings).into_iter().filter(|l| l.kind == LexKind::Reference).map(|l| l.span).collect::<Vec<_>>();
    if references(Strings::Standard) == references(Strings::Escaped) { Ok(()) } else { Err(STRINGS.into()) }
}

fn is_word(text: &str, lexeme: Option<&Lexeme>, words: &[&str]) -> bool {
    lexeme.is_some_and(|l| l.class == Class::Word && words.iter().any(|w| text[l.span.clone()].eq_ignore_ascii_case(w)))
}

/// A name as the server compares it. PostgreSQL folds an unquoted word to lower case, and SQL Server's default
/// collations ignore case.
fn folded(text: &str, lexeme: &Lexeme, kind: DatabaseKind) -> Option<String> {
    let t = &text[lexeme.span.clone()];
    let name = match lexeme.class {
        Class::Word if kind == DatabaseKind::Postgres => return Some(t.to_ascii_lowercase()),
        Class::Word => t.to_owned(),
        Class::Name => match t.strip_prefix('[').and_then(|t| t.strip_suffix(']')) {
            Some(bracketed) => bracketed.replace("]]", "]"),
            None => t.strip_prefix('"')?.strip_suffix('"')?.replace("\"\"", "\""),
        },
        _ => return None,
    };
    Some(fold(kind, name))
}

fn fold(kind: DatabaseKind, name: String) -> String {
    match kind {
        DatabaseKind::Postgres => name,
        DatabaseKind::SqlServer => name.to_lowercase(),
    }
}

/// The unqualified relations `text` reads, folded. Names its own CTEs define are not among them.
fn relations(kind: DatabaseKind, text: &str) -> HashSet<String> {
    match kind {
        DatabaseKind::Postgres => super::relation_names(text).into_iter().filter(|r| r.schema.is_none()).map(|r| r.name).collect(),
        DatabaseKind::SqlServer => tsql::relation_names(text).into_iter().filter(|r| r.schema.is_none()).map(|r| r.name.to_lowercase()).collect(),
    }
}

fn cte_name(title: &str) -> String {
    format!("{{{{{title}}}}}")
}

fn reserved(kind: DatabaseKind, word: &str) -> bool {
    match kind {
        DatabaseKind::Postgres => {
            let word = word.to_ascii_lowercase();
            super::RESERVED.contains(&word.as_str()) || super::TYPE_OR_FUNCTION_NAME.contains(&word.as_str())
        }
        DatabaseKind::SqlServer => tsql::RESERVED.contains(&word.to_ascii_uppercase().as_str()),
    }
}

/// A text change: `with` replaces `range`, and an empty range inserts.
struct Edit {
    range: Range<usize>,
    with: String,
}

/// `text[range]` with the edits inside it applied. `edits` are sorted and do not overlap.
fn render(text: &str, range: Range<usize>, edits: &[Edit]) -> String {
    let mut out = String::new();
    let mut at = range.start;
    for edit in edits.iter().filter(|e| range.start <= e.range.start && e.range.end <= range.end) {
        out.push_str(&text[at..edit.range.start]);
        out.push_str(&edit.with);
        at = edit.range.end;
    }
    out.push_str(&text[at..range.end]);
    out
}

/// The reference `code[k]` as the CTE's quoted name, aliased to its title when it starts a FROM item that has no alias.
fn reference_edit(text: &str, code: &[Lexeme], k: usize, kind: DatabaseKind) -> Edit {
    let title = title(&text[code[k].span.clone()]);
    let cte = quote_ident_in(kind, &cte_name(title));
    let from_item = k.checked_sub(1).is_some_and(|p| code[p].class == Class::Comma || is_word(text, Some(&code[p]), &["FROM", "JOIN", "USING", "APPLY"]));
    let aliased = code.get(k + 1).is_some_and(|next| match next.class {
        Class::Name => true,
        Class::Word => {
            let word = &text[next.span.clone()];
            word.eq_ignore_ascii_case("AS") || !reserved(kind, word)
        }
        _ => false,
    });
    let with = if from_item && !aliased { format!("{cte} AS {}", quote_ident_in(kind, title)) } else { cte };
    Edit { range: code[k].span.clone(), with }
}

fn close_of(code: &[Lexeme], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, l) in code.iter().enumerate().skip(open) {
        match l.class {
            Class::Open => depth += 1,
            Class::Close => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// The CTE list of a statement whose first lexeme, `code[0]`, is WITH.
struct WithList {
    recursive: bool,
    /// Where entries put first in the list go: just after `WITH`, or after `WITH RECURSIVE`.
    head: usize,
    /// Each entry's folded name and its text, `name AS (...)`.
    entries: Vec<(String, Range<usize>)>,
    /// The index in `code` of the statement after the list, when the list reads cleanly.
    main: Option<usize>,
}

fn with_list(text: &str, code: &[Lexeme], kind: DatabaseKind) -> WithList {
    let starts_entry = |j: usize| {
        code.get(j).is_some_and(|l| matches!(l.class, Class::Word | Class::Name))
            && (is_word(text, code.get(j + 1), &["AS"]) || code.get(j + 1).is_some_and(|l| l.class == Class::Open))
    };
    let recursive = kind == DatabaseKind::Postgres && is_word(text, code.get(1), &["RECURSIVE"]);
    let mut i = 1 + usize::from(recursive);
    let head = code[i - 1].span.end;
    let mut entries = Vec::new();
    let main = loop {
        let Some(name) = code.get(i).filter(|_| starts_entry(i)) else { break None };
        let mut j = i + 1;
        if code[j].class == Class::Open {
            let Some(close) = close_of(code, j) else { break None };
            j = close + 1;
        }
        if !is_word(text, code.get(j), &["AS"]) {
            break None;
        }
        j += 1;
        while is_word(text, code.get(j), &["NOT", "MATERIALIZED"]) {
            j += 1;
        }
        let Some(close) = code.get(j).filter(|l| l.class == Class::Open).and_then(|_| close_of(code, j)) else { break None };
        let Some(folded) = folded(text, name, kind) else { break None };
        entries.push((folded, name.span.start..code[close].span.end));
        j = close + 1;
        // PostgreSQL's SEARCH and CYCLE clauses follow an entry and hold commas of their own.
        if is_word(text, code.get(j), &["SEARCH", "CYCLE"]) {
            while j < code.len() && !(code[j].class == Class::Comma && starts_entry(j + 1)) && !is_word(text, code.get(j), MAIN_VERBS) {
                j += 1;
            }
        }
        match code.get(j) {
            Some(l) if l.class == Class::Comma => i = j + 1,
            Some(_) => break Some(j),
            None => break None,
        }
    };
    WithList { recursive, head, entries, main }
}

/// SQL Server refuses ORDER BY in a CTE unless TOP, OFFSET, or FOR XML gives it a meaning, and a CTE's rows have no
/// order anyway, so the final ORDER BY of a body without those goes. An OPTION clause after it stays.
fn order_by_cut(text: &str, code: &[Lexeme]) -> Option<Range<usize>> {
    let mut depth = 0usize;
    let mut order = None;
    let mut end = text.len();
    for (i, l) in code.iter().enumerate() {
        match l.class {
            Class::Open => depth += 1,
            Class::Close => depth = depth.saturating_sub(1),
            Class::Word if depth == 0 => {
                if is_word(text, Some(l), &["TOP", "OFFSET", "FOR"]) {
                    return None;
                }
                if is_word(text, Some(l), &["ORDER"]) && is_word(text, code.get(i + 1), &["BY"]) {
                    order = Some(l.span.start);
                    end = text.len();
                } else if order.is_some() && is_word(text, Some(l), &["OPTION"]) {
                    end = l.span.start;
                }
            }
            _ => {}
        }
    }
    Some(order?..end)
}

/// The byte range of the one query in a cell's text, without a trailing `;`.
fn query_range(text: &str, title: &str, kind: DatabaseKind) -> Result<Range<usize>, String> {
    let empty = || format!("{title} is empty.");
    if kind == DatabaseKind::Postgres {
        return match split_statements(text).as_slice() {
            [] => Err(empty()),
            [one] => Ok(one.start..one.end),
            many => Err(format!("{title} holds {} statements. A reference needs one query.", many.len())),
        };
    }
    let batch = match tsql::split_batches(text).map_err(|e| format!("{title} cannot be read: {e}."))?.as_slice() {
        [] => return Err(empty()),
        [one] => *one,
        many => return Err(format!("{title} holds {} batches. A reference needs one query.", many.len())),
    };
    let sql = batch.text(text);
    let code = code(sql, kind);
    let mut statement = code.iter().filter(|l| l.class != Class::Semi);
    let (Some(first), last) = (statement.next(), statement.next_back()) else { return Err(empty()) };
    let end = tsql::statement_around(sql, first.span.start).end;
    if code.iter().any(|l| l.span.start >= end && l.class != Class::Semi) {
        return Err(format!("{title} holds more than one statement. A reference needs one query."));
    }
    Ok(batch.start + first.span.start..batch.start + last.unwrap_or(first).span.end)
}

fn body(cells: &[CellSource], title: &str, kind: DatabaseKind) -> Result<Body, String> {
    let mut named = cells.iter().enumerate().filter(|(_, c)| c.name == title);
    let Some((cell, source)) = named.next() else { return Err(format!("No cell is named {title}.")) };
    let others = named.count();
    if others > 0 {
        return Err(format!("{} cells are named {title}. Rename one so the reference is clear.", others + 1));
    }
    match kind {
        DatabaseKind::Postgres if title.len() + 4 > 63 => return Err(format!("The title {title} is too long to reference. Keep it to 59 bytes.")),
        DatabaseKind::SqlServer if title.encode_utf16().count() + 4 > 128 => {
            return Err(format!("The title {title} is too long to reference. Keep it to 124 characters."));
        }
        _ => {}
    }
    let range = query_range(&source.text, title, kind)?;
    let sql = &source.text[range];
    same_under_both_string_modes(sql, kind)?;
    let code = code(sql, kind);
    let read = match kind {
        DatabaseKind::Postgres => {
            let into = matches!(super::vet(sql), super::Vet::Unvetted(found) if found.iter().any(|f| f == "SELECT INTO"));
            (code.first().is_some_and(|l| l.class == Class::Open) || is_word(sql, code.first(), &["SELECT", "WITH", "VALUES", "TABLE"])) && super::is_read_statement(sql) && !into
        }
        DatabaseKind::SqlServer => is_word(sql, code.first(), &["SELECT", "WITH"]) && !tsql::is_write(sql),
    };
    if !read {
        let queries = if kind == DatabaseKind::Postgres { "SELECT, WITH, VALUES, or TABLE" } else { "SELECT or WITH" };
        return Err(format!("{title} is not a query. A reference needs one {queries} query."));
    }
    let found = params::discover(sql, RunTarget::All, kind).map_err(|e| match e {
        RunError::Unsupported { what } => format!("{title}: {what}"),
        _ => format!("{title} cannot be read."),
    })?;
    if let Some(param) = found.first() {
        return Err(format!("{title} uses the parameter {}. A referenced cell cannot ask for values.", param.name));
    }
    let mut deps: Vec<String> = Vec::new();
    let mut edits = Vec::new();
    for (k, l) in code.iter().enumerate().filter(|(_, l)| l.class == Class::Reference) {
        let dep = self::title(&sql[l.span.clone()]);
        if !deps.iter().any(|d| d == dep) {
            deps.push(dep.to_owned());
        }
        edits.push(reference_edit(sql, &code, k, kind));
    }
    let own = |body: String| format!("{} AS (\n{body}\n)", quote_ident_in(kind, &cte_name(title)));
    let relations = relations(kind, sql);
    if kind == DatabaseKind::Postgres {
        return Ok(Body { cell, deps, entries: vec![own(render(sql, 0..sql.len(), &edits))], hoisted: Vec::new(), relations });
    }
    let (moved, main) = if is_word(sql, code.first(), &["WITH"]) {
        let list = with_list(sql, &code, kind);
        let Some(main) = list.main.filter(|_| !list.entries.is_empty()) else { return Err(format!("{title} starts with a WITH list this app cannot read.")) };
        (list.entries, main)
    } else {
        (Vec::new(), 0)
    };
    if let Some(cut) = order_by_cut(sql, &code[main..]) {
        edits.retain(|e| e.range.end <= cut.start || cut.end <= e.range.start);
        edits.push(Edit { range: cut, with: String::new() });
        edits.sort_by_key(|e| (e.range.start, e.range.end));
    }
    let mut entries: Vec<String> = moved.iter().map(|(_, range)| render(sql, range.clone(), &edits)).collect();
    entries.push(own(render(sql, code[main].span.start..sql.len(), &edits).trim_end().to_owned()));
    Ok(Body { cell, deps, entries, hoisted: moved.into_iter().map(|(name, _)| name).collect(), relations })
}

/// Rewrites one statement, or one SQL Server batch, when it holds a reference.
fn rewrite(unit: &str, resolver: &mut Resolver<'_>, used: &mut Vec<String>) -> Result<Option<String>, String> {
    let kind = resolver.kind;
    let code = code(unit, kind);
    let refs: Vec<usize> = code.iter().enumerate().filter(|(_, l)| l.class == Class::Reference).map(|(k, _)| k).collect();
    if refs.is_empty() {
        return Ok(None);
    }
    same_under_both_string_modes(unit, kind)?;
    let statements: BTreeMap<usize, Vec<usize>> = match kind {
        DatabaseKind::Postgres => BTreeMap::from([(0, refs)]),
        DatabaseKind::SqlServer => {
            if defines_module(unit, &code) {
                return Err(MODULE.into());
            }
            let mut statements = BTreeMap::<usize, Vec<usize>>::new();
            for k in refs {
                let start = tsql::statement_around(unit, code[k].span.start).start;
                statements.entry(code.partition_point(|l| l.span.start < start)).or_default().push(k);
            }
            statements
        }
    };
    let mut edits = Vec::new();
    for (first, refs) in statements {
        edits.extend(statement_edits(unit, &code, first, &refs, resolver, used)?);
    }
    edits.sort_by_key(|e| (e.range.start, e.range.end));
    Ok(Some(render(unit, 0..unit.len(), &edits)))
}

/// The edits that give the statement starting at `code[first]` a WITH entry for each cell it reaches through `refs`.
fn statement_edits(
    unit: &str,
    code: &[Lexeme],
    first: usize,
    refs: &[usize],
    resolver: &mut Resolver<'_>,
    used: &mut Vec<String>,
) -> Result<Vec<Edit>, String> {
    let kind = resolver.kind;
    let lead = &code[first];
    let (allowed, verbs) = match kind {
        DatabaseKind::Postgres => (lead.class == Class::Open || is_word(unit, Some(lead), PG_VERBS), "SELECT, WITH, VALUES, TABLE, INSERT, UPDATE, DELETE, and MERGE"),
        DatabaseKind::SqlServer => (is_word(unit, Some(lead), TSQL_VERBS), "SELECT, WITH, INSERT, UPDATE, DELETE, and MERGE"),
    };
    if !allowed {
        return Err(format!("A reference works only in {verbs} statements."));
    }
    let order = resolver.closure(refs.iter().map(|&k| title(&unit[code[k].span.clone()]).to_owned()))?;
    let bodies: Vec<(&str, &Body)> = order.iter().map(|t| (t.as_str(), &resolver.bodies[t])).collect();
    let with = is_word(unit, Some(lead), &["WITH"]).then(|| with_list(unit, &code[first..], kind));
    let own_ctes: Vec<&str> = with.iter().flat_map(|w| w.entries.iter().map(|(name, _)| name.as_str())).collect();
    let statement = match kind {
        DatabaseKind::Postgres => unit,
        DatabaseKind::SqlServer => &unit[tsql::statement_around(unit, lead.span.start)],
    };
    let reads = relations(kind, statement);
    for (i, (title, body)) in bodies.iter().enumerate() {
        for hoisted in &body.hoisted {
            let elsewhere = bodies.iter().enumerate().any(|(j, (_, other))| j != i && (other.relations.contains(hoisted) || other.hoisted.contains(hoisted)));
            if elsewhere || reads.contains(hoisted) || own_ctes.contains(&hoisted.as_str()) {
                return Err(format!("{title} defines the CTE {hoisted}, and this statement also reads or defines {hoisted}. Rename the CTE in {title}."));
            }
        }
    }
    let list = bodies.iter().flat_map(|(_, body)| body.entries.iter().map(String::as_str)).collect::<Vec<_>>().join(",\n");
    let mut edits: Vec<Edit> = refs.iter().map(|&k| reference_edit(unit, code, k, kind)).collect();
    if let Some(with) = &with {
        for name in &own_ctes {
            if let Some(title) = order.iter().find(|t| fold(kind, cte_name(t)) == *name) {
                return Err(format!("This statement already has a CTE named {}.", cte_name(title)));
            }
            if with.recursive && let Some((title, _)) = bodies.iter().find(|(_, body)| body.relations.contains(*name)) {
                return Err(format!("{title} reads {name}, which this WITH RECURSIVE also defines. Rename that CTE."));
            }
        }
        edits.push(Edit { range: with.head..with.head, with: format!(" {list},") });
    } else {
        let prefix = if kind == DatabaseKind::SqlServer { tsql_prefix(unit, code, first)? } else { "" };
        edits.push(Edit { range: lead.span.start..lead.span.start, with: format!("{prefix}WITH {list}\n") });
    }
    for title in order {
        if !used.contains(&title) {
            used.push(title);
        }
    }
    Ok(edits)
}

/// A view, procedure, function, or trigger owns its whole batch.
fn defines_module(unit: &str, code: &[Lexeme]) -> bool {
    let at = if is_word(unit, code.get(1), &["OR"]) && is_word(unit, code.get(2), &["ALTER"]) { 3 } else { 1 };
    is_word(unit, code.first(), &["CREATE", "ALTER"]) && is_word(unit, code.get(at), &["PROC", "PROCEDURE", "FUNCTION", "VIEW", "TRIGGER"])
}

/// What a new WITH needs before it. T-SQL needs the statement before a CTE ended with `;`, and the one statement that
/// IF, ELSE, or WHILE governs cannot take a `;` before it.
fn tsql_prefix(unit: &str, code: &[Lexeme], first: usize) -> Result<&'static str, String> {
    let Some(prev) = first.checked_sub(1).map(|p| &code[p]) else { return Ok("") };
    if prev.class == Class::Semi {
        return Ok("");
    }
    let start = tsql::statement_around(unit, prev.span.start).start;
    let governing = code.get(code.partition_point(|l| l.span.start < start));
    if is_word(unit, Some(prev), &["ELSE"]) || is_word(unit, governing, &["IF", "WHILE"]) {
        return Err(CONTROL.into());
    }
    Ok(";")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CellReference, CompletionContext, DocOffset, DocRange, ParamValue};
    use crate::text::{cell_references, completion_context};

    const PG: DatabaseKind = DatabaseKind::Postgres;
    const MS: DatabaseKind = DatabaseKind::SqlServer;
    const A: (&str, &str) = ("a", "SELECT 1 AS n;");

    fn cells(pairs: &[(&str, &str)]) -> Vec<CellSource> {
        pairs.iter().map(|(name, text)| CellSource { name: (*name).into(), text: (*text).into() }).collect()
    }

    /// Each unit the run sends, and the titles of the cells it inlined.
    fn run(kind: DatabaseKind, text: &str, pairs: &[(&str, &str)]) -> Result<(Vec<String>, Vec<String>), String> {
        let script = params::bind(text.into(), RunTarget::All, kind, Vec::new()).unwrap();
        match expand(script, &cells(pairs), kind) {
            Ok((script, sources)) => Ok((script.spans.iter().map(|s| s.text(&script.text).to_owned()).collect(), sources.into_iter().map(|c| c.name).collect())),
            Err(RunError::Reference { message }) => Err(message),
            Err(other) => panic!("{other:?}"),
        }
    }

    fn one(kind: DatabaseKind, text: &str, pairs: &[(&str, &str)]) -> String {
        let (units, _) = run(kind, text, pairs).unwrap();
        assert_eq!(units.len(), 1, "{units:?}");
        units.into_iter().next().unwrap()
    }

    fn refusal(kind: DatabaseKind, text: &str, pairs: &[(&str, &str)]) -> String {
        run(kind, text, pairs).unwrap_err()
    }

    fn titles(kind: DatabaseKind, text: &str) -> Vec<&str> {
        find(text, kind).into_iter().map(|(_, title)| title).collect()
    }

    #[test]
    fn a_reference_is_a_title_in_double_braces() {
        assert_eq!(titles(PG, "select * from {{ Untitled-1.sql }} join {{revenue by region}} using (id)"), ["Untitled-1.sql", "revenue by region"]);
        for text in ["{{}}", "{{   }}", "{{a\n}}", "{{a\r}}", "{a}", "{{a}", "{{a} }"] {
            assert_eq!(titles(PG, text), Vec::<&str>::new(), "{text:?}");
            assert_eq!(titles(MS, text), Vec::<&str>::new(), "{text:?}");
        }
        assert_eq!(titles(PG, "{{a}}}"), ["a"]);
    }

    #[test]
    fn strings_comments_quoted_names_and_dollar_quotes_hide_references() {
        assert_eq!(titles(PG, "select '{{a}}', E'{{a}}', \"{{a}}\", $${{a}}$$, $t$ {{a}} $t$ -- {{a}}\n/* {{a}} */ from {{b}}"), ["b"]);
        assert_eq!(titles(MS, "select N'{{a}}', '{{a}}', [{{a}}], \"{{a}}\" -- {{a}}\n/* {{a}} */ from {{b}}"), ["b"]);
        assert_eq!(one(PG, "SELECT '{{a}}' AS label, n FROM {{a}} x", &[A]), "WITH \"{{a}}\" AS (\nSELECT 1 AS n\n)\nSELECT '{{a}}' AS label, n FROM \"{{a}}\" x");
    }

    #[test]
    fn a_title_may_hold_quotes_and_dashes_without_hiding_the_sql_after_it() {
        let cells = [("bob's -- rows", "SELECT 1 AS n")];
        let (units, _) = run(PG, "SELECT * FROM {{bob's -- rows}} x;\nSELECT 2", &cells).unwrap();
        assert_eq!(units, ["WITH \"{{bob's -- rows}}\" AS (\nSELECT 1 AS n\n)\nSELECT * FROM \"{{bob's -- rows}}\" x", "SELECT 2"]);
        assert_eq!(one(MS, "SELECT * FROM {{bob's -- rows}} x", &cells), "WITH [{{bob's -- rows}}] AS (\nSELECT 1 AS n\n)\nSELECT * FROM [{{bob's -- rows}}] x");
        let found = params::discover("SELECT * FROM {{a :b $1}} WHERE n = :v", RunTarget::All, PG).unwrap();
        assert_eq!(found.into_iter().map(|p| p.name).collect::<Vec<_>>(), [":v"]);
        assert!(params::discover("SELECT * FROM {{@a}}", RunTarget::All, MS).unwrap().is_empty());
    }

    #[test]
    fn a_reference_becomes_a_cte_named_after_its_braces() {
        assert_eq!(one(PG, "SELECT * FROM {{a}} x WHERE x.n > 0", &[A]), "WITH \"{{a}}\" AS (\nSELECT 1 AS n\n)\nSELECT * FROM \"{{a}}\" x WHERE x.n > 0");
        assert_eq!(run(PG, "SELECT * FROM {{a}} x", &[A, ("b", "SELECT 2")]).unwrap().1, ["a"]);
    }

    #[test]
    fn a_from_item_without_an_alias_takes_the_title_as_its_alias() {
        let expanded = |text| one(PG, text, &[A]).split_once("\n)\n").unwrap().1.to_owned();
        assert_eq!(expanded("SELECT a.n FROM {{a}}"), "SELECT a.n FROM \"{{a}}\" AS \"a\"");
        assert_eq!(expanded("SELECT * FROM {{a}} WHERE n > 0"), "SELECT * FROM \"{{a}}\" AS \"a\" WHERE n > 0");
        assert_eq!(expanded("SELECT * FROM t JOIN {{a}} ON true, {{ a }}"), "SELECT * FROM t JOIN \"{{a}}\" AS \"a\" ON true, \"{{a}}\" AS \"a\"");
        assert_eq!(expanded("SELECT * FROM {{a}} AS q"), "SELECT * FROM \"{{a}}\" AS q");
        assert_eq!(expanded("SELECT * FROM {{a}} \"Q\""), "SELECT * FROM \"{{a}}\" \"Q\"");
        assert_eq!(expanded("TABLE {{a}}"), "TABLE \"{{a}}\"");
        assert!(one(MS, "SELECT a.n FROM {{a}}", &[A]).ends_with("SELECT a.n FROM [{{a}}] AS [a]"));
    }

    #[test]
    fn chains_and_diamonds_define_each_cell_once_after_the_cells_it_reads() {
        let pairs = [
            ("top", "SELECT * FROM {{left}} JOIN {{right}} USING (n)"),
            ("left", "SELECT n FROM {{base}}"),
            ("right", "SELECT n FROM {{base}} WHERE n > 0"),
            ("base", "SELECT 1 AS n"),
        ];
        let (units, sources) = run(PG, "SELECT * FROM {{top}} t", &pairs).unwrap();
        assert_eq!(
            units,
            ["WITH \"{{base}}\" AS (\nSELECT 1 AS n\n),\n\"{{left}}\" AS (\nSELECT n FROM \"{{base}}\" AS \"base\"\n),\n\"{{right}}\" AS (\nSELECT n FROM \"{{base}}\" AS \"base\" WHERE n > 0\n),\n\"{{top}}\" AS (\nSELECT * FROM \"{{left}}\" AS \"left\" JOIN \"{{right}}\" AS \"right\" USING (n)\n)\nSELECT * FROM \"{{top}}\" t"]
        );
        assert_eq!(sources, ["base", "left", "right", "top"]);
    }

    #[test]
    fn only_the_units_with_references_change() {
        let script = params::bind("SELECT 1;\nSELECT * FROM {{a}} x;\nSELECT 3".into(), RunTarget::All, PG, Vec::new()).unwrap();
        let ranges = script.ranges.clone();
        let (expanded, sources) = expand(script, &cells(&[A]), PG).unwrap();
        let units: Vec<&str> = expanded.spans.iter().map(|s| s.text(&expanded.text)).collect();
        assert_eq!(units, ["SELECT 1", "WITH \"{{a}}\" AS (\nSELECT 1 AS n\n)\nSELECT * FROM \"{{a}}\" x", "SELECT 3"]);
        assert_eq!(expanded.changed, [false, true, false]);
        assert_eq!(expanded.ranges, ranges);
        assert_eq!(sources, cells(&[A]));

        let plain = params::bind("SELECT '{{a}}'".into(), RunTarget::All, PG, Vec::new()).unwrap();
        let (same, sources) = expand(plain, &[], PG).unwrap();
        assert_eq!((same.text.as_str(), same.changed, sources), ("SELECT '{{a}}'", vec![false], Vec::new()));
    }

    #[test]
    fn parameters_bind_before_references_expand() {
        let script = params::bind("SELECT * FROM {{a}} x WHERE x.n = :v".into(), RunTarget::All, PG, vec![ParamValue { name: ":v".into(), value: Some("1".into()) }]).unwrap();
        let (expanded, _) = expand(script, &cells(&[A]), PG).unwrap();
        assert_eq!(expanded.spans[0].text(&expanded.text), "WITH \"{{a}}\" AS (\nSELECT 1 AS n\n)\nSELECT * FROM \"{{a}}\" x WHERE x.n = E'1'");
    }

    #[test]
    fn a_statement_with_its_own_with_list_gets_the_cell_ctes_first() {
        assert_eq!(
            one(PG, "WITH t AS (SELECT * FROM {{a}}) SELECT * FROM t", &[A]),
            "WITH \"{{a}}\" AS (\nSELECT 1 AS n\n), t AS (SELECT * FROM \"{{a}}\" AS \"a\") SELECT * FROM t"
        );
        assert_eq!(
            one(PG, "WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM r WHERE n < 3) SELECT * FROM r JOIN {{a}} USING (n)", &[A]),
            "WITH RECURSIVE \"{{a}}\" AS (\nSELECT 1 AS n\n), r(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM r WHERE n < 3) SELECT * FROM r JOIN \"{{a}}\" AS \"a\" USING (n)"
        );
        assert_eq!(
            one(MS, "WITH t AS (SELECT * FROM {{a}}) SELECT * FROM t", &[A]),
            "WITH [{{a}}] AS (\nSELECT 1 AS n\n), t AS (SELECT * FROM [{{a}}] AS [a]) SELECT * FROM t"
        );
    }

    #[test]
    fn the_with_goes_before_the_first_token_after_leading_comments() {
        assert_eq!(one(PG, "-- totals\n/* by region */ SELECT * FROM {{a}} x", &[A]), "-- totals\n/* by region */ WITH \"{{a}}\" AS (\nSELECT 1 AS n\n)\nSELECT * FROM \"{{a}}\" x");
    }

    #[test]
    fn a_body_that_ends_in_a_line_comment_cannot_swallow_the_closing_paren() {
        assert!(one(PG, "SELECT * FROM {{a}} x", &[("a", "SELECT 1 AS n -- one row")]).starts_with("WITH \"{{a}}\" AS (\nSELECT 1 AS n -- one row\n)\n"));
        assert!(one(MS, "SELECT * FROM {{a}} x", &[("a", "SELECT 1 AS n -- one row")]).starts_with("WITH [{{a}}] AS (\nSELECT 1 AS n\n)\n"));
    }

    #[test]
    fn writes_and_parenthesized_queries_can_read_a_reference() {
        let insert = one(PG, "INSERT INTO t SELECT * FROM {{a}}", &[A]);
        assert_eq!(insert, "WITH \"{{a}}\" AS (\nSELECT 1 AS n\n)\nINSERT INTO t SELECT * FROM \"{{a}}\" AS \"a\"");
        assert_eq!(super::super::classify(&insert), crate::sql::StmtKind::Dml(crate::sql::DmlVerb::Insert));
        assert!(one(PG, "UPDATE t SET n = x.n FROM {{a}} x WHERE t.id = 1", &[A]).ends_with("\nUPDATE t SET n = x.n FROM \"{{a}}\" x WHERE t.id = 1"));
        assert!(one(PG, "(SELECT * FROM {{a}} x) UNION ALL (SELECT 2)", &[A]).ends_with("\n(SELECT * FROM \"{{a}}\" x) UNION ALL (SELECT 2)"));
        assert!(one(MS, "MERGE INTO t USING {{a}} s ON t.n = s.n WHEN MATCHED THEN DELETE;", &[A]).ends_with("\nMERGE INTO t USING [{{a}}] s ON t.n = s.n WHEN MATCHED THEN DELETE;"));
    }

    #[test]
    fn a_reference_must_name_exactly_one_cell() {
        assert_eq!(refusal(PG, "SELECT * FROM {{nope}}", &[A]), "No cell is named nope.");
        assert_eq!(refusal(PG, "SELECT * FROM {{A}}", &[A]), "No cell is named A.");
        assert_eq!(refusal(PG, "SELECT * FROM {{a}}", &[A, ("a", "SELECT 2")]), "2 cells are named a. Rename one so the reference is clear.");
        assert_eq!(refusal(MS, "SELECT * FROM {{x}}", &[("y", "SELECT * FROM {{x}}")]), "No cell is named x.");
    }

    #[test]
    fn a_cycle_is_refused_with_its_path() {
        assert_eq!(refusal(PG, "SELECT * FROM {{a}}", &[("a", "SELECT * FROM {{a}}")]), "a refers back to itself.");
        assert_eq!(refusal(PG, "SELECT * FROM {{a}}", &[("a", "SELECT * FROM {{b}}"), ("b", "SELECT * FROM {{a}}")]), "a refers back to itself through b.");
        assert_eq!(
            refusal(MS, "SELECT * FROM {{a}}", &[("a", "SELECT * FROM {{b}}"), ("b", "SELECT * FROM {{c}}"), ("c", "SELECT * FROM {{a}}")]),
            "a refers back to itself through b and c."
        );
    }

    #[test]
    fn a_body_must_be_one_query_without_parameters() {
        let body = |kind, text| refusal(kind, "SELECT * FROM {{a}}", &[("a", text)]);
        assert_eq!(body(PG, "  -- nothing\n"), "a is empty.");
        assert_eq!(body(PG, "SELECT 1; SELECT 2"), "a holds 2 statements. A reference needs one query.");
        for text in ["DELETE FROM t RETURNING *", "SHOW search_path", "EXPLAIN SELECT 1", "WITH d AS (DELETE FROM t RETURNING *) SELECT * FROM d", "SELECT * INTO u FROM t"] {
            assert_eq!(body(PG, text), "a is not a query. A reference needs one SELECT, WITH, VALUES, or TABLE query.", "{text}");
        }
        assert_eq!(body(PG, "SELECT * FROM t WHERE id = :id"), "a uses the parameter :id. A referenced cell cannot ask for values.");
        assert_eq!(body(PG, "SELECT * FROM t WHERE id = $1"), "a uses the parameter $1. A referenced cell cannot ask for values.");
        assert_eq!(body(MS, ""), "a is empty.");
        assert_eq!(body(MS, "SELECT 1\nGO\nSELECT 2"), "a holds 2 batches. A reference needs one query.");
        assert_eq!(body(MS, "SELECT 1 SELECT 2"), "a holds more than one statement. A reference needs one query.");
        for text in ["UPDATE t SET n = 1", "SELECT n INTO #x FROM t", "EXEC sp_who", "VALUES (1)"] {
            assert_eq!(body(MS, text), "a is not a query. A reference needs one SELECT or WITH query.", "{text}");
        }
        assert_eq!(body(MS, "SELECT * FROM t WHERE id = @id"), "a uses the parameter @id. A referenced cell cannot ask for values.");
        assert!(run(MS, "SELECT * FROM {{a}}", &[("a", "SELECT 1;")]).is_ok());
    }

    #[test]
    fn a_title_must_fit_an_identifier() {
        let fits = "x".repeat(59);
        assert!(run(PG, &format!("SELECT * FROM {{{{{fits}}}}}"), &[(&fits, "SELECT 1")]).is_ok());
        let long = "x".repeat(60);
        assert_eq!(refusal(PG, &format!("SELECT * FROM {{{{{long}}}}}"), &[(&long, "SELECT 1")]), format!("The title {long} is too long to reference. Keep it to 59 bytes."));
        let long = "x".repeat(125);
        assert_eq!(refusal(MS, &format!("SELECT * FROM {{{{{long}}}}}"), &[(&long, "SELECT 1")]), format!("The title {long} is too long to reference. Keep it to 124 characters."));
    }

    #[test]
    fn only_queries_and_dml_can_hold_a_reference() {
        for text in ["CREATE TABLE t AS SELECT * FROM {{a}}", "EXPLAIN SELECT * FROM {{a}}", "COPY (SELECT * FROM {{a}}) TO STDOUT"] {
            assert_eq!(refusal(PG, text, &[A]), "A reference works only in SELECT, WITH, VALUES, TABLE, INSERT, UPDATE, DELETE, and MERGE statements.", "{text}");
        }
        assert_eq!(refusal(MS, "DECLARE c CURSOR FOR SELECT * FROM {{a}}", &[A]), "A reference works only in SELECT, WITH, INSERT, UPDATE, DELETE, and MERGE statements.");
        assert_eq!(refusal(MS, "CREATE VIEW v AS SELECT * FROM {{a}}", &[A]), MODULE);
        assert_eq!(refusal(MS, "CREATE OR ALTER PROCEDURE p AS SELECT * FROM {{a}}", &[A]), MODULE);
    }

    #[test]
    fn a_cte_name_the_statement_already_uses_is_refused() {
        assert_eq!(refusal(PG, "WITH \"{{a}}\" AS (SELECT 2) SELECT * FROM {{a}} x", &[A]), "This statement already has a CTE named {{a}}.");
        assert_eq!(
            refusal(PG, "WITH RECURSIVE orders AS (SELECT 1 AS n) SELECT * FROM orders, {{a}} x", &[("a", "SELECT * FROM Orders")]),
            "a reads orders, which this WITH RECURSIVE also defines. Rename that CTE."
        );
        assert!(run(PG, "WITH RECURSIVE orders AS (SELECT 1 AS n) SELECT * FROM orders, {{a}} x", &[("a", "SELECT o.orders FROM public.orders o")]).is_ok());
        assert!(run(PG, "WITH orders AS (SELECT 1 AS n) SELECT * FROM orders, {{a}} x", &[("a", "SELECT * FROM orders")]).is_ok());
    }

    #[test]
    fn a_reference_must_read_the_same_whatever_standard_conforming_strings_is() {
        assert_eq!(refusal(PG, "SELECT 'a\\' || {{a}} || '", &[A]), STRINGS);
        assert!(run(PG, "SELECT E'a\\'' AS s, n FROM {{a}} x", &[A]).is_ok());
    }

    #[test]
    fn sql_server_ends_the_statement_before_a_new_with() {
        assert_eq!(
            one(MS, "DECLARE @n int = 1\nSELECT * FROM {{a}} x WHERE x.n = @n", &[A]),
            "DECLARE @n int = 1\n;WITH [{{a}}] AS (\nSELECT 1 AS n\n)\nSELECT * FROM [{{a}}] x WHERE x.n = @n"
        );
        assert_eq!(one(MS, "SELECT 1;\nSELECT * FROM {{a}} x", &[A]), "SELECT 1;\nWITH [{{a}}] AS (\nSELECT 1 AS n\n)\nSELECT * FROM [{{a}}] x");
        assert_eq!(
            one(MS, "IF 1 = 1\nBEGIN\n  SELECT * FROM {{a}} x\nEND", &[A]),
            "IF 1 = 1\nBEGIN\n  ;WITH [{{a}}] AS (\nSELECT 1 AS n\n)\nSELECT * FROM [{{a}}] x\nEND"
        );
        assert_eq!(
            one(MS, "SELECT * FROM {{a}} x\nSELECT * FROM {{b}} y", &[A, ("b", "SELECT 2 AS n")]),
            "WITH [{{a}}] AS (\nSELECT 1 AS n\n)\nSELECT * FROM [{{a}}] x\n;WITH [{{b}}] AS (\nSELECT 2 AS n\n)\nSELECT * FROM [{{b}}] y"
        );
        assert!(one(MS, "SELECT 1\n;WITH t AS (SELECT * FROM {{a}}) SELECT * FROM t", &[A]).starts_with("SELECT 1\n;WITH [{{a}}] AS ("));
    }

    #[test]
    fn sql_server_refuses_a_reference_in_a_statement_that_if_else_or_while_governs() {
        for text in ["IF 1 = 1 SELECT * FROM {{a}}", "IF 1 = 1 SELECT 1 ELSE SELECT * FROM {{a}}", "WHILE 1 = 0\n  INSERT INTO t SELECT * FROM {{a}}"] {
            assert_eq!(refusal(MS, text, &[A]), CONTROL, "{text}");
        }
        assert!(run(MS, "IF 1 = 1 SELECT 1\nSELECT * FROM {{a}} x", &[A]).is_ok());
    }

    #[test]
    fn sql_server_hoists_a_body_with_list_ahead_of_its_cte() {
        let pairs = [("a", "WITH big AS (SELECT n FROM t WHERE n > 1) SELECT n FROM big;")];
        assert_eq!(one(MS, "SELECT * FROM {{a}} x", &pairs), "WITH big AS (SELECT n FROM t WHERE n > 1),\n[{{a}}] AS (\nSELECT n FROM big\n)\nSELECT * FROM [{{a}}] x");
        let pairs = [("a", "WITH t AS (SELECT 1 AS n) SELECT n FROM t"), ("b", "WITH t AS (SELECT 2 AS n) SELECT n FROM t")];
        let clash = "a defines the CTE t, and this statement also reads or defines t. Rename the CTE in a.";
        assert_eq!(refusal(MS, "SELECT * FROM {{a}} x JOIN dbo.u ON 1 = 1 JOIN T ON T.n = x.n", &pairs), clash);
        assert_eq!(refusal(MS, "SELECT * FROM {{a}} x, {{b}} y", &pairs), clash);
        assert_eq!(refusal(MS, "WITH t AS (SELECT 3 AS n) SELECT * FROM {{a}} x", &pairs), clash);
        assert!(run(MS, "SELECT t.n, x.t FROM {{a}} t JOIN dbo.t x ON 1 = 1\nSELECT * FROM t", &pairs).is_ok(), "an alias, a qualified table, or another statement cannot be shadowed");
        assert_eq!(refusal(MS, "SELECT * FROM {{a}} x", &[("a", "WITH XMLNAMESPACES ('urn:x' AS x) SELECT 1 AS n")]), "a starts with a WITH list this app cannot read.");
    }

    #[test]
    fn sql_server_drops_a_body_order_by_that_means_nothing_in_a_cte() {
        let body = |text: &str| one(MS, "SELECT * FROM {{a}} x", &[("a", text)]).split_once(" AS (\n").unwrap().1.split_once("\n)\n").unwrap().0.to_owned();
        assert_eq!(body("SELECT n FROM t ORDER BY n DESC"), "SELECT n FROM t");
        assert_eq!(body("SELECT n FROM t UNION SELECT n FROM u ORDER BY 1;"), "SELECT n FROM t UNION SELECT n FROM u");
        assert_eq!(body("SELECT n FROM t ORDER BY n OPTION (RECOMPILE)"), "SELECT n FROM t OPTION (RECOMPILE)");
        assert_eq!(body("WITH b AS (SELECT TOP 1 n FROM t ORDER BY n) SELECT n FROM b ORDER BY n"), "SELECT n FROM b");
        for kept in ["SELECT TOP 5 n FROM t ORDER BY n", "SELECT n FROM t ORDER BY n OFFSET 0 ROWS", "SELECT n FROM t ORDER BY n FOR XML PATH('')", "SELECT n, ROW_NUMBER() OVER (ORDER BY n) AS r FROM t"] {
            assert_eq!(body(kept), kept);
        }
        assert!(one(PG, "SELECT * FROM {{a}} x", &[("a", "SELECT n FROM t ORDER BY n")]).contains("SELECT n FROM t ORDER BY n\n)"));
    }

    #[test]
    fn formatting_keeps_a_reference_as_written() {
        let format = |kind, text| crate::text::format_edit(text, RunTarget::All, kind).unwrap().insert;
        assert_eq!(format(PG, "select * from {{ revenue by region }} r where r.total > 10"), "SELECT *\nFROM {{ revenue by region }} r\nWHERE r.total > 10");
        assert!(format(MS, "select * from {{ revenue by region }} r").contains("from {{ revenue by region }} r"));
    }

    #[test]
    fn cell_references_are_utf16_ranges_around_the_braces() {
        assert_eq!(
            cell_references("SELECT '😀', {{ a }}", PG),
            [CellReference { range: DocRange { from: DocOffset(13), to: DocOffset(20) }, name: "a".into() }]
        );
        assert_eq!(cell_references("SELECT [{{a}}], {{b}}", MS).into_iter().map(|r| r.name).collect::<Vec<_>>(), ["b"]);
    }

    #[test]
    fn completion_offers_titles_inside_an_open_reference() {
        let at = |kind, text: &str, cursor: usize| completion_context(text, DocOffset(cursor as u32), kind);
        let reference = |from: u32| CompletionContext::Reference { from: DocOffset(from) };
        assert_eq!(at(PG, "SELECT * FROM {{rev", 19), reference(16));
        assert_eq!(at(PG, "SELECT * FROM {{  rev", 21), reference(18));
        assert_eq!(at(PG, "SELECT * FROM {{", 16), reference(16));
        assert_eq!(at(PG, "SELECT * FROM {{revenue}} r", 19), reference(16));
        assert_eq!(at(PG, "SELECT * FROM {{bob's", 21), reference(16));
        assert_eq!(at(PG, "SELECT 'é' FROM {{r", 19), reference(18));
        assert_eq!(at(MS, "SELECT * FROM {{rev", 19), reference(16));
        for (kind, text, cursor) in [(PG, "SELECT * FROM {{revenue}} ", 26), (PG, "SELECT '{{rev", 13), (PG, "-- {{rev", 8), (PG, "{{a\nSELECT ", 11), (MS, "SELECT * FROM [{{rev", 20), (PG, "SELECT * FROM {", 15)] {
            assert!(!matches!(at(kind, text, cursor), CompletionContext::Reference { .. }), "{text:?}");
        }
    }
}
