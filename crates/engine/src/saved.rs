use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::Shared;
use crate::console::now_ms;
use crate::model::*;
use crate::sql::{ByteTarget, history_text};
use crate::store::Store;
use crate::text::{self, OffsetMap};

pub const SQL_LIMIT_BYTES: usize = 64 * 1024;
const NAME_LIMIT_CHARS: usize = 200;

impl From<rusqlite::Error> for SavedQueryError {
    fn from(e: rusqlite::Error) -> Self {
        SavedQueryError::Store { message: e.to_string() }
    }
}

pub(crate) fn text_to_save(text: &str, target: RunTarget, kind: DatabaseKind) -> Result<String, SavedQueryError> {
    let picked = match OffsetMap::new(text).target(target) {
        ByteTarget::All => text.trim(),
        ByteTarget::Selection { from, to } if from < to => text[from..to].trim(),
        cursor => {
            let spans = text::pick(text, cursor, kind).map_err(|_| SavedQueryError::NothingToSave)?;
            match (spans.first(), spans.last()) {
                (Some(first), Some(last)) => &text[first.start..through_semicolon(text, last.end)],
                _ => "",
            }
        }
    };
    checked(picked).map(str::to_owned)
}

fn through_semicolon(text: &str, end: usize) -> usize {
    let rest = &text[end..];
    let gap = rest.len() - rest.trim_start().len();
    if rest[gap..].starts_with(';') { end + gap + 1 } else { end }
}

fn checked(sql: &str) -> Result<&str, SavedQueryError> {
    if sql.is_empty() {
        return Err(SavedQueryError::NothingToSave);
    }
    if sql.len() > SQL_LIMIT_BYTES {
        return Err(SavedQueryError::TooLong { limit_bytes: SQL_LIMIT_BYTES as u32 });
    }
    Ok(sql)
}

pub(crate) fn name_for(name: &str, sql: &str) -> String {
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    let name = if name.is_empty() { history_text(sql).0 } else { name };
    name.chars().take(NAME_LIMIT_CHARS).collect()
}

fn kind_column(kind: DatabaseKind) -> &'static str {
    match kind {
        DatabaseKind::Postgres => "Postgres",
        DatabaseKind::SqlServer => "SqlServer",
    }
}

fn kind_of(column: &str) -> Option<DatabaseKind> {
    match column {
        "Postgres" => Some(DatabaseKind::Postgres),
        "SqlServer" => Some(DatabaseKind::SqlServer),
        _ => None,
    }
}

fn readable(r: &Row<'_>) -> Option<SavedQuery> {
    let read = || -> rusqlite::Result<SavedQuery> {
        let kind: String = r.get(3)?;
        let database_kind = kind_of(&kind).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                3,
                rusqlite::types::Type::Text,
                format!("unknown database kind {kind:?}").into(),
            )
        })?;
        let sql: String = r.get(2)?;
        Ok(SavedQuery {
            id: SavedQueryId(r.get(0)?),
            name: r.get(1)?,
            excerpt: history_text(&sql).1,
            sql,
            database_kind,
            created_at_ms: r.get(4)?,
            used_at_ms: r.get(5)?,
        })
    };
    read()
        .map_err(|e| {
            eprintln!("saved query {} unreadable, left out of the list: {e}", r.get::<_, i64>(0).unwrap_or_default())
        })
        .ok()
}

fn list(conn: &Connection, rev: u64) -> rusqlite::Result<SavedQueries> {
    let mut stmt = conn.prepare(
        "SELECT id, name, sql, database_kind, created_ms, used_ms FROM saved_queries ORDER BY used_ms DESC, id DESC",
    )?;
    let rows = stmt.query_map([], |r| Ok(readable(r)))?;
    let mut items = Vec::new();
    for item in rows {
        items.extend(item?);
    }
    Ok(SavedQueries { rev, items })
}

pub(crate) fn snapshot(store: &Store) -> rusqlite::Result<SavedQueries> {
    store.with_saved(|conn, revision| list(conn, *revision))
}

fn write_and_announce<T>(
    shared: &Shared,
    f: impl FnOnce(&Connection) -> Result<T, SavedQueryError>,
) -> Result<(T, SavedQueries), SavedQueryError> {
    let (out, queries, changed) = shared.store.with_saved(|conn, revision| {
        let before = conn.total_changes();
        let tx = conn.unchecked_transaction()?;
        let out = f(&tx)?;
        tx.commit()?;
        let changed = conn.total_changes() != before;
        if changed {
            *revision += 1;
        }
        Ok::<_, SavedQueryError>((out, list(conn, *revision)?, changed))
    })?;
    if changed {
        shared.emit(AppEvent::SavedQueriesChanged(queries.clone()));
    }
    Ok((out, queries))
}

pub(crate) fn save(
    shared: &Shared,
    text: &str,
    target: RunTarget,
    kind: DatabaseKind,
) -> Result<SaveQueryReply, SavedQueryError> {
    let sql = text_to_save(text, target, kind)?;
    let ((id, already_saved), queries) = write_and_announce(shared, |c| {
        let existing = c
            .query_row(
                "SELECT id FROM saved_queries WHERE database_kind = ?1 AND sql = ?2 ORDER BY id LIMIT 1",
                params![kind_column(kind), sql],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = existing {
            return Ok((SavedQueryId(id), true));
        }
        c.execute(
            "INSERT INTO saved_queries (name, sql, database_kind, created_ms, used_ms) VALUES (?1, ?2, ?3, ?4, ?4)",
            params![name_for("", &sql), sql, kind_column(kind), now_ms()],
        )?;
        Ok((SavedQueryId(c.last_insert_rowid()), false))
    })?;
    Ok(SaveQueryReply { id, already_saved, queries })
}

pub(crate) fn update(shared: &Shared, id: SavedQueryId, edit: SavedQueryEdit) -> Result<SavedQueries, SavedQueryError> {
    let sql = checked(edit.sql.trim())?;
    let ((), queries) = write_and_announce(shared, |c| {
        let updated = c.execute(
            "UPDATE saved_queries SET name = ?1, sql = ?2 WHERE id = ?3",
            params![name_for(&edit.name, sql), sql, id.0],
        )?;
        if updated == 0 { Err(SavedQueryError::NotFound) } else { Ok(()) }
    })?;
    Ok(queries)
}

pub(crate) fn delete(shared: &Shared, id: SavedQueryId) -> Result<SavedQueries, SavedQueryError> {
    let ((), queries) = write_and_announce(shared, |c| {
        c.execute("DELETE FROM saved_queries WHERE id = ?1", [id.0])?;
        Ok(())
    })?;
    Ok(queries)
}

pub(crate) fn used(shared: &Shared, id: SavedQueryId) -> Result<SavedQueries, SavedQueryError> {
    let ((), queries) = write_and_announce(shared, |c| {
        c.execute("UPDATE saved_queries SET used_ms = ?1 WHERE id = ?2", params![now_ms(), id.0])?;
        Ok(())
    })?;
    Ok(queries)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::{AgentPath, Engine, EngineConfig, MemoryKeyStore, ScriptedTransport};

    async fn engine() -> (Arc<Engine>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::open(EngineConfig {
            data_dir: dir.path().to_owned(),
            keys: Arc::new(MemoryKeyStore::default()),
            agent_path: AgentPath::Fixed("".into()),
            az_binary: None,
            user_known_hosts: None,
            updates: Arc::new(ScriptedTransport::named("off").unwrap()),
        })
        .await
        .unwrap();
        (engine, dir)
    }

    const PG: DatabaseKind = DatabaseKind::Postgres;

    fn cursor(offset: u32) -> RunTarget {
        RunTarget::Cursor { offset: DocOffset(offset) }
    }

    fn save_all(engine: &Engine, sql: &str) -> SaveQueryReply {
        engine.saved_query_save(sql, RunTarget::All, PG).unwrap()
    }

    fn names(queries: &SavedQueries) -> Vec<&str> {
        queries.items.iter().map(|q| q.name.as_str()).collect()
    }

    fn announced(events: &mut tokio::sync::broadcast::Receiver<AppEvent>) -> Vec<u64> {
        std::iter::from_fn(|| events.try_recv().ok())
            .filter_map(|e| match e {
                AppEvent::SavedQueriesChanged(q) => Some(q.rev),
                _ => None,
            })
            .collect()
    }

    fn raw(engine: &Engine, sql: &str) {
        engine.shared.store.with_saved(|c, _| c.execute_batch(sql)).unwrap();
    }

    #[tokio::test]
    async fn saving_the_same_statement_twice_returns_the_first_entry() {
        let (engine, _dir) = engine().await;
        let text = "select 1;\n\nselect now();";
        let first = engine.saved_query_save(text, cursor(14), PG).unwrap();
        let again = engine.saved_query_save(text, cursor(14), PG).unwrap();
        assert!(!first.already_saved && again.already_saved);
        assert_eq!(first.id, again.id);
        assert_eq!(again.queries.items.len(), 1);
        assert_eq!(again.queries.items[0].sql, "select now();");
        assert_eq!(again.queries.rev, first.queries.rev, "a save that writes nothing keeps the revision");
        let other_dialect = engine.saved_query_save(text, cursor(14), DatabaseKind::SqlServer).unwrap();
        assert!(!other_dialect.already_saved && other_dialect.id != first.id);
    }

    #[test]
    fn a_selection_is_saved_as_selected_and_a_cursor_takes_the_statement_run_would() {
        let text = "select 1;\nselect 2 from t;\n";
        assert_eq!(
            text_to_save(text, RunTarget::Selection { from: DocOffset(10), to: DocOffset(18) }, PG).unwrap(),
            "select 2"
        );
        assert_eq!(
            text_to_save(text, RunTarget::Selection { from: DocOffset(9), to: DocOffset(27) }, PG).unwrap(),
            "select 2 from t;"
        );
        assert_eq!(text_to_save(text, cursor(12), PG).unwrap(), "select 2 from t;");
        assert_eq!(text_to_save("select 1 ;\nselect 2", cursor(2), PG).unwrap(), "select 1 ;");
        assert_eq!(text_to_save("  select 1;\n\nselect 2  \n", RunTarget::All, PG).unwrap(), "select 1;\n\nselect 2");
    }

    #[test]
    fn a_cursor_offset_counts_utf16_units_past_an_emoji() {
        let text = "select '😀';\nselect 'after';";
        let after_emoji = "select '😀';\n".encode_utf16().count() as u32;
        assert_eq!(text_to_save(text, cursor(after_emoji + 2), PG).unwrap(), "select 'after';");
        assert_eq!(text_to_save(text, cursor(9), PG).unwrap(), "select '😀';");
    }

    #[test]
    fn a_sql_server_cursor_takes_its_go_batch() {
        let text = "select 1\nselect 2\nGO\nselect 3\nGO\n";
        assert_eq!(text_to_save(text, cursor(2), DatabaseKind::SqlServer).unwrap(), "select 1\nselect 2");
        assert_eq!(text_to_save(text, cursor(23), DatabaseKind::SqlServer).unwrap(), "select 3");
    }

    #[test]
    fn blank_and_oversized_text_is_refused() {
        assert_eq!(
            text_to_save(
                "select 1;\n   \nselect 2;",
                RunTarget::Selection { from: DocOffset(10), to: DocOffset(13) },
                PG
            ),
            Err(SavedQueryError::NothingToSave)
        );
        assert_eq!(text_to_save("select 1;\n\n\nselect 2;", cursor(11), PG), Err(SavedQueryError::NothingToSave));
        assert_eq!(text_to_save(" \n ", RunTarget::All, PG), Err(SavedQueryError::NothingToSave));
        let at_limit = format!("select '{}'", "x".repeat(SQL_LIMIT_BYTES - 9));
        assert_eq!(text_to_save(&at_limit, RunTarget::All, PG).unwrap().len(), SQL_LIMIT_BYTES);
        assert_eq!(
            text_to_save(&format!("{at_limit} x"), RunTarget::All, PG),
            Err(SavedQueryError::TooLong { limit_bytes: 65_536 })
        );
    }

    #[tokio::test]
    async fn a_new_query_is_named_by_its_leading_comment_else_its_first_characters() {
        let (engine, _dir) = engine().await;
        let commented = engine
            .saved_query_save("-- Active sessions\nselect pid from pg_stat_activity;\n\nselect 2;", cursor(25), PG)
            .unwrap();
        assert_eq!(commented.queries.items[0].name, "Active sessions");
        assert_eq!(commented.queries.items[0].excerpt, "select pid from pg_stat_activity;");
        let plain = save_all(&engine, &format!("select '{}'", "a".repeat(100)));
        let named = plain.queries.items.iter().find(|q| q.id == plain.id).unwrap();
        assert_eq!(named.name, format!("select '{}…", "a".repeat(64)));
    }

    #[tokio::test]
    async fn an_edit_renames_and_rewrites_and_a_blank_name_falls_back_to_the_sql() {
        let (engine, _dir) = engine().await;
        let id = save_all(&engine, "select 1").id;
        let renamed = engine
            .saved_query_update(
                id,
                SavedQueryEdit { name: "  Top\n customers ".into(), sql: "  select * from customers  ".into() },
            )
            .unwrap();
        assert_eq!(
            (renamed.items[0].name.as_str(), renamed.items[0].sql.as_str()),
            ("Top customers", "select * from customers")
        );
        let fallback = engine
            .saved_query_update(id, SavedQueryEdit { name: " ".into(), sql: "-- Revenue\nselect 42".into() })
            .unwrap();
        assert_eq!(fallback.items[0].name, "Revenue");
        let capped =
            engine.saved_query_update(id, SavedQueryEdit { name: "n".repeat(300), sql: "select 1".into() }).unwrap();
        assert_eq!(capped.items[0].name.chars().count(), 200);
        assert_eq!(
            engine.saved_query_update(id, SavedQueryEdit { name: "x".into(), sql: " \n".into() }),
            Err(SavedQueryError::NothingToSave)
        );
        assert_eq!(
            engine.saved_query_update(SavedQueryId(999), SavedQueryEdit { name: "x".into(), sql: "select 1".into() }),
            Err(SavedQueryError::NotFound)
        );
    }

    #[tokio::test]
    async fn use_orders_the_list_and_an_edit_leaves_the_order_alone() {
        let (engine, _dir) = engine().await;
        let first = save_all(&engine, "-- first\nselect 1").id;
        let second = save_all(&engine, "-- second\nselect 2").id;
        assert_eq!(names(&engine.boot().saved_queries), ["second", "first"]);
        std::thread::sleep(std::time::Duration::from_millis(2));
        assert_eq!(names(&engine.saved_query_used(first).unwrap()), ["first", "second"]);
        let edited = engine
            .saved_query_update(second, SavedQueryEdit { name: "second, edited".into(), sql: "select 2".into() })
            .unwrap();
        assert_eq!(names(&edited), ["first", "second, edited"]);
    }

    #[tokio::test]
    async fn only_a_write_that_changes_a_row_raises_the_revision_and_is_announced() {
        let (engine, _dir) = engine().await;
        let mut events = engine.subscribe();
        let boot = engine.boot().saved_queries;
        assert_eq!((boot.rev, boot.items.len()), (1, 0), "the boot snapshot beats the webview's empty rev 0");

        let saved = save_all(&engine, "select 1");
        let again = save_all(&engine, "select 1");
        let used = engine.saved_query_used(saved.id).unwrap();
        let edited =
            engine.saved_query_update(saved.id, SavedQueryEdit { name: "one".into(), sql: "select 1".into() }).unwrap();
        let used_missing = engine.saved_query_used(SavedQueryId(999)).unwrap();
        let deleted = engine.saved_query_delete(saved.id).unwrap();
        let deleted_again = engine.saved_query_delete(saved.id).unwrap();

        let revs = [
            saved.queries.rev,
            again.queries.rev,
            used.rev,
            edited.rev,
            used_missing.rev,
            deleted.rev,
            deleted_again.rev,
        ];
        assert_eq!(revs, [2, 2, 3, 4, 4, 5, 5]);
        assert_eq!(announced(&mut events), [2, 3, 4, 5]);
        assert!(deleted_again.items.is_empty());
        assert_eq!(engine.boot().saved_queries, deleted_again);
    }

    #[tokio::test]
    async fn saved_queries_survive_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let config = || EngineConfig {
            data_dir: dir.path().to_owned(),
            keys: Arc::new(MemoryKeyStore::default()),
            agent_path: AgentPath::Fixed("".into()),
            az_binary: None,
            user_known_hosts: None,
            updates: Arc::new(ScriptedTransport::named("off").unwrap()),
        };
        let engine = Engine::open(config()).await.unwrap();
        let saved = engine.saved_query_save("-- Kept\nselect 1", RunTarget::All, DatabaseKind::SqlServer).unwrap();
        engine.shutdown().await;
        drop(engine);
        let reopened = Engine::open(config()).await.unwrap().boot().saved_queries;
        assert_eq!(reopened.items, saved.queries.items);
    }

    #[tokio::test]
    async fn a_row_with_a_kind_this_build_cannot_read_is_left_out_and_never_written() {
        let (engine, _dir) = engine().await;
        raw(
            &engine,
            "INSERT INTO saved_queries (name, sql, database_kind, created_ms, used_ms) VALUES ('newer', 'select 1', 'MySql', 1, 9999999999999)",
        );
        let saved = save_all(&engine, "select 1");
        assert_eq!(names(&saved.queries), ["select 1"]);
        engine.saved_query_update(saved.id, SavedQueryEdit { name: "mine".into(), sql: "select 2".into() }).unwrap();
        engine.saved_query_used(saved.id).unwrap();
        engine.saved_query_delete(saved.id).unwrap();
        assert!(engine.boot().saved_queries.items.is_empty());
        let newer = engine.shared.store.with_saved(|c, _| {
            c.query_row("SELECT name, sql, database_kind, created_ms, used_ms FROM saved_queries", [], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                ))
            })
        });
        assert_eq!(newer.unwrap(), ("newer".into(), "select 1".into(), "MySql".into(), 1, 9_999_999_999_999));
    }

    #[tokio::test]
    async fn a_column_a_newer_build_added_keeps_its_values_through_every_write() {
        let (engine, _dir) = engine().await;
        raw(&engine, "ALTER TABLE saved_queries ADD COLUMN folder TEXT DEFAULT 'x'");
        let first = save_all(&engine, "select 1").id;
        let second = save_all(&engine, "select 2").id;
        raw(&engine, &format!("UPDATE saved_queries SET folder = 'work' WHERE id = {}", first.0));
        engine.saved_query_update(first, SavedQueryEdit { name: "renamed".into(), sql: "select 10".into() }).unwrap();
        engine.saved_query_used(first).unwrap();
        let left = engine.saved_query_delete(second).unwrap();
        assert_eq!(names(&left), ["renamed"]);
        let folder = engine.shared.store.with_saved(|c, _| {
            c.query_row("SELECT folder FROM saved_queries WHERE id = ?1", [first.0], |r| r.get::<_, String>(0))
        });
        assert_eq!(folder.unwrap(), "work");
    }
}
