use crate::model::{
    CellReference, CellWrite, ColumnFilter, CompletionContext, DocOffset, DocRange, FilterOp, FormatEdit, FormatError,
    RunTarget, ScriptOutline, TableQuery,
};
use crate::model::{
    DatabaseKind, ExecutionUnitKind, Relation, TableOrderEdit, TableOrderView, TableScript, TableSortKey,
};
use crate::sql::{self, ByteTarget, DmlVerb, StmtKind, StmtSpan};

/// Converts between byte offsets and UTF-16 code units. Callers convert a script's statements in order, so each
/// conversion resumes from the last one, which keeps converting a whole script linear.
pub struct OffsetMap<'a> {
    text: &'a str,
    one_unit_per_byte: bool,
    /// The byte offset and the UTF-16 offset of the last conversion, on a char boundary.
    last: std::cell::Cell<(usize, u32)>,
}

impl<'a> OffsetMap<'a> {
    pub fn new(text: &'a str) -> Self {
        Self { text, one_unit_per_byte: text.is_ascii(), last: std::cell::Cell::new((0, 0)) }
    }

    pub fn to_utf16(&self, byte: usize) -> DocOffset {
        let byte = byte.min(self.text.len());
        if self.one_unit_per_byte {
            return DocOffset(byte as u32);
        }
        let (from_byte, from_units) = match self.last.get() {
            (b, u) if b <= byte => (b, u),
            _ => (0, 0),
        };
        let units = from_units + self.text[from_byte..byte].encode_utf16().count() as u32;
        self.last.set((byte, units));
        DocOffset(units)
    }

    pub fn to_byte(&self, off: DocOffset) -> usize {
        if self.one_unit_per_byte {
            return (off.0 as usize).min(self.text.len());
        }
        let (start, mut units) = match self.last.get() {
            (b, u) if u <= off.0 => (b, u),
            _ => (0, 0),
        };
        for (i, ch) in self.text[start..].char_indices() {
            if units + ch.len_utf16() as u32 > off.0 {
                self.last.set((start + i, units));
                return start + i;
            }
            units += ch.len_utf16() as u32;
        }
        self.last.set((self.text.len(), units));
        self.text.len()
    }

    pub fn target(&self, target: RunTarget) -> ByteTarget {
        match target {
            RunTarget::All => ByteTarget::All,
            RunTarget::Cursor { offset } => ByteTarget::Cursor(self.to_byte(offset)),
            RunTarget::Selection { from, to } => {
                ByteTarget::Selection { from: self.to_byte(from), to: self.to_byte(to) }
            }
        }
    }

    pub fn range(&self, span: &StmtSpan) -> DocRange {
        DocRange { from: self.to_utf16(span.start), to: self.to_utf16(span.end) }
    }

    /// PG `ErrorPosition::Original(n)` is a 1-based character index within the statement.
    pub fn pg_position(&self, stmt: &StmtSpan, n: u32) -> DocOffset {
        let byte =
            stmt.text(self.text).char_indices().nth((n.max(1) - 1) as usize).map_or(stmt.end, |(b, _)| stmt.start + b);
        self.to_utf16(byte)
    }
}

/// `at_cursor` comes from `sql::pick`, so it names the statement a `Cursor` run would execute.
pub fn script_outline(text: &str, cursor: DocOffset, database_kind: DatabaseKind) -> ScriptOutline {
    if database_kind == DatabaseKind::SqlServer {
        let map = OffsetMap::new(text);
        let spans = sql::tsql::split_batches(text).unwrap_or_default();
        let picked = sql::tsql::pick(text, ByteTarget::Cursor(map.to_byte(cursor))).unwrap_or_default();
        return ScriptOutline {
            unit_kind: ExecutionUnitKind::Batch,
            statements: spans.iter().map(|s| map.range(s)).collect(),
            at_cursor: picked
                .first()
                .and_then(|p| spans.iter().position(|s| s == p))
                .and_then(|i| u16::try_from(i).ok()),
        };
    }
    let map = OffsetMap::new(text);
    let spans = sql::split_statements(text);
    let picked = sql::pick(&spans, text, ByteTarget::Cursor(map.to_byte(cursor)));
    ScriptOutline {
        unit_kind: crate::model::ExecutionUnitKind::Statement,
        statements: spans.iter().map(|s| map.range(s)).collect(),
        at_cursor: picked.first().and_then(|p| spans.iter().position(|s| s == p)).and_then(|i| u16::try_from(i).ok()),
    }
}

pub fn cell_references(text: &str, database_kind: DatabaseKind) -> Vec<CellReference> {
    let map = OffsetMap::new(text);
    sql::refs::find(text, database_kind)
        .into_iter()
        .map(|(span, name)| CellReference {
            range: DocRange { from: map.to_utf16(span.start), to: map.to_utf16(span.end) },
            name: name.to_owned(),
        })
        .collect()
}

pub fn completion_context(text: &str, cursor: DocOffset, database_kind: DatabaseKind) -> CompletionContext {
    let map = OffsetMap::new(text);
    let at = map.to_byte(cursor);
    if let Some(from) = sql::refs::typing_at(text, at, database_kind) {
        return CompletionContext::Reference { from: map.to_utf16(from) };
    }
    let Some(scope) = sql::scope_at(text, at, database_kind) else {
        return CompletionContext::None;
    };
    CompletionContext::Complete {
        from: map.to_utf16(scope.from_byte),
        expect: scope.expect,
        qualifier: scope.qualifier,
        relations: scope.relations,
        ctes: scope.ctes,
        keywords: scope.keywords.into_iter().map(str::to_owned).collect(),
        functions: scope.functions.into_iter().map(str::to_owned).collect(),
    }
}

/// A cursor formats the statement a `Cursor` run would execute, a selection every statement it
/// touches, and `All` the document. Each statement is formatted on its own; the text between them,
/// `;` included, is kept.
pub fn format_edit(text: &str, target: RunTarget, database_kind: DatabaseKind) -> Result<FormatEdit, FormatError> {
    if database_kind == DatabaseKind::SqlServer {
        let map = OffsetMap::new(text);
        let spans = sql::tsql::pick(text, map.target(target)).map_err(|_| FormatError::Unsupported)?;
        let (Some(first), Some(last)) = (spans.first(), spans.last()) else {
            return Err(FormatError::NothingToFormat);
        };
        let insert = sql::tsql::format(&text[first.start..last.end]).map_err(|_| FormatError::Unsupported)?;
        return Ok(FormatEdit { from: map.to_utf16(first.start), to: map.to_utf16(last.end), insert });
    }
    let map = OffsetMap::new(text);
    let spans = sql::split_statements(text);
    let picked = match map.target(target) {
        ByteTarget::Selection { from, to } if from < to => {
            spans.into_iter().filter(|s| s.start < to && from < s.end).collect()
        }
        target => sql::pick(&spans, text, target),
    };
    let (Some(first), Some(last)) = (picked.first(), picked.last()) else {
        return Err(FormatError::NothingToFormat);
    };
    let mut insert = String::new();
    let mut at = first.start;
    for span in &picked {
        insert.push_str(&text[at..span.start]);
        insert.push_str(&sql::format_statement(span.text(text))?);
        at = span.end;
    }
    Ok(FormatEdit { from: map.to_utf16(first.start), to: map.to_utf16(last.end), insert })
}

/// tokio-postgres exposes only the row count of a command completion, so the tag is rebuilt
/// from the classified verb the way Postgres would print it.
pub fn synthesize_tag(kind: &StmtKind, rows: u64) -> String {
    match kind {
        StmtKind::Begin => "BEGIN".into(),
        StmtKind::Commit | StmtKind::CommitAndChain => "COMMIT".into(),
        StmtKind::Rollback | StmtKind::RollbackTo | StmtKind::RollbackAndChain => "ROLLBACK".into(),
        StmtKind::Savepoint => "SAVEPOINT".into(),
        StmtKind::Release => "RELEASE".into(),
        StmtKind::Ddl(tag) | StmtKind::NoTxnBlock(tag) | StmtKind::Other(tag) => tag.clone(),
        StmtKind::Dml(DmlVerb::Insert) => format!("INSERT 0 {rows}"),
        StmtKind::Dml(DmlVerb::Update) => format!("UPDATE {rows}"),
        StmtKind::Dml(DmlVerb::Delete) => format!("DELETE {rows}"),
        StmtKind::Dml(DmlVerb::Merge) => format!("MERGE {rows}"),
        StmtKind::Query => format!("SELECT {rows}"),
        StmtKind::CopyStdio => "COPY".into(),
    }
}

pub fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

pub fn quote_if_needed(kind: DatabaseKind, name: &str) -> String {
    name_in(kind, name, &[sql::RESERVED, sql::TYPE_OR_FUNCTION_NAME])
}

pub fn quote_function_if_needed(kind: DatabaseKind, name: &str) -> String {
    name_in(kind, name, &[sql::NEVER_A_FUNCTION])
}

fn name_in(kind: DatabaseKind, name: &str, postgres_keywords: &[&[&str]]) -> String {
    let mut chars = name.chars();
    match kind {
        DatabaseKind::Postgres => {
            let plain = chars.next().is_some_and(|c| c.is_ascii_lowercase() || c == '_')
                && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '$');
            if plain && !postgres_keywords.iter().any(|k| k.contains(&name)) {
                name.to_owned()
            } else {
                quote_ident(name)
            }
        }
        DatabaseKind::SqlServer => {
            let plain = chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '@' | '$' | '#'));
            if plain && !sql::tsql::RESERVED.contains(&name.to_ascii_uppercase().as_str()) {
                name.to_owned()
            } else {
                sql::tsql::quote_ident(name)
            }
        }
    }
}

pub fn quote_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// An `E'…'` literal, which reads a backslash the same whatever `standard_conforming_strings` is.
pub fn quote_escaped_literal(value: &str) -> String {
    format!("E'{}'", value.replace('\\', "\\\\").replace('\'', "''"))
}

pub fn quote_ident_in(kind: DatabaseKind, name: &str) -> String {
    match kind {
        DatabaseKind::Postgres => quote_ident(name),
        DatabaseKind::SqlServer => sql::tsql::quote_ident(name),
    }
}

pub fn quote_literal_in(kind: DatabaseKind, value: &str) -> String {
    match kind {
        DatabaseKind::Postgres => quote_literal(value),
        DatabaseKind::SqlServer => sql::tsql::quote_literal(value),
    }
}

fn like_pattern(value: &str, prefix_only: bool) -> String {
    let escaped = value.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
    quote_literal(&if prefix_only { format!("{escaped}%") } else { format!("%{escaped}%") })
}

fn filter_sql(f: &ColumnFilter) -> String {
    let col = quote_ident(&f.column);
    let lit = || quote_literal(&f.value);
    match f.op {
        FilterOp::Eq => format!("{col} = {}", lit()),
        FilterOp::Ne => format!("{col} <> {}", lit()),
        FilterOp::Lt => format!("{col} < {}", lit()),
        FilterOp::Le => format!("{col} <= {}", lit()),
        FilterOp::Gt => format!("{col} > {}", lit()),
        FilterOp::Ge => format!("{col} >= {}", lit()),
        FilterOp::Contains => format!("{col}::text ILIKE {}", like_pattern(&f.value, false)),
        FilterOp::StartsWith => format!("{col}::text ILIKE {}", like_pattern(&f.value, true)),
        FilterOp::IsNull => format!("{col} IS NULL"),
        FilterOp::NotNull => format!("{col} IS NOT NULL"),
    }
}

fn where_sql(q: &TableQuery) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !q.r#where.trim().is_empty() {
        parts.push(format!("({}\n)", q.r#where.trim()));
    }
    parts.extend(q.filters.iter().map(filter_sql));
    if parts.is_empty() { String::new() } else { format!(" WHERE {}", parts.join(" AND ")) }
}

fn from_sql(schema: &str, table: &str, q: &TableQuery) -> String {
    format!("FROM {}.{}{}", quote_ident(schema), quote_ident(table), where_sql(q))
}

pub fn browse_sql(schema: &str, table: &str, pk: &[String], q: &TableQuery, window: Option<(u32, u64)>) -> String {
    let mut sql = format!("SELECT * {}", from_sql(schema, table, q));
    let mut order: Vec<String> = Vec::new();
    if !q.order_by.trim().is_empty() {
        order.push(format!("{}\n", q.order_by.trim()));
    }
    let tiebreak_so_pages_never_overlap = pk.iter().map(|c| quote_ident(c));
    order.extend(tiebreak_so_pages_never_overlap);
    if !order.is_empty() {
        sql.push_str(&format!(" ORDER BY {}", order.join(", ")));
    }
    if let Some((limit, offset)) = window {
        sql.push_str(&format!(" LIMIT {limit} OFFSET {offset}"));
    }
    sql
}

pub fn count_sql(schema: &str, table: &str, q: &TableQuery) -> String {
    format!("SELECT count(*) {}", from_sql(schema, table, q))
}

fn value_sql(value: &Option<String>) -> String {
    value.as_deref().map_or_else(|| "NULL".into(), quote_literal)
}

fn key_sql(key: &[CellWrite]) -> String {
    key.iter()
        .map(|k| match &k.value {
            Some(v) => format!("{} = {}", quote_ident(&k.column), quote_literal(v)),
            None => format!("{} IS NULL", quote_ident(&k.column)),
        })
        .collect::<Vec<_>>()
        .join(" AND ")
}

fn target(schema: &str, table: &str) -> String {
    format!("{}.{}", quote_ident(schema), quote_ident(table))
}

pub fn insert_sql(schema: &str, table: &str, cells: &[CellWrite]) -> String {
    if cells.is_empty() {
        return format!("INSERT INTO {} DEFAULT VALUES", target(schema, table));
    }
    let cols: Vec<String> = cells.iter().map(|c| quote_ident(&c.column)).collect();
    let vals: Vec<String> = cells.iter().map(|c| value_sql(&c.value)).collect();
    format!("INSERT INTO {} ({}) VALUES ({})", target(schema, table), cols.join(", "), vals.join(", "))
}

pub fn update_sql(schema: &str, table: &str, key: &[CellWrite], set: &[CellWrite]) -> String {
    let assigns: Vec<String> =
        set.iter().map(|c| format!("{} = {}", quote_ident(&c.column), value_sql(&c.value))).collect();
    format!("UPDATE {} SET {} WHERE {}", target(schema, table), assigns.join(", "), key_sql(key))
}

pub fn delete_sql(schema: &str, table: &str, key: &[CellWrite]) -> String {
    format!("DELETE FROM {} WHERE {}", target(schema, table), key_sql(key))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn script_outline(text: &str, cursor: DocOffset) -> ScriptOutline {
        super::script_outline(text, cursor, DatabaseKind::Postgres)
    }

    fn r(from: u32, to: u32) -> DocRange {
        DocRange { from: DocOffset(from), to: DocOffset(to) }
    }

    #[test]
    fn tsql_format_edit_keeps_dml_cursor_and_selection_scope_with_utf16_offsets() {
        let text = "-- 😀\nSELECT  1;\nGO -- boundary\ninsert dbo.t(id) select id from s; update dbo.t set n=@n where id=1;\nGO\nDELETE FROM t WHERE id=2;\n";
        let map = OffsetMap::new(text);
        let start = text.find("insert").unwrap();
        let end = text.find(";\nGO\n").unwrap() + 1;
        let expected = "insert dbo.t(id)\nselect id\nfrom s;\nupdate dbo.t\nset n = @n\nwhere id = 1;";
        for target in
            [cursor_at(text, "set"), RunTarget::Selection { from: map.to_utf16(start), to: map.to_utf16(end) }]
        {
            let edit = format_edit(text, target, DatabaseKind::SqlServer).unwrap();
            assert_eq!(edit.from, map.to_utf16(start));
            assert_eq!(edit.to, map.to_utf16(end));
            assert_eq!(edit.insert, expected);
        }
        let selection = RunTarget::Selection { from: map.to_utf16(start), to: map.to_utf16(text.trim_end().len()) };
        let edit = format_edit(text, selection, DatabaseKind::SqlServer).unwrap();
        assert_eq!(edit.from, map.to_utf16(start));
        assert_eq!(edit.insert, format!("{expected}\nGO\nDELETE FROM t\nWHERE id = 2;"));
        let edit = format_edit(text, RunTarget::All, DatabaseKind::SqlServer).unwrap();
        assert_eq!(
            edit.insert,
            format!("-- 😀\nSELECT 1;\nGO -- boundary\n{expected}\nGO\nDELETE FROM t\nWHERE id = 2;")
        );
        let fragment = RunTarget::Selection { from: map.to_utf16(start + 3), to: map.to_utf16(start + 8) };
        assert!(matches!(format_edit(text, fragment, DatabaseKind::SqlServer), Err(FormatError::Unsupported)));
        let unsafe_batch = "UPDATE t SET n=1; SET NOCOUNT ON";
        assert!(matches!(
            format_edit(unsafe_batch, cursor_at(unsafe_batch, "UPDATE"), DatabaseKind::SqlServer),
            Err(FormatError::Unsupported)
        ));
    }

    #[test]
    fn a_name_is_quoted_only_where_the_bare_word_would_read_differently() {
        let pg = |name| quote_if_needed(DatabaseKind::Postgres, name);
        assert_eq!(
            ["orders", "order$1", "Line Items", "a\"b", "order", "Orders", "left", "int", "café"].map(pg),
            [
                "orders",
                "order$1",
                "\"Line Items\"",
                "\"a\"\"b\"",
                "\"order\"",
                "\"Orders\"",
                "\"left\"",
                "int",
                "\"café\""
            ]
        );
        let pg_call = |name| quote_function_if_needed(DatabaseKind::Postgres, name);
        assert_eq!(["answer", "MyFunc", "int", "left"].map(pg_call), ["answer", "\"MyFunc\"", "\"int\"", "left"]);
        let ms = |name| quote_if_needed(DatabaseKind::SqlServer, name);
        assert_eq!(
            ["Orders", "line items", "a]b", "order", "Order", "x#1", "9lives"].map(ms),
            ["Orders", "[line items]", "[a]]b]", "[order]", "[Order]", "x#1", "[9lives]"]
        );
    }

    #[test]
    fn postgres_table_templates_keep_saved_browse_tabs_recognizable() {
        for (schema, table, expected) in [
            ("public", "orders", "SELECT * FROM public.orders"),
            ("_sales", "order$items", "SELECT * FROM _sales.order$items"),
            ("Sales", "line items", "SELECT * FROM \"Sales\".\"line items\""),
            ("public", "select", "SELECT * FROM public.\"select\""),
            ("current_schema", "a\"b", "SELECT * FROM \"current_schema\".\"a\"\"b\""),
        ] {
            let script = table_script(DatabaseKind::Postgres, schema, table);
            assert_eq!(script.text, expected);
            assert_eq!(script.accepted_texts, [expected.to_owned(), format!("{expected};")]);
        }
        let script = table_script(DatabaseKind::SqlServer, "dbo", "orders");
        assert_eq!(script.text, "SELECT * FROM [dbo].[orders]");
        assert_eq!(script.accepted_texts, ["SELECT * FROM [dbo].[orders]"]);
    }

    #[test]
    fn script_outline_lists_every_statement_and_preselects_the_one_under_the_cursor() {
        let text = "select 1 as one;\nselect * from public.orders limit 3;\nselect 'third' as three;";
        assert_eq!(
            script_outline(text, DocOffset(30)),
            ScriptOutline {
                unit_kind: crate::model::ExecutionUnitKind::Statement,
                statements: vec![r(0, 15), r(17, 52), r(54, 77)],
                at_cursor: Some(1)
            }
        );
    }

    #[test]
    fn script_outline_at_trailing_whitespace_picks_the_previous_statement() {
        let text = "select 1;   \n\nselect 2";
        assert_eq!(
            script_outline(text, DocOffset(11)),
            ScriptOutline {
                unit_kind: crate::model::ExecutionUnitKind::Statement,
                statements: vec![r(0, 8), r(14, 22)],
                at_cursor: Some(0)
            }
        );
    }

    #[test]
    fn script_outline_splits_the_users_doc_at_its_blank_line() {
        let text = "SELECT * FROM public.addresses\nWHERE id > 5\nAND id < 10\nORDER BY id DESC\n\nselect * from public.categories";
        assert_eq!(
            script_outline(text, DocOffset(80)),
            ScriptOutline {
                unit_kind: crate::model::ExecutionUnitKind::Statement,
                statements: vec![r(0, 72), r(74, 105)],
                at_cursor: Some(1)
            }
        );
        assert_eq!(script_outline(text, DocOffset(35)).at_cursor, Some(0));
    }

    #[test]
    fn script_outline_of_a_doc_without_statements_is_empty() {
        let none = ScriptOutline {
            unit_kind: crate::model::ExecutionUnitKind::Statement,
            statements: vec![],
            at_cursor: None,
        };
        assert_eq!(script_outline("", DocOffset(0)), none);
        assert_eq!(script_outline("  -- nothing\n/* here */\n", DocOffset(5)), none);
    }

    #[test]
    fn script_outline_ranges_and_cursor_are_utf16() {
        let text = "-- 😀é\nselect 1;\nselect 2;";
        assert_eq!(
            script_outline(text, DocOffset(18)),
            ScriptOutline {
                unit_kind: crate::model::ExecutionUnitKind::Statement,
                statements: vec![r(0, 15), r(17, 25)],
                at_cursor: Some(1)
            }
        );
    }

    #[test]
    fn offsets_convert_the_same_in_any_order() {
        let text = "-- é\nselect '😀', 'ü';\nselect 2;";
        let map = OffsetMap::new(text);
        let bytes: Vec<usize> = text.char_indices().map(|(b, _)| b).chain([text.len()]).collect();
        for &b in bytes.iter().rev().chain(bytes.iter()).chain(bytes.iter().step_by(3).rev()) {
            let units = map.to_utf16(b);
            assert_eq!(units, OffsetMap::new(text).to_utf16(b), "to_utf16({b})");
            assert_eq!(map.to_byte(units), b, "to_byte({units:?})");
        }
        assert_eq!(map.to_byte(DocOffset(8)), OffsetMap::new(text).to_byte(DocOffset(8)));
    }

    #[test]
    fn completion_context_reads_and_reports_utf16_offsets() {
        let text = "select '😀' as e, o.cu from orders o";
        let CompletionContext::Complete { from, qualifier, .. } =
            completion_context(text, DocOffset(22), DatabaseKind::Postgres)
        else {
            panic!("a column after o. completes");
        };
        assert_eq!(from, DocOffset(20));
        assert_eq!(qualifier, Some(crate::model::Qualifier::Relation { index: 0 }));
        assert_eq!(completion_context(text, DocOffset(10), DatabaseKind::Postgres), CompletionContext::None);
    }

    fn writes(text: &str, target: RunTarget, kind: DatabaseKind) -> Vec<&str> {
        let spans = pick(text, OffsetMap::new(text).target(target), kind).unwrap_or_default();
        spans.iter().map(|span| span.text(text)).filter(|sql| is_write(sql, kind)).collect()
    }

    fn cursor_at(text: &str, needle: &str) -> RunTarget {
        RunTarget::Cursor { offset: OffsetMap::new(text).to_utf16(text.find(needle).unwrap()) }
    }

    #[test]
    fn only_the_postgres_statements_a_run_executes_that_write_count() {
        let pg = DatabaseKind::Postgres;
        assert_eq!(writes("select * from orders", RunTarget::All, pg), Vec::<&str>::new());
        assert_eq!(
            writes("update orders set n = 1 where id = 7", RunTarget::All, pg),
            ["update orders set n = 1 where id = 7"]
        );
        let script = "select 1;\nupdate orders set n = 1;\nselect 2;\ndelete from orders;\nwith x as (select 1) select * from x;";
        assert_eq!(writes(script, RunTarget::All, pg), ["update orders set n = 1", "delete from orders"]);
        assert_eq!(writes(script, cursor_at(script, "update"), pg), ["update orders set n = 1"]);
        assert_eq!(writes(script, cursor_at(script, "select 2"), pg), Vec::<&str>::new());
        let from = OffsetMap::new(script).to_utf16(script.find("select 2").unwrap());
        let to = OffsetMap::new(script).to_utf16(script.find("with").unwrap());
        assert_eq!(writes(script, RunTarget::Selection { from, to }, pg), ["delete from orders"]);
    }

    #[test]
    fn only_the_sql_server_batches_a_run_executes_that_write_count() {
        let ms = DatabaseKind::SqlServer;
        assert_eq!(writes("SELECT * FROM dbo.orders", RunTarget::All, ms), Vec::<&str>::new());
        assert_eq!(writes("UPDATE dbo.orders SET n = 1", RunTarget::All, ms), ["UPDATE dbo.orders SET n = 1"]);
        let script = "SELECT 1\nGO\nUPDATE dbo.orders SET n = 1\nGO\nSELECT 2\nGO\nEXEC dbo.purge\nGO";
        assert_eq!(writes(script, RunTarget::All, ms), ["UPDATE dbo.orders SET n = 1", "EXEC dbo.purge"]);
        assert_eq!(writes(script, cursor_at(script, "UPDATE"), ms), ["UPDATE dbo.orders SET n = 1"]);
        assert_eq!(writes(script, cursor_at(script, "SELECT 2"), ms), Vec::<&str>::new());
        assert_eq!(
            writes("USE shop\nGO\nDELETE FROM dbo.orders", RunTarget::All, ms),
            Vec::<&str>::new(),
            "the run refuses USE, so it deletes nothing"
        );
    }

    #[test]
    fn utf16_offsets_round_trip_past_emoji_and_cjk() {
        let text = "select '😀漢'; select x";
        let map = OffsetMap::new(text);
        let x = text.find("select x").unwrap();
        assert_eq!(map.to_utf16(x), DocOffset(14));
        assert_eq!(map.to_byte(DocOffset(14)), x);
        assert_eq!(map.to_byte(DocOffset(9)), 8, "inside the surrogate pair snaps to its start");
        assert_eq!(map.to_byte(DocOffset(999)), text.len());
        assert_eq!(map.to_utf16(999), DocOffset(text.encode_utf16().count() as u32));
    }

    #[test]
    fn pg_position_is_one_based_chars_within_the_statement() {
        let text = "select 1;\nselect '😀', nope from t";
        let spans = crate::sql::split_statements(text);
        let map = OffsetMap::new(text);
        let stmt = spans[1].text(text);
        let nope_char = stmt[..stmt.find("nope").unwrap()].chars().count() as u32 + 1;
        let doc = map.pg_position(&spans[1], nope_char);
        let expected = text[..text.find("nope").unwrap()].encode_utf16().count() as u32;
        assert_eq!(doc, DocOffset(expected));
        assert_eq!(map.pg_position(&spans[0], 1), DocOffset(0));
        assert_eq!(map.pg_position(&spans[0], 0), DocOffset(0));
        assert_eq!(map.pg_position(&spans[0], 500), map.to_utf16(spans[0].end));
    }

    #[test]
    fn tags() {
        assert_eq!(synthesize_tag(&StmtKind::Dml(DmlVerb::Insert), 3), "INSERT 0 3");
        assert_eq!(synthesize_tag(&StmtKind::Dml(DmlVerb::Update), 3), "UPDATE 3");
        assert_eq!(synthesize_tag(&StmtKind::Query, 6), "SELECT 6");
        assert_eq!(synthesize_tag(&StmtKind::Ddl("CREATE TABLE".into()), 0), "CREATE TABLE");
        assert_eq!(synthesize_tag(&StmtKind::Begin, 0), "BEGIN");
        assert_eq!(synthesize_tag(&StmtKind::Other("SET".into()), 0), "SET");
    }

    #[test]
    fn browse_sql_quotes_identifiers_and_orders_by_pk() {
        let none = TableQuery::default();
        assert_eq!(
            browse_sql("public", "orders", &["id".into()], &none, Some((500, 1000))),
            r#"SELECT * FROM "public"."orders" ORDER BY "id" LIMIT 500 OFFSET 1000"#
        );
        assert_eq!(
            browse_sql("we\"ird", "T;x", &["a\"b".into(), "c".into()], &none, Some((10, 0))),
            r#"SELECT * FROM "we""ird"."T;x" ORDER BY "a""b", "c" LIMIT 10 OFFSET 0"#
        );
        assert_eq!(browse_sql("s", "t", &[], &none, Some((5, 0))), r#"SELECT * FROM "s"."t" LIMIT 5 OFFSET 0"#);
        assert_eq!(
            browse_sql("public", "orders", &["id".into()], &none, None),
            r#"SELECT * FROM "public"."orders" ORDER BY "id""#
        );
        assert_eq!(browse_sql("s", "t", &[], &none, None), r#"SELECT * FROM "s"."t""#);
    }

    fn filter(column: &str, op: FilterOp, value: &str) -> ColumnFilter {
        ColumnFilter { column: column.into(), op, value: value.into() }
    }

    #[test]
    fn browse_sql_puts_the_order_by_text_before_the_pk_tiebreak() {
        let q = TableQuery { order_by: " lower(name) DESC NULLS LAST ".into(), ..Default::default() };
        assert_eq!(
            browse_sql("public", "customers", &["id".into(), "tenant".into()], &q, Some((50, 0))),
            "SELECT * FROM \"public\".\"customers\" ORDER BY lower(name) DESC NULLS LAST\n, \"id\", \"tenant\" LIMIT 50 OFFSET 0"
        );
    }

    #[test]
    fn browse_sql_ands_the_where_text_with_column_filters() {
        let q = TableQuery {
            r#where: " created_at > now() - interval '7 days' ".into(),
            filters: vec![filter("region", FilterOp::Eq, "O'Hare"), filter("email", FilterOp::NotNull, "")],
            ..Default::default()
        };
        assert_eq!(
            browse_sql("public", "customers", &[], &q, Some((10, 0))),
            "SELECT * FROM \"public\".\"customers\" WHERE (created_at > now() - interval '7 days'\n) AND \"region\" = 'O''Hare' AND \"email\" IS NOT NULL LIMIT 10 OFFSET 0"
        );
        assert_eq!(
            count_sql("public", "customers", &q),
            "SELECT count(*) FROM \"public\".\"customers\" WHERE (created_at > now() - interval '7 days'\n) AND \"region\" = 'O''Hare' AND \"email\" IS NOT NULL"
        );
    }

    #[test]
    fn every_filter_op_compiles_and_like_patterns_escape_wildcards() {
        let ops = [
            (FilterOp::Eq, r#""n" = '5'"#),
            (FilterOp::Ne, r#""n" <> '5'"#),
            (FilterOp::Lt, r#""n" < '5'"#),
            (FilterOp::Le, r#""n" <= '5'"#),
            (FilterOp::Gt, r#""n" > '5'"#),
            (FilterOp::Ge, r#""n" >= '5'"#),
            (FilterOp::Contains, r#""n"::text ILIKE '%5%'"#),
            (FilterOp::StartsWith, r#""n"::text ILIKE '5%'"#),
            (FilterOp::IsNull, r#""n" IS NULL"#),
            (FilterOp::NotNull, r#""n" IS NOT NULL"#),
        ];
        for (op, sql) in ops {
            assert_eq!(filter_sql(&filter("n", op, "5")), sql, "{op:?}");
        }
        assert_eq!(filter_sql(&filter("n", FilterOp::Contains, r"50%_a\b")), r#""n"::text ILIKE '%50\%\_a\\b%'"#);
    }

    fn cell(column: &str, value: Option<&str>) -> CellWrite {
        CellWrite { column: column.into(), value: value.map(Into::into) }
    }

    #[test]
    fn edit_sql_quotes_names_and_values_and_writes_null() {
        assert_eq!(
            insert_sql("public", "customers", &[cell("name", Some("O'Neil")), cell("region", None)]),
            r#"INSERT INTO "public"."customers" ("name", "region") VALUES ('O''Neil', NULL)"#
        );
        assert_eq!(insert_sql("public", "customers", &[]), r#"INSERT INTO "public"."customers" DEFAULT VALUES"#);
        assert_eq!(
            update_sql(
                "public",
                "line items",
                &[cell("order_id", Some("7")), cell("sku", Some("A-1"))],
                &[cell("qty", Some("3")), cell("note", None)]
            ),
            r#"UPDATE "public"."line items" SET "qty" = '3', "note" = NULL WHERE "order_id" = '7' AND "sku" = 'A-1'"#
        );
        assert_eq!(
            delete_sql("public", "customers", &[cell("id", Some("42"))]),
            r#"DELETE FROM "public"."customers" WHERE "id" = '42'"#
        );
    }
}

pub fn pick(text: &str, target: ByteTarget, kind: DatabaseKind) -> Result<Vec<StmtSpan>, String> {
    match kind {
        DatabaseKind::Postgres => Ok(sql::pick(&sql::split_statements(text), text, target)),
        DatabaseKind::SqlServer => sql::tsql::pick(text, target),
    }
}
pub fn classify(text: &str, kind: DatabaseKind) -> StmtKind {
    match kind {
        DatabaseKind::Postgres => sql::classify(text),
        DatabaseKind::SqlServer => sql::tsql::classify(text),
    }
}
pub fn is_write(text: &str, kind: DatabaseKind) -> bool {
    match kind {
        DatabaseKind::Postgres => sql::history_is_write(text),
        DatabaseKind::SqlServer => sql::tsql::is_write(text),
    }
}

pub fn quote_ident_if_needed(name: &str) -> String {
    let mut chars = name.chars();
    let simple = chars.next().is_some_and(|c| c.is_ascii_lowercase() || c == '_')
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '$');
    if simple && !sql::RESERVED.contains(&name) && !sql::TYPE_OR_FUNCTION_NAME.contains(&name) {
        name.to_owned()
    } else {
        quote_ident(name)
    }
}

pub fn table_script(kind: DatabaseKind, schema: &str, table: &str) -> TableScript {
    let quote = match kind {
        DatabaseKind::Postgres => quote_ident_if_needed,
        DatabaseKind::SqlServer => sql::tsql::quote_ident,
    };
    let text = format!("SELECT * FROM {}.{}", quote(schema), quote(table));
    let mut accepted_texts = vec![text.clone()];
    if kind == DatabaseKind::Postgres {
        accepted_texts.push(format!("{text};"));
    }
    TableScript { text, accepted_texts }
}

pub fn table_order(
    kind: DatabaseKind,
    relation: Option<&Relation>,
    order_by: &str,
    edit: TableOrderEdit,
) -> Result<TableOrderView, String> {
    let parsed = match kind {
        DatabaseKind::Postgres => sql::order::parse(order_by),
        DatabaseKind::SqlServer => sql::tsql::parse_order_by(order_by),
    };
    if kind == DatabaseKind::SqlServer {
        let known = |name: &str| relation.is_some_and(|r| r.columns.iter().any(|c| c.name == name));
        let resolved =
            parsed.as_ref().is_some_and(|keys| keys.iter().all(|(name, _)| known(name))) && relation.is_some();
        match &edit {
            TableOrderEdit::Inspect if !resolved => return Ok(TableOrderView { sql: order_by.to_owned(), keys: None }),
            TableOrderEdit::Cycle { column, additive } => {
                if relation.is_none() {
                    return Err(
                        "Table columns are unavailable. Connect and refresh the table catalog before sorting.".into()
                    );
                }
                if !known(column) {
                    return Err(format!(
                        "Column {column:?} is not in this table's catalog. Refresh the catalog before sorting."
                    ));
                }
                if *additive && !resolved {
                    return Err("To add a header sort, ORDER BY must use exact table column names. Edit ORDER BY, or click without Shift to replace a simple column order.".into());
                }
            }
            TableOrderEdit::Inspect => {}
        }
    }
    let Some(mut keys) = parsed else {
        return Ok(TableOrderView { sql: order_by.to_owned(), keys: None });
    };
    if let TableOrderEdit::Cycle { column, additive } = edit {
        let previous = keys.iter().find(|(name, _)| *name == column).map(|(_, desc)| *desc);
        if !additive {
            keys.retain(|(name, _)| *name == column);
        }
        match previous {
            None => keys.push((column, false)),
            Some(false) => {
                if let Some((_, desc)) = keys.iter_mut().find(|(name, _)| *name == column) {
                    *desc = true;
                }
            }
            Some(true) => keys.retain(|(name, _)| *name != column),
        }
    } else {
        return Ok(TableOrderView {
            sql: order_by.to_owned(),
            keys: Some(keys.into_iter().map(|(column, desc)| TableSortKey { column, desc }).collect()),
        });
    }
    let sql = match kind {
        DatabaseKind::Postgres => keys
            .iter()
            .map(|(column, desc)| format!("{}{}", quote_ident(column), if *desc { " DESC" } else { "" }))
            .collect::<Vec<_>>()
            .join(", "),
        DatabaseKind::SqlServer => sql::tsql::format_order_by(&keys),
    };
    Ok(TableOrderView {
        sql,
        keys: Some(keys.into_iter().map(|(column, desc)| TableSortKey { column, desc }).collect()),
    })
}

pub fn table_browse(
    kind: DatabaseKind,
    schema: &str,
    table: &str,
    relation: Option<&Relation>,
    query: &TableQuery,
    window: Option<(u32, u64)>,
) -> Result<String, String> {
    let pk = relation.map(Relation::pk_columns).unwrap_or_default();
    match kind {
        DatabaseKind::Postgres => {
            let text = browse_sql(schema, table, &pk, query, window);
            if sql::split_statements(&text).len() != 1 {
                return Err("The filter must be a single expression.".into());
            }
            Ok(text)
        }
        DatabaseKind::SqlServer => {
            sql::tsql::validate_expression(&query.r#where, false)?;
            sql::tsql::validate_expression(&query.order_by, true)?;
            let projection = relation
                .map(|r| r.columns.iter().map(|c| sql::tsql::quote_ident(&c.name)).collect::<Vec<_>>().join(", "))
                .filter(|p| !p.is_empty())
                .unwrap_or_else(|| "*".into());
            sql::tsql::browse_sql(
                schema,
                table,
                &projection,
                relation.map(|r| r.columns.as_slice()).unwrap_or_default(),
                &pk,
                query,
                window,
            )
        }
    }
}

pub fn table_count(
    kind: DatabaseKind,
    schema: &str,
    table: &str,
    relation: Option<&Relation>,
    query: &TableQuery,
) -> Result<String, String> {
    match kind {
        DatabaseKind::Postgres => {
            let text = count_sql(schema, table, query);
            if sql::split_statements(&text).len() != 1 {
                Err("The filter must be a single expression.".into())
            } else {
                Ok(text)
            }
        }
        DatabaseKind::SqlServer => {
            sql::tsql::validate_expression(&query.r#where, false)?;
            sql::tsql::count_sql(schema, table, relation.map(|r| r.columns.as_slice()).unwrap_or_default(), query)
        }
    }
}

#[cfg(test)]
mod completion_never_panics {
    use crate::model::{DatabaseKind, DocOffset};

    const TOKENS: &[&str] = &[
        "select",
        "from",
        "where",
        "join",
        "on",
        "using",
        "natural",
        "with",
        "recursive",
        "as",
        "lateral",
        "apply",
        "cross",
        "outer",
        "insert",
        "into",
        "values",
        "update",
        "set",
        "delete",
        "top",
        "percent",
        "merge",
        "output",
        "returning",
        "union",
        "group",
        "by",
        "order",
        "pivot",
        "unpivot",
        "for",
        "nowait",
        "go",
        "begin",
        "end",
        "case",
        "over",
        "window",
        "isnull",
        "interval",
        "o",
        "orders",
        "\"Q\"",
        "[b]",
        "[a]]b]",
        "'s'",
        "'open",
        "N'x'",
        "$$x$$",
        "$$open",
        "--c\n",
        "/*c*/",
        "/*open",
        "@v",
        "#t",
        "$1",
        "1",
        "1e",
        "0x1",
        "*",
        "=",
        "::",
        ".",
        "..",
        ",",
        ";",
        "(",
        ")",
        "[",
        "]",
        "+",
        "U&\"x\"",
        "E'a\\'b'",
        "😀",
        "é",
        "\n",
        "\nGO\n",
        "GO 5\n",
        " ",
        "\t",
        "x join y join (a join b) pivot (max(v) for k in ([a])) p",
        "s(v, i)",
        "with ordinality",
        "o(a, b)",
        "on",
        "returning",
    ];

    #[test]
    fn completion_context_never_panics_on_any_token_sequence_or_cursor() {
        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..8_000 {
            let len = (next() % 10) as usize;
            let mut text =
                String::from(["", "select ", "select * from t ", "with x as (select "][(next() % 4) as usize]);
            for _ in 0..len {
                text.push_str(TOKENS[(next() % TOKENS.len() as u64) as usize]);
                if next() % 3 != 0 {
                    text.push(' ');
                }
            }
            let units = text.encode_utf16().count() as u32;
            for cursor in (0..=units).chain([units + 3]) {
                for kind in [DatabaseKind::Postgres, DatabaseKind::SqlServer] {
                    let outcome =
                        std::panic::catch_unwind(|| super::completion_context(&text, DocOffset(cursor), kind));
                    assert!(outcome.is_ok(), "{kind:?} panicked at {cursor} in {text:?}");
                }
            }
        }
    }
}
