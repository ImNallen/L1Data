#![cfg(feature = "pg-tests")]

use engine::model::*;
use engine::{AgentPath, Engine, EngineConfig, MemoryKeyStore, draft_from_url};
use futures_util::FutureExt;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

struct Harness {
    engine: Arc<Engine>,
    db: DatabaseRef,
    editor: TabId,
    ddl: TabId,
    observer: TabId,
    schema: String,
    _dir: tempfile::TempDir,
}

impl Harness {
    async fn open() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::open(EngineConfig {
            data_dir: dir.path().into(),
            keys: Arc::new(MemoryKeyStore::default()),
            agent_path: AgentPath::Fixed("".into()),
            az_binary: None,
            user_known_hosts: None,
            updates: Arc::new(engine::ScriptedTransport::named("off").unwrap()),
        })
        .await
        .unwrap();
        let url =
            std::env::var("L1_DATA_TEST_PG").unwrap_or_else(|_| "postgres://l1data:l1data@localhost:5433/shop".into());
        let editor = engine.save_connection(draft_from_url(&url, "key identity editor").unwrap()).await.unwrap();
        let other = engine.save_connection(draft_from_url(&url, "key identity observer").unwrap()).await.unwrap();
        engine.connect(editor.id).await.unwrap();
        engine.connect(other.id).await.unwrap();
        let h = Self {
            engine,
            db: DatabaseRef { connection_id: editor.id, database: editor.database.clone() },
            editor: TabId(uuid::Uuid::new_v4()),
            ddl: TabId(uuid::Uuid::new_v4()),
            observer: TabId(uuid::Uuid::new_v4()),
            schema: format!("l1_key_identity_{}", uuid::Uuid::new_v4().simple()),
            _dir: dir,
        };
        for (tab, connection_id) in [(h.editor, editor.id), (h.ddl, other.id), (h.observer, other.id)] {
            h.engine
                .console_configure(
                    tab,
                    ConsoleConfig {
                        connection_id,
                        database: editor.database.clone(),
                        schema: None,
                        mode: TxnMode::AutoCommit,
                    },
                )
                .await
                .unwrap();
        }
        h
    }

    async fn request(&self, tab: TabId, request: RunRequest) -> ConsoleState {
        let mut events = self.engine.subscribe();
        let accepted = self.engine.console_run(tab, request, None).await.unwrap();
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                if let AppEvent::ConsoleChanged(state) = events.recv().await.unwrap()
                    && state.tab_id == tab
                    && matches!(state.run, RunState::Finished { run_id, .. } if run_id == accepted.run_id)
                {
                    assert!(matches!(state.run, RunState::Finished { outcome: RunOutcome::Ok, .. }), "{:?}", state.run);
                    return state;
                }
            }
        })
        .await
        .expect("PostgreSQL key identity run did not finish")
    }

    fn result(state: &ConsoleState) -> &ResultMeta {
        let RunState::Finished { statements, .. } = &state.run else { panic!("run not finished") };
        statements
            .iter()
            .flat_map(|s| &s.outputs)
            .find_map(|o| match o {
                RunOutput::Rows { result, .. } => Some(result),
                _ => None,
            })
            .expect("query returned rows")
    }

    async fn sql(&self, tab: TabId, text: &str) -> ConsoleState {
        self.request(
            tab,
            RunRequest::Script {
                text: text.into(),
                target: RunTarget::All,
                mode: RunMode::Run,
                params: Vec::new(),
                cells: Vec::new(),
            },
        )
        .await
    }

    fn rows(&self, state: &ConsoleState) -> Vec<Vec<Option<String>>> {
        self.engine.result_page(Self::result(state).r#ref, 0, 10).unwrap().rows
    }

    async fn cleanup(&self) {
        for tab in [self.editor, self.ddl, self.observer] {
            self.engine.console_cancel(tab).await;
            let _ = self.engine.console_rollback(tab).await;
            self.engine.console_close(tab, true).await.unwrap();
        }
        self.engine
            .console_configure(
                self.observer,
                ConsoleConfig {
                    connection_id: self.db.connection_id,
                    database: self.db.database.clone(),
                    schema: None,
                    mode: TxnMode::AutoCommit,
                },
            )
            .await
            .unwrap();
        let state = self
            .sql(
                self.observer,
                &format!(
                    "DROP SCHEMA IF EXISTS \"{}\" CASCADE; SELECT to_regnamespace('{}')::text",
                    self.schema, self.schema
                ),
            )
            .await;
        assert_eq!(self.rows(&state), vec![vec![None]], "disposable schema was removed");
        self.engine.shutdown().await;
        eprintln!("key identity cleanup verified");
    }
}

#[tokio::test]
async fn named_keys_refuse_updates_and_deletes_after_pk_replacement_and_catalog_refresh() {
    let h = Harness::open().await;
    let outcome = AssertUnwindSafe(async {
        h.sql(h.ddl, &format!("CREATE SCHEMA \"{}\"", h.schema)).await;
        for (table, mode) in [("auto_rows", TxnMode::AutoCommit), ("manual_rows", TxnMode::Manual)] {
            h.engine.console_configure(h.editor, ConsoleConfig {
                connection_id: h.db.connection_id, database: h.db.database.clone(), schema: None, mode: TxnMode::AutoCommit,
            }).await.unwrap();
            let target = format!("\"{}\".\"{table}\"", h.schema);
            h.sql(h.ddl, &format!("CREATE TABLE {target} (id int PRIMARY KEY, alternate int NOT NULL, note text); INSERT INTO {target} VALUES (1,2,'selected'),(2,1,'other')")).await;
            h.engine.refresh_catalog(h.db.clone()).await.unwrap();
            let catalog = h.engine.catalog(h.db.clone()).await.unwrap();
            let relation = catalog.schemas.iter().find(|s| s.name == h.schema).unwrap().relations.iter().find(|r| r.name == table).unwrap();
            assert_eq!(relation.columns.iter().filter(|c| c.pk).map(|c| c.name.as_str()).collect::<Vec<_>>(), ["id"]);
            let page = h.request(h.editor, RunRequest::TablePage {
                schema: h.schema.clone(), table: table.into(), page: 0,
                query: TableQuery { order_by: "id".into(), ..Default::default() },
            }).await;
            let original = h.rows(&page);
            assert_eq!(original, vec![
                vec![Some("1".into()), Some("2".into()), Some("selected".into())],
                vec![Some("2".into()), Some("1".into()), Some("other".into())],
            ]);
            let key: Vec<CellWrite> = relation.columns.iter().filter(|c| c.pk).map(|column| {
                let index = Harness::result(&page).columns.iter().position(|c| c.name == column.name).unwrap();
                CellWrite { column: column.name.clone(), value: original[0][index].clone() }
            }).collect();
            assert_eq!(key, [CellWrite { column: "id".into(), value: Some("1".into()) }]);
            let update = TableChanges {
                schema: h.schema.clone(), table: table.into(), inserts: vec![], deletes: vec![],
                updates: vec![RowUpdate { key: key.clone(), set: vec![CellWrite { column: "note".into(), value: Some("edited".into()) }] }],
            };
            let delete = TableChanges { updates: vec![], deletes: vec![key], ..update.clone() };
            h.sql(h.ddl, &format!("ALTER TABLE {target} DROP CONSTRAINT {table}_pkey; ALTER TABLE {target} ADD PRIMARY KEY (alternate)")).await;
            h.engine.refresh_catalog(h.db.clone()).await.unwrap();
            let refreshed = h.engine.catalog(h.db.clone()).await.unwrap();
            assert!(refreshed.version > catalog.version);
            let current = refreshed.schemas.iter().find(|s| s.name == h.schema).unwrap().relations.iter().find(|r| r.name == table).unwrap();
            assert_eq!(current.oid, relation.oid);
            assert_eq!(current.columns.iter().filter(|c| c.pk).map(|c| c.name.as_str()).collect::<Vec<_>>(), ["alternate"]);
            h.engine.console_configure(h.editor, ConsoleConfig {
                connection_id: h.db.connection_id, database: h.db.database.clone(), schema: None, mode,
            }).await.unwrap();
            for (kind, changes) in [("update", update), ("delete", delete)] {
                let applied = h.engine.table_apply(h.editor, changes, None).await;
                assert!(matches!(applied, Err(ApplyError::NoPrimaryKey)), "{mode:?} {kind}: {applied:?}");
                let observed = h.sql(h.observer, &format!("SELECT id,alternate,note FROM {target} ORDER BY id")).await;
                let rows = h.rows(&observed);
                assert_eq!(rows, original, "{mode:?} {kind} must not rebind id=1 to alternate=1");
                eprintln!("{mode:?} {kind}: {applied:?}; observer={rows:?}");
            }
        }
    }).catch_unwind().await;
    h.cleanup().await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}
