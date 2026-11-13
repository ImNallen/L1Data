use crate::model::{AssistantAccess, Attachment, Catalog, DatabaseKind, DatabaseRef, RelKind, Relation};
use crate::results::ResultSet;
use crate::sql::StmtSpan;

pub(crate) const CONTEXT_MAX: usize = 28_000;
const TAB_SQL_MAX: usize = 8_000;
const TAB_SQL_WINDOW: usize = 2_000;
const STATEMENT_MAX: usize = 4_000;
const ERROR_MAX: usize = 1_000;
const ATTACHMENT_MAX: usize = 8_000;
const OUTLINE_MAX: usize = 12_000;
pub(crate) const ATTACHMENT_ROWS: u32 = 100;
pub(crate) const CELL_CHARS: usize = 200;

pub(crate) struct ConnectionFacts<'a> {
    pub database_kind: DatabaseKind,
    pub name: &'a str,
    pub server_version: Option<&'a str>,
    pub connected: Result<(), &'a str>,
}

pub(crate) struct TabFacts<'a> {
    pub title: &'a str,
    pub text: &'a str,
    pub picked: Option<StmtSpan>,
    pub selected: bool,
    pub last_error: Option<&'a str>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Repeat {
    Full,
    Unchanged,
}

pub(crate) struct ContextInput<'a> {
    pub access: AssistantAccess,
    pub connection: Option<ConnectionFacts<'a>>,
    pub db: Option<&'a DatabaseRef>,
    pub catalog: Option<&'a Catalog>,
    pub shown_schemas: &'a [String],
    pub tab: Option<TabFacts<'a>>,
    pub tab_sql: Repeat,
    pub attachment: Option<&'a Attachment>,
    pub outline: Repeat,
}

pub(crate) fn build(input: &ContextInput<'_>) -> String {
    let mut out = String::from("<l1_context>\n");
    match (&input.connection, input.db) {
        (Some(c), Some(db)) => {
            let dialect = match c.database_kind {
                DatabaseKind::Postgres => "PostgreSQL",
                DatabaseKind::SqlServer => "SQL Server / T-SQL",
            };
            let version = c.server_version.map_or_else(|| format!(" ({dialect})"), |v| format!(" ({dialect} {v})"));
            match c.connected {
                Ok(()) => {
                    out.push_str(&format!("Connection: {}{version}, database {}, connected.\n", c.name, db.database))
                }
                Err(reason) => {
                    let reason = if input.access == AssistantAccess::Full { reason } else { "connection unavailable" };
                    out.push_str(&format!(
                        "Connection: {}{version}, database {}. Not connected: {reason}\n",
                        c.name, db.database
                    ));
                }
            }
        }
        _ => out.push_str("No database is selected: the active tab has no connection, so the tools cannot run.\n"),
    }
    if let Some(catalog) = input.catalog {
        out.push_str(&format!(
            "Default schema: {}. Shown schemas: {}.\n",
            catalog.default_schema.as_deref().unwrap_or("none"),
            if input.shown_schemas.is_empty() { "none".to_owned() } else { input.shown_schemas.join(", ") }
        ));
    }
    if input.access == AssistantAccess::Full
        && let Some(tab) = &input.tab
    {
        out.push_str(&format!("Tab: {}\n", tab.title));
        match input.tab_sql {
            Repeat::Unchanged => out.push_str("Tab SQL: unchanged since your last message.\n"),
            Repeat::Full => {
                let (text, windowed) = tab_sql(tab.text, tab.picked);
                out.push_str(if windowed { "Tab SQL (the part around the cursor):\n" } else { "Tab SQL:\n" });
                out.push_str(&text);
                out.push('\n');
            }
        }
        if let Some(picked) = tab.picked {
            out.push_str(if tab.selected { "Selected statement:\n" } else { "Statement at the cursor:\n" });
            out.push_str(&cut(picked.text(tab.text), STATEMENT_MAX));
            out.push('\n');
        }
        if let Some(error) = tab.last_error {
            out.push_str("Last run failed: ");
            out.push_str(&cut(error, ERROR_MAX));
            out.push('\n');
        }
    }
    if input.access == AssistantAccess::Full
        && let Some(a) = input.attachment
    {
        out.push_str(&attachment_text(a));
    }
    if let Some(catalog) = input.catalog {
        match input.outline {
            Repeat::Unchanged => out.push_str("Schema outline: unchanged since earlier in this chat.\n"),
            Repeat::Full => {
                out.push_str("Schema outline:\n");
                out.push_str(&outline(catalog, input.shown_schemas, OUTLINE_MAX, input.access));
            }
        }
    }
    let close = "</l1_context>";
    if out.chars().count() + close.len() > CONTEXT_MAX {
        out = cut(&out, CONTEXT_MAX - close.len() - '\n'.len_utf8());
        out.push('\n');
    }
    out.push_str(close);
    out
}

fn tab_sql(text: &str, picked: Option<StmtSpan>) -> (String, bool) {
    if text.chars().count() <= TAB_SQL_MAX {
        return (text.to_owned(), false);
    }
    let (from, to) = picked.map_or((0, 0), |p| (p.start, p.end));
    let start = text[..from].char_indices().rev().nth(TAB_SQL_WINDOW - 1).map_or(0, |(i, _)| i);
    let end = text[to..].char_indices().nth(TAB_SQL_WINDOW).map_or(text.len(), |(i, _)| to + i);
    let mut out = String::new();
    if start > 0 {
        out.push_str("…\n");
    }
    out.push_str(&text[start..end]);
    if end < text.len() {
        out.push_str("\n…");
    }
    (out, true)
}

fn attachment_text(a: &Attachment) -> String {
    let mut out = format!(
        "Attached result from {} ({} of {} rows):\n{}\n",
        a.tab_title,
        a.rows.len(),
        a.total_rows,
        a.columns.join("\t")
    );
    for row in &a.rows {
        let line: Vec<&str> = row.iter().map(|c| c.as_deref().unwrap_or("NULL")).collect();
        let line = line.join("\t");
        if out.chars().count() + line.chars().count() > ATTACHMENT_MAX {
            out.push_str("…\n");
            break;
        }
        out.push_str(&line);
        out.push('\n');
    }
    out
}

pub(crate) fn attachment(set: &ResultSet, tab_title: &str) -> Attachment {
    let meta = set.meta();
    let page = set.page(0, ATTACHMENT_ROWS).expect("a result always has a first page").rows;
    let mut rows = Vec::new();
    let mut chars = 0;
    for row in page {
        let row: Vec<Option<String>> = row.into_iter().map(|c| c.map(|c| cut(&c, CELL_CHARS))).collect();
        chars += row.iter().map(|c| c.as_ref().map_or(4, |c| c.chars().count() + 1)).sum::<usize>();
        if chars > ATTACHMENT_MAX {
            break;
        }
        rows.push(row);
    }
    Attachment {
        tab_title: tab_title.to_owned(),
        columns: set.columns.iter().map(|c| c.name.clone()).collect(),
        rows,
        total_rows: meta.rows_seen,
    }
}

pub(crate) fn outline(catalog: &Catalog, shown: &[String], budget: usize, access: AssistantAccess) -> String {
    let schemas: Vec<_> = catalog.schemas.iter().filter(|s| shown.contains(&s.name)).collect();
    for detail in [Detail::Types, Detail::Names, Detail::Counts] {
        let lines: Vec<String> = schemas
            .iter()
            .flat_map(|s| s.relations.iter().map(move |r| outline_line(&s.name, r, detail, access)))
            .collect();
        let total: usize = lines.iter().map(|l| l.chars().count() + 1).sum();
        if total <= budget || detail == Detail::Counts {
            return fit(lines, budget);
        }
    }
    unreachable!("Counts always returns")
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Detail {
    Types,
    Names,
    Counts,
}

fn outline_line(schema: &str, r: &Relation, detail: Detail, access: AssistantAccess) -> String {
    let kind = match r.kind {
        RelKind::Table | RelKind::PartitionedTable => "",
        RelKind::View => " view",
        RelKind::MaterializedView => " materialized view",
        RelKind::ForeignTable => " foreign table",
    };
    let rows = if access == AssistantAccess::Full {
        r.row_estimate.map(|n| format!(", ~{n} rows")).unwrap_or_default()
    } else {
        String::new()
    };
    let columns = match detail {
        Detail::Types => r
            .columns
            .iter()
            .map(|c| {
                let mut s = format!("{} {}", c.name, c.type_display);
                if c.pk {
                    s.push_str(" PK");
                }
                if c.fk {
                    s.push_str(" FK");
                }
                s
            })
            .collect::<Vec<_>>()
            .join(", "),
        Detail::Names => r.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join(", "),
        Detail::Counts => format!("{} columns", r.columns.len()),
    };
    format!("{schema}.{}{kind}{rows}: {columns}", r.name)
}

fn fit(lines: Vec<String>, budget: usize) -> String {
    let mut out = String::new();
    for (kept, line) in lines.iter().enumerate() {
        let more = format!("…and {} more tables; use search_catalog to find them.\n", lines.len() - kept);
        if out.chars().count() + line.chars().count() + 1 + more.len() > budget && kept + 1 < lines.len() {
            out.push_str(&more);
            return out;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

pub(crate) fn cut(text: &str, max: usize) -> String {
    if text.chars().nth(max).is_none() {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ColumnInfo, ConnectionId, Relation, SchemaInfo, TypeCategory};

    fn column(name: &str, pk: bool, fk: bool) -> ColumnInfo {
        ColumnInfo {
            writable: true,
            attnum: 1,
            name: name.into(),
            quoted: name.into(),
            type_display: "bigint".into(),
            category: TypeCategory::Integer,
            not_null: true,
            pk,
            fk,
        }
    }

    fn catalog(tables: usize) -> Catalog {
        let relations = (0..tables)
            .map(|i| Relation {
                qualified_name: String::new(),
                oid: i as u32,
                name: format!("table_{i}"),
                quoted: format!("table_{i}"),
                kind: RelKind::Table,
                row_estimate: Some(10 * i as i64),
                columns: vec![
                    column("id", true, false),
                    column("customer_id", false, true),
                    column("total", false, false),
                ],
            })
            .collect();
        Catalog {
            database_kind: crate::model::DatabaseKind::Postgres,
            db: DatabaseRef { connection_id: ConnectionId(uuid::Uuid::nil()), database: "shop".into() },
            version: 1,
            default_schema: Some("public".into()),
            schemas: vec![
                SchemaInfo {
                    name: "public".into(),
                    quoted: "public".into(),
                    system: false,
                    table_count: tables as u32,
                    relations,
                    functions: Vec::new(),
                    sequences: Vec::new(),
                    types: Vec::new(),
                    synonyms: Vec::new(),
                },
                SchemaInfo {
                    name: "hidden".into(),
                    quoted: "hidden".into(),
                    system: false,
                    table_count: 1,
                    relations: vec![Relation {
                        qualified_name: String::new(),
                        oid: 9,
                        name: "secret".into(),
                        quoted: "secret".into(),
                        kind: RelKind::View,
                        row_estimate: None,
                        columns: vec![],
                    }],
                    functions: Vec::new(),
                    sequences: Vec::new(),
                    types: Vec::new(),
                    synonyms: Vec::new(),
                },
            ],
            extensions: Vec::new(),
        }
    }

    static PUBLIC: std::sync::LazyLock<Vec<String>> = std::sync::LazyLock::new(|| vec!["public".to_owned()]);

    fn input<'a>(
        catalog: &'a Catalog,
        db: &'a DatabaseRef,
        tab: Option<TabFacts<'a>>,
        attachment: Option<&'a Attachment>,
    ) -> ContextInput<'a> {
        ContextInput {
            access: AssistantAccess::Full,
            connection: Some(ConnectionFacts {
                database_kind: DatabaseKind::Postgres,
                name: "dev",
                server_version: Some("16.4"),
                connected: Ok(()),
            }),
            db: Some(db),
            catalog: Some(catalog),
            shown_schemas: &PUBLIC,
            tab,
            tab_sql: Repeat::Full,
            attachment,
            outline: Repeat::Full,
        }
    }

    #[test]
    fn schema_only_excludes_automatic_data_even_when_the_input_contains_it() {
        let c = catalog(2);
        let db = c.db.clone();
        let a = Attachment {
            tab_title: "ATTACHMENT_TITLE_CANARY".into(),
            columns: vec!["value".into()],
            rows: vec![vec![Some("ROW_CANARY".into())]],
            total_rows: 918273,
        };
        let tab = TabFacts {
            title: "TITLE_CANARY",
            text: "select 'EDITOR_CANARY'",
            picked: Some(StmtSpan { start: 0, end: 22 }),
            selected: true,
            last_error: Some("ERROR_CANARY"),
        };
        let mut i = input(&c, &db, Some(tab), Some(&a));
        i.access = AssistantAccess::SchemaOnly;
        i.connection.as_mut().unwrap().connected = Err("CONNECT_CANARY");
        let built = build(&i);
        for secret in ["CANARY", "~0 rows", "~10 rows", "918273", "Selected statement", "Tab SQL"] {
            assert!(!built.contains(secret), "{built}");
        }
        for structural in ["PostgreSQL", "database shop", "public.table_0", "id bigint PK", "customer_id bigint FK"] {
            assert!(built.contains(structural), "{built}");
        }
    }

    #[test]
    fn the_block_names_the_connection_the_tab_the_statement_the_error_and_the_outline() {
        let c = catalog(2);
        let db = c.db.clone();
        let text = "select 1;\nselect 2 frm orders";
        let tab = TabFacts {
            title: "scratch.sql",
            text,
            picked: Some(StmtSpan { start: 10, end: text.len() }),
            selected: false,
            last_error: Some("syntax error at or near \"orders\" (line 2, column 10)"),
        };
        let built = build(&input(&c, &db, Some(tab), None));
        assert_eq!(
            built,
            "<l1_context>\nConnection: dev (PostgreSQL 16.4), database shop, connected.\nDefault schema: public. Shown schemas: public.\nTab: scratch.sql\nTab SQL:\nselect 1;\nselect 2 frm orders\nStatement at the cursor:\nselect 2 frm orders\nLast run failed: syntax error at or near \"orders\" (line 2, column 10)\nSchema outline:\npublic.table_0, ~0 rows: id bigint PK, customer_id bigint FK, total bigint\npublic.table_1, ~10 rows: id bigint PK, customer_id bigint FK, total bigint\n</l1_context>"
        );
        assert!(!built.contains("secret"), "a schema the tree hides stays out");
    }

    #[test]
    fn repeats_collapse_to_one_line_and_a_missing_database_says_so() {
        let c = catalog(1);
        let db = c.db.clone();
        let tab = TabFacts { title: "t.sql", text: "select 1", picked: None, selected: false, last_error: None };
        let mut i = input(&c, &db, Some(tab), None);
        i.tab_sql = Repeat::Unchanged;
        i.outline = Repeat::Unchanged;
        let built = build(&i);
        assert!(
            built.contains("Tab SQL: unchanged since your last message.\n")
                && built.contains("Schema outline: unchanged since earlier in this chat.\n"),
            "{built}"
        );
        let none = build(&ContextInput {
            access: AssistantAccess::Full,
            connection: None,
            db: None,
            catalog: None,
            shown_schemas: &[],
            tab: None,
            tab_sql: Repeat::Full,
            attachment: None,
            outline: Repeat::Full,
        });
        assert_eq!(
            none,
            "<l1_context>\nNo database is selected: the active tab has no connection, so the tools cannot run.\n</l1_context>"
        );
        let mut disconnected = input(&c, &db, None, None);
        disconnected.connection = Some(ConnectionFacts {
            database_kind: DatabaseKind::Postgres,
            name: "dev",
            server_version: None,
            connected: Err("password missing"),
        });
        assert!(
            build(&disconnected)
                .contains("Connection: dev (PostgreSQL), database shop. Not connected: password missing\n")
        );
    }

    #[test]
    fn a_long_document_sends_the_window_around_the_cursor_statement() {
        let filler = "select 'x';\n".repeat(1_000);
        let text = format!("{filler}select 'here';\n{filler}");
        let at = filler.len();
        let c = catalog(1);
        let db = c.db.clone();
        let tab = TabFacts {
            title: "long.sql",
            text: &text,
            picked: Some(StmtSpan { start: at, end: at + 14 }),
            selected: true,
            last_error: None,
        };
        let built = build(&input(&c, &db, Some(tab), None));
        assert!(built.contains("Tab SQL (the part around the cursor):\n…\n"), "{}", &built[..300]);
        assert!(built.contains("select 'here';") && built.contains("\n…\nSelected statement:\nselect 'here';\n"));
        let sql_part = built
            .split("Tab SQL (the part around the cursor):\n")
            .nth(1)
            .unwrap()
            .split("\nSelected statement")
            .next()
            .unwrap();
        assert!(sql_part.chars().count() <= 2 * TAB_SQL_WINDOW + 14 + 6, "{}", sql_part.chars().count());
    }

    #[test]
    fn the_cursor_statement_has_its_own_budget() {
        let text = format!("select '{}'", "y".repeat(5_000));
        let c = catalog(1);
        let db = c.db.clone();
        let tab = TabFacts {
            title: "t.sql",
            text: &text,
            picked: Some(StmtSpan { start: 0, end: text.len() }),
            selected: false,
            last_error: None,
        };
        let built = build(&input(&c, &db, Some(tab), None));
        let statement = built.split("Statement at the cursor:\n").nth(1).unwrap().lines().next().unwrap();
        assert_eq!(statement.chars().count(), STATEMENT_MAX);
        assert!(statement.ends_with('…'));
        assert!(built.contains(&format!("Tab SQL:\n{text}\n")), "the tab SQL keeps its own budget");
    }

    #[test]
    fn a_block_over_budget_is_cut_to_exactly_the_budget() {
        let text = format!("select '{}'", "z".repeat(CONTEXT_MAX));
        let c = catalog(1);
        let db = c.db.clone();
        let tab = TabFacts {
            title: "t.sql",
            text: &text,
            picked: Some(StmtSpan { start: 0, end: text.len() }),
            selected: false,
            last_error: None,
        };
        let built = build(&input(&c, &db, Some(tab), None));
        assert_eq!(built.chars().count(), CONTEXT_MAX);
        assert!(built.ends_with("z…\n</l1_context>"), "{}", &built[built.len() - 40..]);
    }

    #[test]
    fn the_outline_degrades_by_types_then_columns_then_a_more_line_and_the_block_stays_under_budget() {
        let c = catalog(3_000);
        let shown = ["public".to_owned()];
        let full = outline(&c, &shown, usize::MAX, AssistantAccess::Full);
        assert!(full.starts_with("public.table_0, ~0 rows: id bigint PK, customer_id bigint FK, total bigint\n"));
        let names = outline(&c, &shown, 180_000, AssistantAccess::Full);
        assert!(names.starts_with("public.table_0, ~0 rows: id, customer_id, total\n"), "{}", &names[..100]);
        let counts = outline(&c, &shown, OUTLINE_MAX, AssistantAccess::Full);
        assert!(counts.starts_with("public.table_0, ~0 rows: 3 columns\n"), "{}", &counts[..100]);
        assert!(counts.chars().count() <= OUTLINE_MAX);
        let last = counts.lines().last().unwrap();
        assert!(
            last.starts_with("…and ") && last.ends_with(" more tables; use search_catalog to find them."),
            "{last}"
        );
        let db = c.db.clone();
        let built = build(&input(&c, &db, None, None));
        assert!(built.chars().count() <= CONTEXT_MAX && built.ends_with("</l1_context>"));
    }

    #[test]
    fn cut_keeps_at_most_max_chars_with_the_ellipsis_counted() {
        assert_eq!(cut("abcdef", 4), "abc…");
        assert_eq!(cut("abcd", 4), "abcd");
        assert_eq!(cut("abcdef", 4).chars().count(), 4);
        assert_eq!(cut("", 4), "");
    }

    #[test]
    fn an_attachment_lists_rows_as_tsv_with_null_and_a_cut_marker() {
        let a = Attachment {
            tab_title: "orders.sql".into(),
            columns: vec!["id".into(), "region".into()],
            rows: vec![vec![Some("1".into()), None], vec![Some("2".into()), Some("EU".into())]],
            total_rows: 50,
        };
        assert_eq!(
            attachment_text(&a),
            "Attached result from orders.sql (2 of 50 rows):\nid\tregion\n1\tNULL\n2\tEU\n"
        );
        let wide = Attachment {
            tab_title: "w".into(),
            columns: vec!["c".into()],
            rows: (0..100).map(|_| vec![Some("y".repeat(199))]).collect(),
            total_rows: 100,
        };
        let text = attachment_text(&wide);
        assert!(text.chars().count() <= ATTACHMENT_MAX + 2 && text.ends_with("…\n"));
    }
}
