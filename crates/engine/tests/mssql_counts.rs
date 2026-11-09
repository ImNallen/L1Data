#![cfg(feature = "mssql-tests")]

use engine::model::*;
use engine::{AgentPath, Engine, EngineConfig, MemoryKeyStore};
use futures_util::FutureExt;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

static FIXTURES: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct Harness {
    engine: Arc<Engine>,
    db: DatabaseRef,
    tab: TabId,
    control: TabId,
    table: String,
    _dir: tempfile::TempDir,
    _fixture: tokio::sync::MutexGuard<'static, ()>,
}

impl Harness {
    async fn open() -> Self {
        let fixture = FIXTURES.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::open(EngineConfig {
            data_dir: dir.path().into(), keys: Arc::new(MemoryKeyStore::default()), agent_path: AgentPath::Fixed("".into()), az_binary: None, user_known_hosts: None,
            updates: Arc::new(engine::ScriptedTransport::named("off").unwrap()),
        }).await.unwrap();
        let connection = engine.save_connection(ConnectionDraft {
            id: None, database_kind: DatabaseKind::SqlServer, name: "Native count regression".into(),
            host: std::env::var("L1_DATA_TEST_MSSQL_HOST").unwrap_or_else(|_| "localhost".into()),
            port: std::env::var("L1_DATA_TEST_MSSQL_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(1434),
            database: "shop".into(), user: "sa".into(),
            password: PasswordInput::Set(std::env::var("L1_DATA_TEST_MSSQL_PASSWORD").unwrap_or_else(|_| "L1Data_dev_Str0ng".into())),
            ssl_mode: SslMode::Require, ca_cert_path: None, color: None, ssh: None, environment: None,
            sql_server_auth: None,
        }).await.unwrap();
        engine.connect(connection.id).await.unwrap();
        let h = Self {
            engine, db: DatabaseRef { connection_id: connection.id, database: "shop".into() },
            tab: TabId(uuid::Uuid::new_v4()), control: TabId(uuid::Uuid::new_v4()),
            table: format!("l1_native_count_{}", uuid::Uuid::new_v4().simple()), _dir: dir, _fixture: fixture,
        };
        for tab in [h.tab, h.control] { h.engine.console_configure(tab, h.config("shop")).await.unwrap(); }
        let setup = AssertUnwindSafe(async {
            h.sql(h.control, &format!("CREATE TABLE dbo.[{}] (id int PRIMARY KEY, d date); INSERT dbo.[{}] VALUES (1,'20240101'),(2,'20240102'),(3,'20240201')", h.table, h.table)).await;
            h.engine.refresh_catalog(h.db.clone()).await.unwrap();
        }).catch_unwind().await;
        if let Err(panic) = setup {
            h.cleanup(Err(panic)).await;
            unreachable!("cleanup resumes the setup panic");
        }
        h
    }

    fn config(&self, database: &str) -> ConsoleConfig {
        ConsoleConfig { connection_id: self.db.connection_id, database: database.into(), schema: None, mode: TxnMode::AutoCommit }
    }

    fn state(&self, tab: TabId) -> ConsoleState {
        self.engine.boot().consoles.into_iter().find(|s| s.tab_id == tab).unwrap()
    }

    async fn run(&self, tab: TabId, request: RunRequest) -> ConsoleState {
        let accepted = self.engine.console_run(tab, request, None).await.unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let state = self.state(tab);
                if matches!(state.run, RunState::Finished { run_id, .. } if run_id == accepted.run_id) {
                    assert!(matches!(state.run, RunState::Finished { outcome: RunOutcome::Ok, .. }), "{:?}", state.run);
                    return state;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.expect("count fixture run timed out")
    }

    async fn sql(&self, tab: TabId, sql: &str) -> ConsoleState {
        self.run(tab, RunRequest::Script { text: sql.into(), target: RunTarget::All, mode: RunMode::Run, params: Vec::new(), cells: Vec::new() }).await
    }

    fn rows(&self, state: &ConsoleState) -> Vec<Vec<Option<String>>> {
        let RunState::Finished { statements, .. } = &state.run else { panic!("unfinished run") };
        statements.iter().flat_map(|s| &s.outputs).filter_map(|o| match o {
            RunOutput::Rows { result, .. } => Some(self.engine.result_page(result.r#ref, 0, 20).unwrap().rows),
            _ => None,
        }).flatten().collect()
    }

    async fn count(&self, query: TableQuery) -> Result<u64, CountError> {
        self.engine.exact_row_count(self.tab, self.db.clone(), "dbo".into(), self.table.clone(), query).await
    }

    async fn blocked_count(&self) -> tokio::task::JoinHandle<Result<u64, CountError>> {
        let before = self.sql(self.tab, "SELECT 42 AS original_result").await;
        self.sql(self.control, &format!("BEGIN TRANSACTION; SELECT * FROM dbo.[{}] WITH (TABLOCKX,HOLDLOCK)", self.table)).await;
        let rev = self.state(self.tab).rev;
        let (engine, tab, db, table) = (self.engine.clone(), self.tab, self.db.clone(), self.table.clone());
        let count = tokio::spawn(async move { engine.exact_row_count(tab, db, "dbo".into(), table, TableQuery::default()).await });
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                assert!(!count.is_finished(), "count did not acquire the tab's session");
                if self.state(self.tab).rev > rev { break; }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.expect("count did not become busy");
        let during = self.state(self.tab);
        assert_eq!(during.work, Some(ConsoleWork::Count));
        assert_eq!(serde_json::to_value(&during.run).unwrap(), serde_json::to_value(&before.run).unwrap());
        assert_eq!(self.rows(&during), self.rows(&before));
        count
    }

    async fn cleanup(self, result: std::thread::Result<()>) {
        self.engine.console_cancel(self.tab).await;
        self.engine.console_close(self.tab, true).await.unwrap();
        self.engine.connect(self.db.connection_id).await.unwrap();
        let state = self.sql(self.control, &format!("IF @@TRANCOUNT > 0 ROLLBACK; DROP TABLE IF EXISTS dbo.[{}]; SELECT OBJECT_ID(N'dbo.{}')", self.table, self.table)).await;
        assert_eq!(self.rows(&state), vec![vec![None]], "disposable count table remains");
        println!("cleanup {} absent", self.table);
        self.engine.console_close(self.control, true).await.unwrap();
        self.engine.disconnect(self.db.connection_id, true).await.unwrap();
        self.engine.shutdown().await;
        if let Err(panic) = result { std::panic::resume_unwind(panic); }
    }
}

#[tokio::test]
async fn native_settings_and_uncommitted_rows_match_pages_without_replacing_results() {
    let h = Harness::open().await;
    let result = AssertUnwindSafe(async {
        for (settings, predicate, expected) in [
            ("SET DATEFIRST 1", "DATEPART(weekday,[d]) = 1", "1"),
            ("SET LANGUAGE German", "DATENAME(weekday,[d]) = N'Montag'", "1"),
            ("SET DATEFORMAT dmy", "[d] = '01/02/2024'", "3"),
        ] {
            h.sql(h.tab, settings).await;
            let query = TableQuery { r#where: predicate.into(), ..Default::default() };
            let before = h.run(h.tab, RunRequest::TablePage { schema: "dbo".into(), table: h.table.clone(), page: 0, query: query.clone() }).await;
            assert_eq!(h.rows(&before).len(), 1);
            assert_eq!(h.rows(&before)[0][0].as_deref(), Some(expected));
            let history = h.engine.history(HistoryQuery { text: None, connection_id: Some(h.db.connection_id), before_id: None, limit: 100 }).entries;
            assert_eq!(h.count(query).await.unwrap(), 1, "{settings} {predicate}");
            let after = h.state(h.tab);
            assert_eq!(after.work, None);
            assert!(after.rev > before.rev);
            assert_eq!(serde_json::to_value((&after.run, &after.session, after.txn)).unwrap(), serde_json::to_value((&before.run, &before.session, before.txn)).unwrap());
            assert_eq!(h.rows(&after), h.rows(&before));
            assert_eq!(serde_json::to_value(h.engine.history(HistoryQuery { text: None, connection_id: Some(h.db.connection_id), before_id: None, limit: 100 }).entries).unwrap(), serde_json::to_value(history).unwrap());
            println!("{settings}: page=1 count=1, original run/results/history preserved");
        }
        h.sql(h.tab, &format!("BEGIN TRANSACTION; INSERT dbo.[{}] VALUES (4,'20240205')", h.table)).await;
        let query = TableQuery { filters: vec![ColumnFilter { column: "id".into(), op: FilterOp::Eq, value: "4".into() }], ..Default::default() };
        let before = h.run(h.tab, RunRequest::TablePage { schema: "dbo".into(), table: h.table.clone(), page: 0, query: query.clone() }).await;
        assert_eq!(before.txn, TxnState::Open);
        assert_eq!(h.rows(&before).len(), 1);
        assert_eq!(h.count(query).await.unwrap(), 1);
        let after = h.state(h.tab);
        assert_eq!(after.work, None);
        assert_eq!(serde_json::to_value((&after.run, &after.session, after.txn, after.txn_depth)).unwrap(), serde_json::to_value((&before.run, &before.session, TxnState::Open, Option::<u32>::None)).unwrap());
        assert!(matches!(h.engine.console_close(h.tab, false).await, Err(DisconnectError::OpenTransactions { .. })));
        assert!(matches!(h.engine.disconnect(h.db.connection_id, false).await, Err(DisconnectError::OpenTransactions { .. })));
        h.engine.console_rollback(h.tab).await.unwrap();
        assert_eq!(h.count(TableQuery::default()).await.unwrap(), 3, "count must not commit the native insert");
        println!("uncommitted native insert: page=1 count=1, transaction stays Open and rollback removes row");
        let before = h.sql(h.tab, "SET IMPLICIT_TRANSACTIONS ON").await;
        assert_eq!(h.count(TableQuery::default()).await.unwrap(), 3);
        let after = h.state(h.tab);
        assert_eq!(after.work, None);
        assert_eq!(after.txn, TxnState::Open);
        assert_eq!(after.txn_depth, None);
        assert_eq!(serde_json::to_value(after.run).unwrap(), serde_json::to_value(before.run).unwrap());
        h.engine.console_rollback(h.tab).await.unwrap();
        h.sql(h.tab, "SET IMPLICIT_TRANSACTIONS OFF").await;
    }).catch_unwind().await;
    h.cleanup(result).await;
}

#[tokio::test]
async fn a_native_count_error_settles_refreshes_when_the_server_ends_the_transaction() {
    let h = Harness::open().await;
    let result = AssertUnwindSafe(async {
        let version = h.engine.catalog(h.db.clone()).await.unwrap().version;
        let before = h.sql(h.tab, &format!("SET XACT_ABORT ON; BEGIN TRANSACTION; ALTER TABLE dbo.[{}] ADD count_pending int NULL", h.table)).await;
        assert_eq!(before.txn, TxnState::Open);
        assert_eq!(h.count(TableQuery::default()).await.unwrap(), 3);
        let query = TableQuery { r#where: "1 / ([id] - 1) = 0".into(), ..Default::default() };
        assert!(matches!(h.count(query).await, Err(CountError::Failed { message }) if message.contains("Divide by zero")));
        let after = h.state(h.tab);
        assert_eq!(after.work, None);
        assert!(after.rev > before.rev);
        assert_eq!(after.txn, TxnState::Idle);
        assert_eq!(serde_json::to_value(after.run).unwrap(), serde_json::to_value(before.run).unwrap());
        let catalog = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let catalog = h.engine.catalog(h.db.clone()).await.unwrap();
                if catalog.version > version { break catalog; }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.expect("count did not settle the native DDL refresh");
        assert!(catalog.relation("dbo", &h.table).unwrap().columns.iter().all(|column| column.name != "count_pending"));
        assert_eq!(h.count(TableQuery::default()).await.unwrap(), 3);
    }).catch_unwind().await;
    h.cleanup(result).await;
}

#[tokio::test]
async fn lazy_count_validates_expected_target_and_recovers_from_query_errors() {
    let h = Harness::open().await;
    let result = AssertUnwindSafe(async {
        assert!(matches!(h.state(h.tab).session, SessionState::None));
        assert_eq!(h.count(TableQuery::default()).await.unwrap(), 3);
        let before = h.state(h.tab);
        let bad = TableQuery { r#where: "missing_count_column = 1".into(), ..Default::default() };
        assert!(matches!(h.count(bad).await, Err(CountError::Failed { message }) if message.contains("Invalid column")));
        assert_eq!(h.state(h.tab).work, None);
        assert_eq!(h.count(TableQuery::default()).await.unwrap(), 3);
        assert_eq!(serde_json::to_value(h.state(h.tab).session).unwrap(), serde_json::to_value(before.session).unwrap());
        h.engine.console_configure(h.tab, h.config("reporting")).await.unwrap();
        assert!(matches!(h.count(TableQuery::default()).await, Err(CountError::Failed { message }) if message.contains("target changed")));
        assert_eq!(h.state(h.tab).config.database, "reporting");
        h.engine.console_close(h.tab, true).await.unwrap();
        assert!(matches!(h.count(TableQuery::default()).await, Err(CountError::Failed { .. })));
        h.engine.disconnect(h.db.connection_id, true).await.unwrap();
        assert!(matches!(h.count(TableQuery::default()).await, Err(CountError::NotConnected)));
    }).catch_unwind().await;
    h.cleanup(result).await;
}

#[tokio::test]
async fn busy_count_refuses_competitors_and_cancel_preserves_original_results() {
    let h = Harness::open().await;
    let result = AssertUnwindSafe(async {
        h.sql(h.tab, "BEGIN TRANSACTION").await;
        let count = h.blocked_count().await;
        let before = h.state(h.tab);
        assert_eq!(before.txn, TxnState::Open);
        assert!(matches!(h.count(TableQuery::default()).await, Err(CountError::Failed { message }) if message.contains("busy")));
        assert!(matches!(h.engine.console_run(h.tab, RunRequest::Script { text: "SELECT 99".into(), target: RunTarget::All, mode: RunMode::Run, params: Vec::new(), cells: Vec::new() }, None).await, Err(RunError::Busy)));
        assert!(matches!(h.engine.console_commit(h.tab, None).await, Err(TxnError::Busy)));
        assert!(matches!(h.engine.console_rollback(h.tab).await, Err(TxnError::Busy)));
        assert!(matches!(h.engine.console_configure(h.tab, h.config("reporting")).await, Err(ConfigureError::Busy)));
        h.engine.console_cancel(h.tab).await;
        assert!(tokio::time::timeout(Duration::from_secs(5), count).await.unwrap().unwrap().is_err());
        let after = h.state(h.tab);
        assert_eq!(after.work, None);
        assert!(matches!(after.session, SessionState::Lost { transaction: TransactionLoss::Unknown, .. }));
        assert!(after.rev > before.rev);
        assert_eq!(after.txn, TxnState::Idle);
        assert_eq!(serde_json::to_value(&after.run).unwrap(), serde_json::to_value(&before.run).unwrap());
        assert_eq!(h.rows(&after), vec![vec![Some("42".into())]]);
        h.engine.console_rollback(h.control).await.unwrap();
        assert_eq!(h.count(TableQuery::default()).await.unwrap(), 3);
    }).catch_unwind().await;
    h.cleanup(result).await;
}

#[tokio::test]
async fn close_and_disconnect_orphan_a_count_without_stealing_the_replacement_session() {
    for close in [true, false] {
        let h = Harness::open().await;
        let result = AssertUnwindSafe(async {
            let count = h.blocked_count().await;
            if close {
                h.engine.console_close(h.tab, true).await.unwrap();
                assert!(h.engine.boot().consoles.iter().all(|state| state.tab_id != h.tab));
            } else {
                h.engine.disconnect(h.db.connection_id, true).await.unwrap();
                assert!(matches!(h.state(h.tab).session, SessionState::None));
                assert_eq!(h.state(h.tab).work, None);
                h.engine.connect(h.db.connection_id).await.unwrap();
            }
            h.engine.console_configure(h.tab, h.config("reporting")).await.unwrap();
            let replacement = h.sql(h.tab, "SELECT 99 AS replacement").await;
            assert!(tokio::time::timeout(Duration::from_secs(5), count).await.unwrap().unwrap().is_err());
            let after = h.state(h.tab);
            assert_eq!(after.work, None);
            assert_eq!(serde_json::to_value((&after.run, &after.session, &after.config)).unwrap(), serde_json::to_value((&replacement.run, &replacement.session, &replacement.config)).unwrap());
            assert_eq!(h.rows(&after), vec![vec![Some("99".into())]]);
        }).catch_unwind().await;
        h.cleanup(result).await;
    }
}

#[tokio::test]
async fn abandoned_count_retires_its_lease_and_clears_busy() {
    let h = Harness::open().await;
    let result = AssertUnwindSafe(async {
        let count = h.blocked_count().await;
        let before = h.state(h.tab);
        count.abort();
        assert!(count.await.unwrap_err().is_cancelled());
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if matches!(h.state(h.tab).session, SessionState::Lost { .. }) { break; }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.expect("abandoned count left its session busy");
        assert_eq!(h.state(h.tab).work, None);
        assert!(h.state(h.tab).rev > before.rev);
        assert_eq!(serde_json::to_value(h.state(h.tab).run).unwrap(), serde_json::to_value(&before.run).unwrap());
        assert_eq!(h.rows(&h.state(h.tab)), vec![vec![Some("42".into())]]);
        h.engine.console_rollback(h.control).await.unwrap();
        assert_eq!(h.count(TableQuery::default()).await.unwrap(), 3);
    }).catch_unwind().await;
    h.cleanup(result).await;
}
