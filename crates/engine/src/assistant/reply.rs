use std::collections::HashSet;

use super::context::cut;
use crate::model::{Catalog, Entry, ReplyBlock, ToolResult, ToolStatus, ToolUse, TurnOutcome};
use crate::sql;

const SQL_INFOS: [&str; 6] = ["sql", "postgresql", "pgsql", "tsql", "mssql", "sqlserver"];
const FOLLOWUPS_INFO: &str = "followups";
pub(crate) const MAX_FOLLOWUPS: usize = 3;
pub(crate) const MAX_FOLLOWUP_CHARS: usize = 80;

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Parsed {
    pub blocks: Vec<ReplyBlock>,
    pub followups: Vec<String>,
}

pub(crate) fn parse_text(text: &str, done: bool) -> Parsed {
    let mut parsed = Parsed::default();
    let mut markdown = String::new();
    let mut lines = text.split_inclusive('\n').peekable();
    let flush = |markdown: &mut String, blocks: &mut Vec<ReplyBlock>| {
        let trimmed = markdown.trim_matches('\n');
        if !trimmed.trim().is_empty() {
            blocks.push(ReplyBlock::Markdown { text: trimmed.to_owned() });
        }
        markdown.clear();
    };
    while let Some(line) = lines.next() {
        let bare = line.trim_end_matches(['\n', '\r']);
        let last_and_unterminated = lines.peek().is_none() && !line.ends_with('\n');
        let Some((ticks, info)) = fence_open(bare) else {
            if !(last_and_unterminated && !done && bare.trim_start().starts_with("```")) {
                markdown.push_str(line);
            }
            continue;
        };
        if last_and_unterminated && !done {
            continue;
        }
        let mut body = String::new();
        let mut closed = false;
        for inner in lines.by_ref() {
            if fence_close(inner.trim_end_matches(['\n', '\r']), ticks) {
                closed = true;
                break;
            }
            body.push_str(inner);
        }
        let info = info.to_ascii_lowercase();
        if SQL_INFOS.contains(&info.as_str()) {
            flush(&mut markdown, &mut parsed.blocks);
            parsed.blocks.push(ReplyBlock::Sql {
                sql: body.trim_matches(['\n', '\r']).to_owned(),
                relations: Vec::new(),
                vetted: false,
                complete: closed || done,
            });
        } else if info == FOLLOWUPS_INFO {
            if closed || done {
                parsed.followups = body
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .take(MAX_FOLLOWUPS)
                    .map(cut_followup)
                    .collect();
            }
        } else {
            markdown.push_str(line);
            markdown.push_str(&body);
            if closed {
                markdown.push_str(&"`".repeat(ticks));
                markdown.push('\n');
            }
        }
    }
    flush(&mut markdown, &mut parsed.blocks);
    parsed
}

fn cut_followup(line: &str) -> String {
    cut(line.trim_start_matches(['-', '*', ' ']).trim_matches('"').trim(), MAX_FOLLOWUP_CHARS)
}

fn fence_open(line: &str) -> Option<(usize, &str)> {
    let rest = line.trim_start_matches(' ');
    if line.len() - rest.len() > 3 {
        return None;
    }
    let ticks = rest.chars().take_while(|c| *c == '`').count();
    let info = &rest[ticks..];
    (ticks >= 3 && !info.contains('`')).then(|| (ticks, info.split_whitespace().next().unwrap_or("")))
}

fn fence_close(line: &str, ticks: usize) -> bool {
    let rest = line.trim_start_matches(' ');
    let body = rest.trim_end();
    line.len() - rest.len() <= 3 && !body.is_empty() && body.chars().all(|c| c == '`') && body.len() >= ticks
}

pub(crate) fn uses(sql: &str, catalog: Option<&Catalog>, kind: crate::model::DatabaseKind) -> Vec<String> {
    let names = match kind {
        crate::model::DatabaseKind::Postgres => sql::relation_names(sql),
        crate::model::DatabaseKind::SqlServer => sql::tsql::relation_names(sql),
    };
    let Some(catalog) = catalog else {
        return names.into_iter().map(|n| display(n.schema.as_deref(), &n.name, None)).collect();
    };
    let default = catalog.default_schema.as_deref();
    let holds = |schema: &str, name: &str| {
        catalog.schemas.iter().any(|s| s.name == schema && s.relations.iter().any(|r| r.name == name))
    };
    names
        .into_iter()
        .filter_map(|n| {
            let schema = match n.schema {
                Some(schema) => holds(&schema, &n.name).then_some(schema)?,
                None => default.filter(|d| holds(d, &n.name)).map(str::to_owned).or_else(|| {
                    catalog
                        .schemas
                        .iter()
                        .filter(|s| !s.system)
                        .chain(catalog.schemas.iter().filter(|s| s.system))
                        .find(|s| s.relations.iter().any(|r| r.name == n.name))
                        .map(|s| s.name.clone())
                })?,
            };
            Some(display(Some(&schema), &n.name, default))
        })
        .collect()
}

fn display(schema: Option<&str>, name: &str, default: Option<&str>) -> String {
    match schema {
        Some(schema) if Some(schema) != default => format!("{schema}.{name}"),
        _ => name.to_owned(),
    }
}

pub(crate) fn vetted(sql: &str) -> bool {
    sql::split_statements(sql).iter().all(|span| sql::vet(span.text(sql)) == sql::Vet::Clear)
}

pub(crate) fn recover(entries: &[(u32, u32, Entry)], at_ms: i64) -> Vec<(u32, u32, Entry)> {
    let ended: HashSet<u32> =
        entries.iter().filter(|(_, _, e)| matches!(e, Entry::End { .. })).map(|(_, turn, _)| *turn).collect();
    let mut fixes = Vec::new();
    let mut open_turns = Vec::new();
    for (seq, turn, entry) in entries {
        if ended.contains(turn) {
            continue;
        }
        match entry {
            Entry::User { .. } => open_turns.push(*turn),
            Entry::Text { blocks, done: false } => {
                fixes.push((*seq, *turn, Entry::Text { blocks: blocks.clone(), done: true }))
            }
            Entry::Tool { call, tool, status: ToolStatus::Running | ToolStatus::AwaitingApproval { .. } } => {
                fixes.push((
                    *seq,
                    *turn,
                    Entry::Tool { call: call.clone(), tool: tool.clone(), status: ToolStatus::Expired },
                ));
            }
            _ => {}
        }
    }
    let mut next_seq = entries.iter().map(|(seq, _, _)| seq + 1).max().unwrap_or(0);
    for turn in open_turns {
        fixes.push((next_seq, turn, Entry::End { at_ms, outcome: TurnOutcome::Lost, suggestions: Vec::new() }));
        next_seq += 1;
    }
    fixes
}

pub(crate) fn recap(entries: &[(u32, u32, Entry)], max_chars: usize) -> String {
    let mut turns: Vec<(u32, String)> = Vec::new();
    for (_, turn, entry) in entries {
        let line = match entry {
            Entry::User { text, .. } => format!("User: {text}\n"),
            Entry::Text { blocks, .. } => {
                let text: Vec<String> = blocks
                    .iter()
                    .map(|b| match b {
                        ReplyBlock::Markdown { text } => text.clone(),
                        ReplyBlock::Sql { sql, .. } => format!("```sql\n{sql}\n```"),
                    })
                    .collect();
                format!("Assistant: {}\n", text.join("\n\n"))
            }
            Entry::Tool { tool, status, .. } => format!("[{}: {}]\n", tool_line(tool), status_line(status)),
            Entry::Notice { .. } | Entry::End { .. } | Entry::Unreadable => continue,
        };
        match turns.last_mut() {
            Some((t, text)) if *t == *turn => text.push_str(&line),
            _ => turns.push((*turn, line)),
        }
    }
    let mut kept: Vec<&str> = Vec::new();
    let mut total = 0;
    for (_, text) in turns.iter().rev() {
        let len = text.chars().count();
        if total + len > max_chars {
            break;
        }
        total += len;
        kept.push(text);
    }
    kept.reverse();
    let mut out = String::from("Earlier in this conversation, restored after the session was lost:\n\n");
    out.push_str(&kept.join("\n"));
    out
}

fn tool_line(tool: &ToolUse) -> String {
    match tool {
        ToolUse::DescribeTables { tables } => format!("described {}", tables.join(", ")),
        ToolUse::SearchCatalog { query } => format!("searched the catalog for {query:?}"),
        ToolUse::RunQuery { sql, .. } => format!("ran {sql}"),
        ToolUse::Other { name } => format!("called {name}"),
    }
}

fn status_line(status: &ToolStatus) -> String {
    match status {
        ToolStatus::Running | ToolStatus::AwaitingApproval { .. } | ToolStatus::Expired => "no result".into(),
        ToolStatus::Done { result: ToolResult::Rows { rows, more, .. } } => {
            format!("{}{} rows", rows.len(), if *more { "+" } else { "" })
        }
        ToolStatus::Done { result: ToolResult::Described { found, .. } } => format!("described {}", found.join(", ")),
        ToolStatus::Done { result: ToolResult::Found { matches } } => format!("{matches} matches"),
        ToolStatus::Failed { message } => format!("failed: {message}"),
        ToolStatus::Denied { .. } => "denied".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn uses(sql: &str, catalog: Option<&Catalog>) -> Vec<String> {
        super::uses(sql, catalog, crate::model::DatabaseKind::Postgres)
    }
    use crate::model::{
        ColumnInfo, ConnectionId, DatabaseRef, RelKind, Relation, SchemaInfo, ToolCallId, TypeCategory,
    };

    const REPLY: &str = "This compares paid revenue:\n\n```sql\nSELECT c.region\nFROM orders o\n```\n\n`FILTER` keeps both periods in one scan.\n\n```python\nprint(1)\n```\n\n```followups\nShow growth as a percentage\nOnly EU regions\n```\n";

    fn markdown(text: &str) -> ReplyBlock {
        ReplyBlock::Markdown { text: text.into() }
    }

    fn sql(text: &str, complete: bool) -> ReplyBlock {
        ReplyBlock::Sql { sql: text.into(), relations: Vec::new(), vetted: false, complete }
    }

    #[test]
    fn a_finished_reply_splits_into_markdown_sql_and_hidden_followups() {
        let parsed = parse_text(REPLY, true);
        assert_eq!(
            parsed.blocks,
            vec![
                markdown("This compares paid revenue:"),
                sql("SELECT c.region\nFROM orders o", true),
                markdown("`FILTER` keeps both periods in one scan.\n\n```python\nprint(1)\n```")
            ]
        );
        assert_eq!(parsed.followups, ["Show growth as a percentage", "Only EU regions"]);
    }

    #[test]
    fn an_open_fence_streams_as_an_incomplete_block_and_a_partial_fence_line_stays_hidden() {
        assert_eq!(
            parse_text("Here:\n```sql\nSELECT 1\nFROM", false).blocks,
            vec![markdown("Here:"), sql("SELECT 1\nFROM", false)]
        );
        assert_eq!(
            parse_text("Here:\n```sql\nSELECT 1\nFROM", true).blocks,
            vec![markdown("Here:"), sql("SELECT 1\nFROM", true)],
            "a done block closes its fence"
        );
        assert_eq!(parse_text("Done.\n``", false).blocks, vec![markdown("Done.\n``")]);
        assert_eq!(
            parse_text("Done.\n```fol", false).blocks,
            vec![markdown("Done.")],
            "a trailing partial fence line is hidden"
        );
        let open_followups = parse_text("Done.\n```followups\nOnly EU", false);
        assert_eq!(
            (open_followups.blocks, open_followups.followups),
            (vec![markdown("Done.")], vec![]),
            "an open followups fence shows nothing and yields no chips yet"
        );
    }

    #[test]
    fn followups_are_capped_and_cleaned() {
        let long = "x".repeat(200);
        let parsed = parse_text(&format!("```followups\n- a\n* \"b\"\n  c  \nd\n{long}\n```"), true);
        assert_eq!(parsed.followups, ["a", "b", "c"]);
        let parsed = parse_text(&format!("```followups\n{long}\n```"), true);
        assert_eq!(parsed.followups[0].chars().count(), 80);
        assert!(parsed.followups[0].ends_with('…'));
    }

    #[test]
    fn only_column_zero_fences_with_a_sql_info_become_blocks() {
        let parsed = parse_text(
            "- item\n    ```sql\n    not a fence\n    ```\n\n~~~sql\nnope\n~~~\n\n```\nplain\n```\n\n``` PostgreSQL\nselect 2\n```",
            true,
        );
        assert_eq!(parsed.blocks.len(), 2);
        assert_eq!(parsed.blocks[1], sql("select 2", true));
        assert_eq!(parse_text("````sql\nselect '```'\n````", true).blocks, vec![sql("select '```'", true)]);
    }

    #[test]
    fn every_prefix_keeps_the_blocks_a_longer_text_keeps() {
        let full = parse_text(REPLY, true);
        for end in (0..=REPLY.len()).filter(|e| REPLY.is_char_boundary(*e)) {
            let prefix = parse_text(&REPLY[..end], false);
            for (i, block) in prefix.blocks.iter().enumerate().take(prefix.blocks.len().saturating_sub(1)) {
                assert_eq!(block, &full.blocks[i], "prefix of {end} bytes moved block {i}");
            }
        }
    }

    fn catalog() -> Catalog {
        let relation = |name: &str| Relation {
            qualified_name: String::new(),
            oid: 1,
            name: name.into(),
            quoted: name.into(),
            kind: RelKind::Table,
            row_estimate: Some(3),
            columns: vec![ColumnInfo {
                writable: true,
                attnum: 1,
                name: "id".into(),
                quoted: "id".into(),
                type_display: "bigint".into(),
                category: TypeCategory::Integer,
                not_null: true,
                pk: true,
                fk: false,
            }],
        };
        let schema = |name: &str, system: bool, relations: Vec<Relation>| SchemaInfo {
            name: name.into(),
            quoted: name.into(),
            system,
            table_count: relations.len() as u32,
            relations,
            functions: Vec::new(),
            sequences: Vec::new(),
            types: Vec::new(),
            synonyms: Vec::new(),
        };
        Catalog {
            database_kind: crate::model::DatabaseKind::Postgres,
            db: DatabaseRef { connection_id: ConnectionId(uuid::Uuid::nil()), database: "shop".into() },
            version: 1,
            default_schema: Some("public".into()),
            schemas: vec![
                schema("audit", false, vec![relation("events")]),
                schema("lab", false, vec![relation("type_zoo"), relation("orders")]),
                schema("pg_catalog", true, vec![relation("pg_class")]),
                schema("public", false, vec![relation("orders"), relation("customers")]),
            ],
            extensions: Vec::new(),
        }
    }

    #[test]
    fn uses_resolves_against_the_catalog_and_prints_other_schemas_qualified() {
        let c = catalog();
        let mockup = "select c.region, count(*) from orders o join customers c on c.id = o.customer_id";
        assert_eq!(uses(mockup, Some(&c)), ["orders", "customers"]);
        assert_eq!(
            uses(
                "select * from public.orders, lab.orders, lab.type_zoo, type_zoo, events, pg_class, nope, lab.nope",
                Some(&c)
            ),
            ["orders", "lab.orders", "lab.type_zoo", "lab.type_zoo", "audit.events", "pg_catalog.pg_class"]
        );
        assert_eq!(uses("with recent as (select * from orders) select * from recent", Some(&c)), ["orders"]);
        assert_eq!(uses(mockup, None), ["orders", "customers"], "without a catalog the names print as written");
        assert_eq!(uses("select * from shop.\"Odd\" o", None), ["shop.Odd"]);
    }

    #[test]
    fn vetted_is_false_for_any_statement_that_may_not_run_unattended() {
        assert!(vetted("select 1;\nselect 2"));
        assert!(!vetted("select 1; delete from orders"));
        assert!(!vetted("with x as (delete from t returning *) select * from x"));
        assert!(
            !vetted("with x as (delete from t returning *)\n\nselect * from x"),
            "a blank line inside a CTE does not split it into a clear read"
        );
        assert!(!vetted("select pg_terminate_backend(1)"), "a read calling an unvetted function gets no one-click Run");
        assert!(!vetted("select * into backup from orders"));
        assert!(!vetted(r#"select U&"pg\005fcancel_backend"(0)"#));
        assert!(vetted(""));
    }

    fn text(done: bool) -> Entry {
        Entry::Text { blocks: vec![markdown("hi")], done }
    }

    fn tool(status: ToolStatus) -> Entry {
        Entry::Tool { call: ToolCallId("t1".into()), tool: ToolUse::SearchCatalog { query: "x".into() }, status }
    }

    fn user(text: &str) -> Entry {
        Entry::User { at_ms: 1, text: text.into(), context: None, attachment: None }
    }

    #[test]
    fn recover_closes_open_rows_once_and_leaves_closed_turns_alone() {
        let end = Entry::End { at_ms: 2, outcome: TurnOutcome::Completed, suggestions: Vec::new() };
        let entries = vec![
            (0, 0, user("one")),
            (1, 0, text(true)),
            (2, 0, end.clone()),
            (3, 3, user("two")),
            (4, 3, text(false)),
            (
                5,
                3,
                tool(ToolStatus::AwaitingApproval {
                    approval: crate::model::ApprovalId(1),
                    reason: crate::model::ApprovalReason::AskFirst,
                }),
            ),
            (6, 3, tool(ToolStatus::Done { result: ToolResult::Found { matches: 2 } })),
        ];
        let fixes = recover(&entries, 9);
        assert_eq!(
            fixes,
            vec![
                (4, 3, text(true)),
                (5, 3, tool(ToolStatus::Expired)),
                (7, 3, Entry::End { at_ms: 9, outcome: TurnOutcome::Lost, suggestions: Vec::new() })
            ]
        );
        let mut fixed = entries.clone();
        for fix in fixes {
            match fixed.iter_mut().find(|(seq, _, _)| *seq == fix.0) {
                Some(row) => *row = fix,
                None => fixed.push(fix),
            }
        }
        assert_eq!(recover(&fixed, 10), vec![], "a second run finds nothing open");
        assert_eq!(recover(&[(0, 0, user("one")), (1, 0, text(true)), (2, 0, end)], 10), vec![]);
        assert_eq!(
            recover(&[(0, 0, user("alone"))], 10),
            vec![(1, 0, Entry::End { at_ms: 10, outcome: TurnOutcome::Lost, suggestions: Vec::new() })]
        );
    }

    #[test]
    fn recap_keeps_the_newest_turns_that_fit() {
        let entries = vec![
            (0, 0, user("first question")),
            (1, 0, Entry::Text { blocks: vec![markdown("first answer"), sql("select 1", true)], done: true }),
            (2, 0, Entry::End { at_ms: 2, outcome: TurnOutcome::Completed, suggestions: Vec::new() }),
            (3, 3, user("second question")),
            (
                4,
                3,
                tool(ToolStatus::Done {
                    result: ToolResult::Rows {
                        columns: vec!["n".into()],
                        rows: vec![vec![Some("7".into())]],
                        more: true,
                        elapsed_ms: 1,
                    },
                }),
            ),
            (5, 3, text(true)),
            (6, 3, Entry::Notice { text: "restarted".into() }),
            (7, 3, Entry::End { at_ms: 2, outcome: TurnOutcome::Completed, suggestions: Vec::new() }),
        ];
        let all = recap(&entries, 10_000);
        assert_eq!(
            all,
            "Earlier in this conversation, restored after the session was lost:\n\nUser: first question\nAssistant: first answer\n\n```sql\nselect 1\n```\n\nUser: second question\n[searched the catalog for \"x\": 1+ rows]\nAssistant: hi\n"
        );
        let newest = recap(&entries, 80);
        assert_eq!(
            newest,
            "Earlier in this conversation, restored after the session was lost:\n\nUser: second question\n[searched the catalog for \"x\": 1+ rows]\nAssistant: hi\n"
        );
    }
}
