#![cfg(feature = "mssql-tests")]

use engine::model::*;
use engine::{AgentPath, Engine, EngineConfig, MemoryKeyStore};
use futures_util::FutureExt;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;

struct Harness {
    engine: Arc<Engine>,
    events: broadcast::Receiver<AppEvent>,
    db: DatabaseRef,
    tab: TabId,
    _dir: tempfile::TempDir,
}

impl Harness {
    async fn open(database: &str) -> Self {
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
        let events = engine.subscribe();
        let connection = engine
            .save_connection(ConnectionDraft {
                database_kind: DatabaseKind::SqlServer,
                id: None,
                name: "SQL Server header ordering".into(),
                host: std::env::var("L1_DATA_TEST_MSSQL_HOST").unwrap_or_else(|_| "localhost".into()),
                port: std::env::var("L1_DATA_TEST_MSSQL_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(1434),
                database: database.into(),
                user: "sa".into(),
                password: PasswordInput::Set(
                    std::env::var("L1_DATA_TEST_MSSQL_PASSWORD").unwrap_or_else(|_| "L1Data_dev_Str0ng".into()),
                ),
                ssl_mode: SslMode::Require,
                ca_cert_path: None,
                color: None,
                ssh: None,
                environment: None,
                sql_server_auth: None,
            })
            .await
            .unwrap();
        engine.connect(connection.id).await.unwrap();
        let db = DatabaseRef { connection_id: connection.id, database: database.into() };
        let tab = TabId(uuid::Uuid::new_v4());
        engine
            .console_configure(
                tab,
                ConsoleConfig {
                    connection_id: connection.id,
                    database: db.database.clone(),
                    schema: None,
                    mode: TxnMode::AutoCommit,
                },
            )
            .await
            .unwrap();
        Self { engine, events, db, tab, _dir: dir }
    }

    async fn request(&mut self, request: RunRequest) -> ConsoleState {
        let run = self.engine.console_run(self.tab, request, None).await.unwrap().run_id;
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if let AppEvent::ConsoleChanged(state) = self.events.recv().await.unwrap()
                    && state.tab_id == self.tab
                    && matches!(state.run, RunState::Finished { run_id, .. } if run_id == run)
                {
                    return state;
                }
            }
        })
        .await
        .expect("SQL Server run did not finish")
    }

    async fn sql(&mut self, text: &str) -> ConsoleState {
        let state = self
            .request(RunRequest::Script {
                text: text.into(),
                target: RunTarget::All,
                mode: RunMode::Run,
                params: Vec::new(),
                cells: Vec::new(),
            })
            .await;
        Self::assert_ok(&state);
        state
    }

    async fn browse(&mut self, table: &str, order: &str) -> ConsoleState {
        self.request(RunRequest::TablePage {
            schema: "dbo".into(),
            table: table.into(),
            page: 0,
            query: TableQuery { order_by: order.into(), ..Default::default() },
        })
        .await
    }

    fn assert_ok(state: &ConsoleState) {
        assert!(matches!(state.run, RunState::Finished { outcome: RunOutcome::Ok, .. }), "{:?}", state.run);
    }

    fn rows(&self, state: &ConsoleState) -> Vec<Vec<Option<String>>> {
        Self::assert_ok(state);
        let RunState::Finished { statements, .. } = &state.run else { unreachable!() };
        let result = statements
            .iter()
            .flat_map(|s| &s.outputs)
            .find_map(|o| match o {
                RunOutput::Rows { result, .. } => Some(result),
                _ => None,
            })
            .expect("row output");
        self.engine.result_page(result.r#ref, 0, 100).unwrap().rows
    }

    async fn cleanup_table(&mut self, table: &str, outcome: std::thread::Result<()>) {
        let state = self.sql(&format!("DROP TABLE IF EXISTS dbo.[{table}]; SELECT OBJECT_ID(N'dbo.{table}')")).await;
        assert_eq!(self.rows(&state), vec![vec![None]]);
        self.engine.shutdown().await;
        if let Err(panic) = outcome {
            std::panic::resume_unwind(panic);
        }
    }
}

#[tokio::test]
async fn additive_headers_refuse_unresolved_case_insensitive_order_names() {
    let mut h = Harness::open("shop").await;
    let table = format!("l1_header_ci_{}", uuid::Uuid::new_v4().simple());
    let outcome = AssertUnwindSafe(async {
        h.sql(&format!("CREATE TABLE dbo.[{table}] (id int PRIMARY KEY); INSERT INTO dbo.[{table}] VALUES (1),(2)"))
            .await;
        h.engine.refresh_catalog(h.db.clone()).await.unwrap();
        for order in ["ID DESC", "[ID] DESC", "\"ID\" DESC"] {
            let raw = h.browse(&table, order).await;
            let inspected = h.engine.table_order(h.db.clone(), "dbo", &table, order, TableOrderEdit::Inspect).unwrap();
            assert_eq!(inspected.sql, order);
            assert!(inspected.keys.is_none());
            let error = h
                .engine
                .table_order(
                    h.db.clone(),
                    "dbo",
                    &table,
                    order,
                    TableOrderEdit::Cycle { column: "id".into(), additive: true },
                )
                .unwrap_err();
            assert!(error.contains("exact table column names") && error.contains("without Shift"), "{error}");
            assert_eq!(h.rows(&raw), vec![vec![Some("2".into())], vec![Some("1".into())]]);
            let sorted = h
                .engine
                .table_order(
                    h.db.clone(),
                    "dbo",
                    &table,
                    order,
                    TableOrderEdit::Cycle { column: "id".into(), additive: false },
                )
                .unwrap();
            assert_eq!(sorted.sql, "[id] ASC");
            let state = h.browse(&table, &sorted.sql).await;
            assert_eq!(h.rows(&state), vec![vec![Some("1".into())], vec![Some("2".into())]]);
        }
    })
    .catch_unwind()
    .await;
    h.cleanup_table(&table, outcome).await;
}

#[tokio::test]
async fn case_sensitive_headers_keep_distinct_columns_and_refuse_accent_aliases() {
    let mut admin = Harness::open("shop").await;
    let database = format!("l1_header_cs_{}", uuid::Uuid::new_v4().simple());
    let mut h = None;
    let outcome = AssertUnwindSafe(async {
        admin.sql(&format!("CREATE DATABASE [{database}] COLLATE Latin1_General_100_CS_AI")).await;
        h = Some(Harness::open(&database).await);
        let h = h.as_mut().unwrap();
        h.sql("CREATE TABLE dbo.header_order ([id] int PRIMARY KEY, [ID] int, [café] int); INSERT INTO dbo.header_order VALUES (1,20,2),(2,10,1),(3,20,3)")
            .await;
        h.engine.refresh_catalog(h.db.clone()).await.unwrap();
        let sorted =
            h.engine.table_order(h.db.clone(), "dbo", "header_order", "ID DESC", TableOrderEdit::Cycle { column: "id".into(), additive: true }).unwrap();
        assert_eq!(sorted.sql, "[ID] DESC, [id] ASC");
        let keys = sorted.keys.unwrap();
        assert_eq!(keys.iter().map(|k| (k.column.as_str(), k.desc)).collect::<Vec<_>>(), [("ID", true), ("id", false)]);
        let state = h.browse("header_order", &sorted.sql).await;
        assert_eq!(h.rows(&state).iter().map(|r| r[0].as_deref()).collect::<Vec<_>>(), [Some("1"), Some("3"), Some("2")]);
        let sorted =
            h.engine.table_order(h.db.clone(), "dbo", "header_order", &sorted.sql, TableOrderEdit::Cycle { column: "id".into(), additive: true }).unwrap();
        assert_eq!(sorted.sql, "[ID] DESC, [id] DESC");
        let state = h.browse("header_order", &sorted.sql).await;
        assert_eq!(h.rows(&state).iter().map(|r| r[0].as_deref()).collect::<Vec<_>>(), [Some("3"), Some("1"), Some("2")]);
        let sorted =
            h.engine.table_order(h.db.clone(), "dbo", "header_order", &sorted.sql, TableOrderEdit::Cycle { column: "id".into(), additive: true }).unwrap();
        assert_eq!(sorted.sql, "[ID] DESC");
        Harness::assert_ok(&h.browse("header_order", &sorted.sql).await);
        let raw = h.browse("header_order", "cafe DESC").await;
        assert_eq!(h.rows(&raw).iter().map(|r| r[0].as_deref()).collect::<Vec<_>>(), [Some("3"), Some("1"), Some("2")]);
        let inspected = h.engine.table_order(h.db.clone(), "dbo", "header_order", "cafe DESC", TableOrderEdit::Inspect).unwrap();
        assert_eq!(inspected.sql, "cafe DESC");
        assert!(inspected.keys.is_none());
        assert!(
            h.engine.table_order(h.db.clone(), "dbo", "header_order", "cafe DESC", TableOrderEdit::Cycle { column: "café".into(), additive: true }).is_err()
        );
        let replacement =
            h.engine.table_order(h.db.clone(), "dbo", "header_order", "cafe DESC", TableOrderEdit::Cycle { column: "café".into(), additive: false }).unwrap();
        assert_eq!(replacement.sql, "[café] ASC");
        let state = h.browse("header_order", &replacement.sql).await;
        assert_eq!(h.rows(&state).iter().map(|r| r[0].as_deref()).collect::<Vec<_>>(), [Some("2"), Some("1"), Some("3")]);
    })
    .catch_unwind()
    .await;
    if let Some(h) = h {
        h.engine.disconnect(h.db.connection_id, true).await.unwrap();
        h.engine.shutdown().await;
    }
    let state = admin.sql(&format!("DROP DATABASE IF EXISTS [{database}]; SELECT DB_ID(N'{database}')")).await;
    assert_eq!(admin.rows(&state), vec![vec![None]]);
    admin.engine.shutdown().await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test]
async fn header_sort_uses_only_the_loaded_table_catalog_and_rechecks_changed_columns() {
    let mut h = Harness::open("shop").await;
    let table = format!("l1_header_catalog_{}", uuid::Uuid::new_v4().simple());
    let outcome = AssertUnwindSafe(async {
        h.sql(&format!("CREATE TABLE dbo.[{table}] (id int PRIMARY KEY, priority int); INSERT INTO dbo.[{table}] VALUES (1,2),(2,1)")).await;
        h.engine.refresh_catalog(h.db.clone()).await.unwrap();
        let engine = h.engine.clone();
        let db = h.db.clone();
        let target = table.clone();
        let sorted =
            std::thread::spawn(move || engine.table_order(db, "dbo", &target, "id ASC", TableOrderEdit::Cycle { column: "priority".into(), additive: true }))
                .join()
                .unwrap()
                .unwrap();
        assert_eq!(sorted.sql, "[id] ASC, [priority] ASC");
        for (db, schema, target) in [
            (h.db.clone(), "missing_schema", table.as_str()),
            (h.db.clone(), "dbo", "missing_table"),
            (DatabaseRef { database: "reporting".into(), ..h.db.clone() }, "dbo", table.as_str()),
        ] {
            let inspected = h.engine.table_order(db.clone(), schema, target, "id", TableOrderEdit::Inspect).unwrap();
            assert_eq!(inspected.sql, "id");
            assert!(inspected.keys.is_none());
            let error = h.engine.table_order(db, schema, target, "id", TableOrderEdit::Cycle { column: "id".into(), additive: false }).unwrap_err();
            assert!(error.contains("catalog") && error.contains("refresh"), "{error}");
        }
        h.sql(&format!("ALTER TABLE dbo.[{table}] DROP COLUMN priority")).await;
        h.engine.refresh_catalog(h.db.clone()).await.unwrap();
        let inspected = h.engine.table_order(h.db.clone(), "dbo", &table, "priority DESC", TableOrderEdit::Inspect).unwrap();
        assert_eq!(inspected.sql, "priority DESC");
        assert!(inspected.keys.is_none());
        let error = h.engine.table_order(h.db.clone(), "dbo", &table, "id", TableOrderEdit::Cycle { column: "priority".into(), additive: false }).unwrap_err();
        assert!(error.contains("priority") && error.contains("Refresh"), "{error}");
        assert!(h.engine.table_order(h.db.clone(), "dbo", &table, "priority DESC", TableOrderEdit::Cycle { column: "id".into(), additive: true }).is_err());
        let replacement =
            h.engine.table_order(h.db.clone(), "dbo", &table, "priority DESC", TableOrderEdit::Cycle { column: "id".into(), additive: false }).unwrap();
        let state = h.browse(&table, &replacement.sql).await;
        assert_eq!(h.rows(&state), vec![vec![Some("1".into())], vec![Some("2".into())]]);
        h.sql(&format!("DROP TABLE dbo.[{table}]")).await;
        h.engine.refresh_catalog(h.db.clone()).await.unwrap();
        assert!(h.engine.table_order(h.db.clone(), "dbo", &table, "id", TableOrderEdit::Cycle { column: "id".into(), additive: false }).is_err());
    })
    .catch_unwind()
    .await;
    h.cleanup_table(&table, outcome).await;
}
