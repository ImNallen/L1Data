use rusqlite::{Connection, Row, params};

use crate::model::*;
use crate::sql::history_text;
use crate::store::{Store, StoreError};

const GROUPS: &str = "WITH groups AS (
    SELECT min(id) AS group_id, max(id) AS latest_id,
        max(CASE WHEN json_extract(outcome_json, '$.kind') = 'Error' THEN id END) AS failed_id,
        count(*) AS run_count,
        sum(json_extract(outcome_json, '$.kind') = 'Error') AS failed_count,
        max(is_write) AS is_write,
        max(CASE WHEN ?1 = '' OR instr(history_lower(sql), ?1) > 0 OR instr(history_lower(connection_name), ?1) > 0
            OR instr(history_lower(database), ?1) > 0
            OR (json_extract(outcome_json, '$.kind') = 'Error' AND instr(history_lower(json_extract(outcome_json, '$.message')), ?1) > 0)
            THEN 1 ELSE 0 END) AS matches
    FROM history GROUP BY sql, connection_id, database, database_kind
) ";

fn database_kind(raw: String) -> rusqlite::Result<DatabaseKind> {
    serde_json::from_value(serde_json::Value::String(raw))
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))
}

fn connection_id(raw: String) -> rusqlite::Result<ConnectionId> {
    raw.parse()
        .map(ConnectionId)
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))
}

fn run(row: &Row<'_>, offset: usize) -> rusqlite::Result<HistoryRun> {
    let json: String = row.get(offset + 4)?;
    Ok(HistoryRun {
        id: row.get(offset)?,
        at_ms: row.get(offset + 1)?,
        connection_name: row.get(offset + 2)?,
        elapsed_ms: row.get(offset + 3)?,
        outcome: serde_json::from_str(&json).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(offset + 4, rusqlite::types::Type::Text, Box::new(e))
        })?,
    })
}

fn summary(row: &Row<'_>) -> rusqlite::Result<HistoryGroupSummary> {
    let sql: String = row.get(8)?;
    let (title, excerpt) = history_text(&sql);
    Ok(HistoryGroupSummary {
        database_kind: database_kind(row.get(14)?)?,
        id: row.get(0)?,
        latest_id: row.get(1)?,
        latest_at_ms: row.get(2)?,
        connection_id: connection_id(row.get(3)?)?,
        database: row.get(4)?,
        run_count: row.get(5)?,
        failed_count: row.get(6)?,
        is_write: row.get(7)?,
        title,
        excerpt,
        representative: run(row, 9)?,
    })
}

fn unavailable(error: StoreError) -> HistoryError {
    HistoryError::Unavailable { message: error.to_string() }
}

fn browse(c: &Connection, q: &HistoryBrowseQuery, revision: u64) -> Result<HistoryBrowsePage, StoreError> {
    let needle = q.text.as_deref().unwrap_or("").trim().to_lowercase();
    let counts = c.query_row(&format!("{GROUPS} SELECT count(*), coalesce(sum(failed_count > 0), 0), coalesce(sum(is_write), 0) FROM groups WHERE matches"), [&needle], |r| {
        Ok(HistoryCounts { all: r.get(0)?, failed: r.get(1)?, writes: r.get(2)? })
    })?;
    let filter = match q.filter {
        HistoryFilter::All => "All",
        HistoryFilter::Failed => "Failed",
        HistoryFilter::Writes => "Writes",
    };
    let mut stmt = c.prepare(&format!(
        "{GROUPS}
        SELECT g.group_id, g.latest_id, latest.at_ms, h.connection_id, h.database,
            g.run_count, g.failed_count, g.is_write, h.sql,
            h.id, h.at_ms, h.connection_name, h.elapsed_ms, h.outcome_json, h.database_kind
        FROM groups g JOIN history latest ON latest.id = g.latest_id
        JOIN history h ON h.id = CASE WHEN ?2 = 'Failed' THEN g.failed_id ELSE g.latest_id END
        WHERE g.matches AND (?2 = 'All' OR (?2 = 'Failed' AND g.failed_count > 0) OR (?2 = 'Writes' AND g.is_write))
            AND (?3 IS NULL OR g.latest_id < ?3)
        ORDER BY g.latest_id DESC LIMIT ?4"
    ))?;
    let limit = usize::from(q.limit.clamp(1, 100));
    let mut groups = stmt
        .query_map(params![needle, filter, q.before_id, (limit + 1) as i64], summary)?
        .collect::<Result<Vec<_>, _>>()?;
    let has_more = groups.len() > limit;
    groups.truncate(limit);
    Ok(HistoryBrowsePage { groups, counts, has_more, revision })
}

impl Store {
    pub fn history_browse(&self, q: &HistoryBrowseQuery) -> Result<HistoryBrowsePage, HistoryError> {
        self.with_history(|c, revision| {
            if q.revision.is_some_and(|r| r != *revision) || (q.before_id.is_some() && q.revision.is_none()) {
                return Ok(Err(HistoryError::Stale));
            }
            Ok(Ok(browse(c, q, *revision)?))
        })
        .map_err(unavailable)?
    }

    pub fn history_group(&self, q: &HistoryGroupQuery) -> Result<HistoryGroupPage, HistoryError> {
        self.with_history(|c, revision| {
            if q.revision != *revision { return Ok(Err(HistoryError::Stale)); }
            let mut stmt = c.prepare("SELECT h.id, h.at_ms, h.connection_name, h.elapsed_ms, h.outcome_json
                FROM history h JOIN history anchor ON anchor.id = ?1
                WHERE h.sql = anchor.sql AND h.connection_id = anchor.connection_id AND h.database = anchor.database AND h.database_kind = anchor.database_kind
                    AND (?2 IS NULL OR h.id < ?2) ORDER BY h.id DESC LIMIT ?3")?;
            let limit = usize::from(q.limit.clamp(1, 100));
            let mut runs = stmt.query_map(params![q.id, q.before_id, (limit + 1) as i64], |r| run(r, 0))?.collect::<Result<Vec<_>, _>>()?;
            if runs.is_empty() && q.before_id.is_none() { return Ok(Err(HistoryError::Stale)); }
            let (sql, max_elapsed_ms, kind) = c.query_row("SELECT anchor.sql, max(h.elapsed_ms), anchor.database_kind
                FROM history anchor JOIN history h ON h.sql = anchor.sql AND h.connection_id = anchor.connection_id AND h.database = anchor.database AND h.database_kind = anchor.database_kind
                WHERE anchor.id = ?1", [q.id], |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, String>(2)?)))?;
            let has_more = runs.len() > limit;
            runs.truncate(limit);
            Ok(Ok(HistoryGroupPage { database_kind: database_kind(kind)?, sql, runs, has_more, max_elapsed_ms, revision: *revision }))
        }).map_err(unavailable)?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{HISTORY_KEEP, HistoryInsert};

    fn store() -> (Store, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        (Store::open(&dir.path().join("history.sqlite3")).unwrap(), dir)
    }

    fn insert(s: &Store, sql: &str, database: &str, name: &str, outcome: HistoryOutcome) -> HistoryEntry {
        s.insert_history_and_prune(HistoryInsert {
            database_kind: crate::model::DatabaseKind::Postgres,
            at_ms: 123,
            connection_id: ConnectionId(uuid::Uuid::nil()),
            connection_name: name.into(),
            database: database.into(),
            sql: sql.into(),
            elapsed_ms: 23,
            outcome,
        })
        .unwrap()
    }

    fn query() -> HistoryBrowseQuery {
        HistoryBrowseQuery { text: None, filter: HistoryFilter::All, before_id: None, limit: 50, revision: None }
    }

    fn ok() -> HistoryOutcome {
        HistoryOutcome::Rows { rows: 4 }
    }

    #[test]
    fn database_kind_separates_otherwise_identical_history_groups() {
        let (store, _dir) = store();
        for database_kind in [DatabaseKind::Postgres, DatabaseKind::SqlServer] {
            store
                .insert_history_and_prune(HistoryInsert {
                    database_kind,
                    at_ms: 1,
                    connection_id: ConnectionId(uuid::Uuid::nil()),
                    connection_name: "same".into(),
                    database: "shop".into(),
                    sql: "SELECT 1".into(),
                    elapsed_ms: 1,
                    outcome: ok(),
                })
                .unwrap();
        }
        let page = store.history_browse(&query()).unwrap();
        assert_eq!(page.groups.len(), 2);
        for group in &page.groups {
            let detail = store
                .history_group(&HistoryGroupQuery { id: group.id, before_id: None, limit: 10, revision: page.revision })
                .unwrap();
            assert_eq!(detail.database_kind, group.database_kind);
            assert_eq!(detail.runs.len(), 1);
        }
    }
    fn failed() -> HistoryOutcome {
        HistoryOutcome::Error { message: "permission denied for orders".into() }
    }

    #[test]
    fn unicode_search_folds_sql_connection_database_and_errors_consistently() {
        let (s, _dir) = store();
        let saved = insert(
            &s,
            "SELECT \"État\" FROM orders",
            "RÉSUMÉ",
            "Örebro",
            HistoryOutcome::Error { message: "Échec".into() },
        );
        for text in ["SELECT \"État\" FROM orders", "état", "Örebro", "örebro", "résumé", "échec"] {
            let page = s.history_browse(&HistoryBrowseQuery { text: Some(text.into()), ..query() }).unwrap();
            assert_eq!(page.groups.iter().map(|g| g.id).collect::<Vec<_>>(), vec![saved.id], "{text}");
        }
    }

    #[test]
    fn exact_groups_keep_database_connection_and_sql_identity() {
        let (s, _dir) = store();
        let first = insert(&s, "select 1", "shop", "old name", ok());
        insert(&s, "select 1", "shop", "new name", ok());
        insert(&s, "select 1", "reporting", "new name", ok());
        insert(&s, "SELECT 1", "shop", "new name", ok());
        insert(&s, "select 1 ", "shop", "new name", ok());
        s.insert_history_and_prune(HistoryInsert {
            database_kind: crate::model::DatabaseKind::Postgres,
            at_ms: 124,
            connection_id: ConnectionId(uuid::Uuid::new_v4()),
            connection_name: "other".into(),
            database: "shop".into(),
            sql: "select 1".into(),
            elapsed_ms: 20,
            outcome: ok(),
        })
        .unwrap();
        let page = s.history_browse(&query()).unwrap();
        assert_eq!(page.counts.all, 5);
        assert_eq!(page.groups.len(), 5);
        let group = page.groups.last().unwrap();
        assert_eq!(
            (group.id, group.run_count, group.representative.connection_name.as_str()),
            (first.id, 2, "new name")
        );
        let detail = s
            .history_group(&HistoryGroupQuery { id: group.id, before_id: None, limit: 1, revision: page.revision })
            .unwrap();
        assert_eq!(
            (detail.sql.as_str(), detail.runs.len(), detail.max_elapsed_ms, detail.has_more),
            ("select 1", 1, 23, true)
        );
        let older = s
            .history_group(&HistoryGroupQuery {
                id: group.id,
                before_id: Some(detail.runs[0].id),
                limit: 1,
                revision: page.revision,
            })
            .unwrap();
        assert_eq!(older.runs[0].connection_name, "old name");
        assert!(!older.has_more);
    }

    #[test]
    fn search_matches_any_member_but_counts_and_timelines_cover_all_runs() {
        let (s, _dir) = store();
        let bad = insert(&s, "update orders set x=1 returning *", "shop", "production", failed());
        insert(&s, "update orders set x=1 returning *", "shop", "renamed", ok());
        insert(&s, "select 1", "shop", "renamed", HistoryOutcome::Cancelled);
        insert(&s, "truncate items", "shop", "renamed", ok());
        let all = s.history_browse(&query()).unwrap();
        assert_eq!((all.counts.all, all.counts.failed, all.counts.writes), (3, 1, 2));
        let filtered = s.history_browse(&HistoryBrowseQuery { filter: HistoryFilter::Failed, ..query() }).unwrap();
        assert_eq!((filtered.counts.all, filtered.counts.failed, filtered.counts.writes), (3, 1, 2));
        assert_eq!((filtered.groups[0].representative.id, filtered.groups[0].run_count), (bad.id, 2));
        for text in ["PRODUCTION", "permission denied", "orders"] {
            let matched = s.history_browse(&HistoryBrowseQuery { text: Some(text.into()), ..query() }).unwrap();
            assert_eq!((matched.counts.all, matched.groups[0].run_count, matched.groups[0].failed_count), (1, 2, 1));
            assert!(matches!(matched.groups[0].representative.outcome, HistoryOutcome::Rows { rows: 4 }));
            let detail = s
                .history_group(&HistoryGroupQuery {
                    id: matched.groups[0].id,
                    before_id: None,
                    limit: 50,
                    revision: matched.revision,
                })
                .unwrap();
            assert_eq!(detail.runs.len(), 2);
        }
        assert_eq!(
            s.history_browse(&HistoryBrowseQuery { text: Some("shop".into()), ..query() }).unwrap().counts.all,
            3
        );
        assert_eq!(s.history_browse(&HistoryBrowseQuery { text: Some("%".into()), ..query() }).unwrap().counts.all, 0);
        assert_eq!(
            s.history_browse(&HistoryBrowseQuery { filter: HistoryFilter::Writes, ..query() }).unwrap().groups.len(),
            2
        );
    }

    #[test]
    fn group_pages_use_latest_ids_and_require_the_same_revision() {
        let (s, _dir) = store();
        let oldest = insert(&s, "select 1", "shop", "dev", ok());
        insert(&s, "select 2", "shop", "dev", ok());
        insert(&s, "select 1", "shop", "dev", ok());
        let first = s.history_browse(&HistoryBrowseQuery { limit: 1, ..query() }).unwrap();
        assert_eq!((first.groups[0].id, first.groups[0].run_count, first.has_more), (oldest.id, 2, true));
        let next = HistoryBrowseQuery {
            before_id: Some(first.groups[0].latest_id),
            revision: Some(first.revision),
            ..query()
        };
        let second = s.history_browse(&next).unwrap();
        assert_eq!(second.groups[0].title, "select 2");
        assert!(!second.has_more);
        insert(&s, "select 3", "shop", "dev", ok());
        assert!(matches!(s.history_browse(&next), Err(HistoryError::Stale)));
        assert!(matches!(
            s.history_group(&HistoryGroupQuery { id: oldest.id, before_id: None, limit: 10, revision: first.revision }),
            Err(HistoryError::Stale)
        ));
        let after = s.history_browse(&query()).unwrap();
        assert!(after.revision > first.revision);
        s.clear_history().unwrap();
        assert!(matches!(
            s.history_browse(&HistoryBrowseQuery { revision: Some(after.revision), ..query() }),
            Err(HistoryError::Stale)
        ));
        let empty = s.history_browse(&query()).unwrap();
        assert_eq!((empty.counts.all, empty.counts.failed, empty.counts.writes, empty.has_more), (0, 0, 0, false));
        assert!(empty.revision > after.revision);
    }

    #[test]
    fn counts_follow_pruning_and_old_schema_backfills_write_metadata() {
        let (s, _dir) = store();
        insert(&s, "delete from orders", "shop", "dev", failed());
        for _ in 0..HISTORY_KEEP {
            insert(&s, "select 1", "shop", "dev", ok());
        }
        let page = s.history_browse(&query()).unwrap();
        assert_eq!(
            (page.counts.all, page.counts.failed, page.counts.writes, page.groups[0].run_count),
            (1, 0, 0, HISTORY_KEEP as u32)
        );

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.sqlite3");
        let old = Connection::open(&path).unwrap();
        old.execute_batch("CREATE TABLE connections (id TEXT PRIMARY KEY, json TEXT NOT NULL);
            CREATE TABLE history (id INTEGER PRIMARY KEY, at_ms INTEGER NOT NULL, connection_id TEXT NOT NULL,
                connection_name TEXT NOT NULL, database TEXT NOT NULL, sql TEXT NOT NULL, elapsed_ms INTEGER NOT NULL, outcome_json TEXT NOT NULL);
            CREATE TABLE kv (key TEXT PRIMARY KEY, json TEXT NOT NULL);
            CREATE TABLE secrets (connection_id TEXT PRIMARY KEY, nonce BLOB NOT NULL, ciphertext BLOB NOT NULL);
            CREATE TABLE connection_views (connection_id TEXT PRIMARY KEY, json TEXT NOT NULL);
            PRAGMA user_version = 3;").unwrap();
        old.execute("INSERT INTO history VALUES (1, 1, ?1, 'dev', 'shop', 'truncate t', 2, '{\"kind\":\"Command\",\"tag\":\"TRUNCATE\"}')", [uuid::Uuid::nil().to_string()]).unwrap();
        old.execute_batch("CREATE TRIGGER fail_metadata BEFORE UPDATE ON history BEGIN SELECT RAISE(ABORT, 'test migration failure'); END;").unwrap();
        drop(old);
        assert!(Store::open(&path).is_err());
        let rolled_back = Connection::open(&path).unwrap();
        assert_eq!(rolled_back.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0)).unwrap(), 3);
        assert!(rolled_back.prepare("SELECT is_write FROM history").is_err());
        rolled_back.execute_batch("DROP TRIGGER fail_metadata").unwrap();
        drop(rolled_back);
        let migrated = Store::open(&path).unwrap();
        assert_eq!(migrated.history_browse(&query()).unwrap().counts.writes, 1);
        drop(migrated);
        assert_eq!(Store::open(&path).unwrap().history_browse(&query()).unwrap().counts.writes, 1);
    }
}
