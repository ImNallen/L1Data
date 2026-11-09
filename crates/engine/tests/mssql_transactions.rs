#![cfg(feature = "mssql-tests")]

use engine::model::*;
use engine::{AgentPath, Engine, EngineConfig, MemoryKeyStore};
use futures_util::{FutureExt, future::BoxFuture};
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

struct Harness {
    engine: Arc<Engine>,
    connection: ConnectionId,
    tab: TabId,
    observer: TabId,
    table: String,
    objects: Vec<(&'static str, String)>,
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
        }).await.unwrap();
        let connection = engine.save_connection(ConnectionDraft {
            database_kind: DatabaseKind::SqlServer,
            id: None,
            name: "Native transaction regression".into(),
            host: std::env::var("L1_DATA_TEST_MSSQL_HOST").unwrap_or_else(|_| "localhost".into()),
            port: std::env::var("L1_DATA_TEST_MSSQL_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(1434),
            database: "shop".into(),
            user: "sa".into(),
            password: PasswordInput::Set(std::env::var("L1_DATA_TEST_MSSQL_PASSWORD").unwrap_or_else(|_| "L1Data_dev_Str0ng".into())),
            ssl_mode: SslMode::Require,
            ca_cert_path: None,
            color: None,
            ssh: None,
            environment: None,
            sql_server_auth: None,
        }).await.unwrap().id;
        engine.connect(connection).await.unwrap();
        let tab = TabId(uuid::Uuid::new_v4());
        let observer = TabId(uuid::Uuid::new_v4());
        let h = Self {
            engine, connection, tab, observer,
            table: format!("l1_native_{}", uuid::Uuid::new_v4().simple()),
            objects: Vec::new(),
            _dir: dir,
        };
        h.configure(tab, TxnMode::AutoCommit).await;
        h.configure(observer, TxnMode::AutoCommit).await;
        h.ok(observer, &format!("CREATE TABLE [{}] (id int PRIMARY KEY, n int); INSERT INTO [{}] VALUES(1,1)", h.table, h.table)).await;
        h
    }

    async fn configure(&self, tab: TabId, mode: TxnMode) {
        self.engine.console_configure(tab, ConsoleConfig {
            connection_id: self.connection, database: "shop".into(), schema: None, mode,
        }).await.unwrap();
    }

    fn state(&self, tab: TabId) -> ConsoleState {
        self.engine.boot().consoles.into_iter().find(|s| s.tab_id == tab).unwrap()
    }

    async fn run(&self, tab: TabId, sql: &str) -> ConsoleState {
        let accepted = self.engine.console_run(tab, RunRequest::Script { text: sql.into(), target: RunTarget::All, mode: RunMode::Run, params: Vec::new(), cells: Vec::new() }, None).await.unwrap();
        self.finish(tab, accepted.run_id).await
    }

    async fn finish(&self, tab: TabId, run: RunId) -> ConsoleState {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let state = self.state(tab);
                if matches!(state.run, RunState::Finished { run_id, .. } if run_id == run) {
                    return state;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.expect("Native transaction regression did not finish")
    }

    async fn ok(&self, tab: TabId, sql: &str) -> ConsoleState {
        let state = self.run(tab, sql).await;
        assert!(matches!(state.run, RunState::Finished { outcome: RunOutcome::Ok, .. }), "{sql}: {:?}", state.run);
        state
    }

    async fn scalar(&self, tab: TabId, sql: &str) -> String {
        let state = self.ok(tab, sql).await;
        let RunState::Finished { statements, .. } = state.run else { unreachable!() };
        let result = statements.iter().flat_map(|s| &s.outputs).find_map(|o| match o {
            RunOutput::Rows { result, .. } => Some(result.r#ref),
            _ => None,
        }).unwrap();
        self.engine.result_page(result, 0, 1).unwrap().rows[0][0].clone().unwrap()
    }

    fn procedure(&mut self) -> String {
        self.object("PROCEDURE")
    }

    fn object(&mut self, kind: &'static str) -> String {
        let name = format!("l1_tab_{}", uuid::Uuid::new_v4().simple());
        self.objects.push((kind, name.clone()));
        name
    }

    async fn cleanup(self) {
        for tab in [self.tab, self.observer] {
            let _ = self.engine.console_rollback(tab).await;
            self.engine.console_close(tab, true).await.unwrap();
        }
        self.configure(self.observer, TxnMode::AutoCommit).await;
        for (kind, name) in self.objects.iter().rev() {
            self.ok(self.observer, &format!("DROP {kind} IF EXISTS dbo.[{name}]")).await;
            assert_eq!(self.scalar(self.observer, &format!("SELECT COUNT_BIG(*) FROM sys.objects WHERE name=N'{name}'")).await, "0");
        }
        self.ok(self.observer, &format!("DROP TABLE IF EXISTS [{}]", self.table)).await;
        assert_eq!(self.scalar(self.observer, &format!("SELECT COUNT_BIG(*) FROM sys.tables WHERE name=N'{}'", self.table)).await, "0");
        self.engine.shutdown().await;
    }
}

async fn check(test: impl for<'a> FnOnce(&'a mut Harness) -> BoxFuture<'a, ()>) {
    let mut h = Harness::open().await;
    let result = AssertUnwindSafe(test(&mut h)).catch_unwind().await;
    h.cleanup().await;
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

fn transaction(state: &ConsoleState, depth: u32) {
    assert_eq!((state.txn, state.txn_depth), if depth == 0 { (TxnState::Idle, Some(0)) } else { (TxnState::Open, None) });
}

fn changes(table: &str) -> TableChanges {
    TableChanges { schema: "dbo".into(), table: table.into(), inserts: vec![], updates: vec![], deletes: vec![] }
}

fn cell(column: &str, value: &str) -> CellWrite {
    CellWrite { column: column.into(), value: Some(value.into()) }
}

#[tokio::test]
async fn manual_configuration_and_staged_edits_refuse_before_trigger_execution() {
    check(|h| async move {
        let sequence = h.object("SEQUENCE");
        let audit = h.object("TABLE");
        let trigger = h.object("TRIGGER");
        h.ok(h.observer, &format!("CREATE SEQUENCE dbo.[{sequence}] AS bigint START WITH 1; CREATE TABLE dbo.[{audit}](n int); INSERT INTO dbo.[{audit}] VALUES(1)")).await;
        h.ok(h.observer, &format!("CREATE TRIGGER dbo.[{trigger}] ON dbo.[{}] AFTER UPDATE AS BEGIN SET NOCOUNT ON; DECLARE @attempt bigint = NEXT VALUE FOR dbo.[{sequence}]; WHILE @@TRANCOUNT > 0 COMMIT TRANSACTION; BEGIN TRANSACTION; UPDATE dbo.[{audit}] SET n=99; END", h.table)).await;
        assert_eq!(h.scalar(h.observer, &format!("SELECT NEXT VALUE FOR dbo.[{sequence}]")).await, "1");
        h.ok(h.tab, "SELECT @@TRANCOUNT").await;
        let before = serde_json::to_value(h.state(h.tab)).unwrap();
        let fresh = TabId(uuid::Uuid::new_v4());
        for tab in [fresh, h.tab] {
            let refused = h.engine.console_configure(tab, ConsoleConfig {
                connection_id: h.connection, database: "shop".into(), schema: Some("lab".into()), mode: TxnMode::Manual,
            }).await;
            assert!(matches!(refused, Err(ConfigureError::Unsupported { ref what }) if what.contains("Select Auto-commit")), "{refused:?}");
        }
        assert!(!h.engine.boot().consoles.iter().any(|state| state.tab_id == fresh));
        let mut edit = changes(&h.table);
        edit.updates.push(RowUpdate { key: vec![cell("id", "1")], set: vec![cell("n", "2")] });
        edit.inserts.push(vec![cell("id", "1"), cell("n", "3")]);
        let mut delete = changes(&h.table);
        delete.deletes.push(vec![cell("id", "1")]);
        for edit in [changes(&h.table), changes("not_a_table"), edit, delete] {
            let refused = h.engine.table_apply(h.tab, edit, None).await;
            assert!(matches!(refused, Err(ApplyError::Unsupported { ref what }) if what.contains("SQL editor") && what.contains("Auto-commit")), "{refused:?}");
            assert_eq!(serde_json::to_value(h.state(h.tab)).unwrap(), before);
        }
        assert_eq!(h.scalar(h.observer, &format!("SELECT n FROM dbo.[{}]", h.table)).await, "1");
        assert_eq!(h.scalar(h.observer, &format!("SELECT n FROM dbo.[{audit}]")).await, "1");
        assert_eq!(h.scalar(h.observer, &format!("SELECT NEXT VALUE FOR dbo.[{sequence}]")).await, "2", "A refused call reached the trigger");
    }.boxed()).await;
}

#[tokio::test]
async fn staged_refusal_preserves_xact_abort_and_prior_native_transaction_work() {
    check(|h| async move {
        h.ok(h.tab, "SET XACT_ABORT ON").await;
        transaction(&h.ok(h.tab, &format!("BEGIN TRANSACTION; BEGIN TRANSACTION; UPDATE dbo.[{}] SET n=3 WHERE id=1", h.table)).await, 2);
        assert_eq!(h.scalar(h.tab, "SELECT XACT_STATE()").await, "1");
        let before = serde_json::to_value(h.state(h.tab)).unwrap();
        let mut edit = changes(&h.table);
        edit.inserts.push(vec![cell("id", "1"), cell("n", "99")]);
        for edit in [edit, changes(&h.table)] {
            assert!(matches!(h.engine.table_apply(h.tab, edit, None).await, Err(ApplyError::Unsupported { .. })));
            assert_eq!(serde_json::to_value(h.state(h.tab)).unwrap(), before);
        }
        let refused = h.engine.console_configure(h.tab, ConsoleConfig {
            connection_id: h.connection, database: "shop".into(), schema: None, mode: TxnMode::Manual,
        }).await;
        assert!(matches!(refused, Err(ConfigureError::Unsupported { .. })));
        assert_eq!(serde_json::to_value(h.state(h.tab)).unwrap(), before);
        assert_eq!(h.scalar(h.tab, &format!("SELECT n FROM dbo.[{}] WHERE id=1", h.table)).await, "3");
        assert_eq!(h.scalar(h.tab, "SELECT @@TRANCOUNT").await, "2");
        assert_eq!(h.scalar(h.tab, "SELECT XACT_STATE()").await, "1");
        assert_eq!(h.scalar(h.tab, "SELECT CASE WHEN (@@OPTIONS & 16384)=16384 THEN 1 ELSE 0 END").await, "1");
        assert_eq!(h.scalar(h.observer, &format!("SELECT n FROM dbo.[{}] WITH (READUNCOMMITTED) WHERE id=1", h.table)).await, "3");
        h.engine.console_commit(h.tab, None).await.unwrap();
        transaction(&h.state(h.tab), 0);
        assert_eq!(h.scalar(h.observer, &format!("SELECT n FROM dbo.[{}] WHERE id=1", h.table)).await, "3");
        h.ok(h.tab, &format!("BEGIN TRANSACTION; UPDATE dbo.[{}] SET n=4 WHERE id=1", h.table)).await;
        h.engine.console_rollback(h.tab).await.unwrap();
        transaction(&h.state(h.tab), 0);
        assert_eq!(h.scalar(h.observer, &format!("SELECT n FROM dbo.[{}] WHERE id=1", h.table)).await, "3");
    }.boxed()).await;
}

#[tokio::test]
async fn native_explicit_transactions_keep_module_definitions_rollbackable() {
    check(|h| async move {
        for finish in [false, true] {
            let name = h.procedure();
            transaction(&h.ok(h.tab, "BEGIN TRANSACTION").await, 1);
            transaction(&h.ok(h.tab, &format!("CREATE PROCEDURE dbo.[{name}] AS BEGIN SELECT 1 AS n END")).await, 1);
            if finish {
                h.engine.console_commit(h.tab, None).await.unwrap();
            } else {
                h.engine.console_rollback(h.tab).await.unwrap();
            }
            transaction(&h.state(h.tab), 0);
            assert_eq!(h.scalar(h.observer, &format!("SELECT COUNT_BIG(*) FROM sys.procedures WHERE name=N'{name}'")).await, if finish { "1" } else { "0" });
        }
    }.boxed()).await;
}

#[tokio::test]
async fn native_errors_and_cancellation_preserve_observable_transaction_state() {
    check(|h| async move {
        let failed = h.run(h.tab, &format!("BEGIN TRANSACTION; UPDATE dbo.[{}] SET n=2; RAISERROR('expected failure',16,1)", h.table)).await;
        assert!(matches!(failed.run, RunState::Finished { outcome: RunOutcome::Failed, .. }));
        transaction(&failed, 1);
        h.engine.console_rollback(h.tab).await.unwrap();
        assert_eq!(h.scalar(h.observer, &format!("SELECT n FROM dbo.[{}]", h.table)).await, "1");
        h.ok(h.tab, &format!("BEGIN TRANSACTION; UPDATE dbo.[{}] SET n=3", h.table)).await;
        let SessionState::Open { backend_pid } = h.state(h.tab).session else { panic!("Expected native session") };
        let accepted = h.engine.console_run(h.tab, RunRequest::Script { text: "WAITFOR DELAY '00:00:30'".into(), target: RunTarget::All, mode: RunMode::Run, params: Vec::new(), cells: Vec::new() }, None).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if h.scalar(h.observer, &format!("SELECT COUNT_BIG(*) FROM sys.dm_exec_requests WHERE session_id={backend_pid} AND command='WAITFOR'")).await == "1" { break; }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.unwrap();
        h.engine.console_cancel(h.tab).await;
        let state = h.finish(h.tab, accepted.run_id).await;
        assert!(matches!(state.run, RunState::Finished { outcome: RunOutcome::Cancelled, .. }));
        assert!(matches!(state.session, SessionState::Lost { transaction: TransactionLoss::Unknown, .. }));
        assert_eq!(h.scalar(h.observer, &format!("SELECT n FROM dbo.[{}]", h.table)).await, "1");
        transaction(&h.ok(h.tab, "SELECT @@TRANCOUNT").await, 0);
    }.boxed()).await;
}

#[tokio::test]
async fn autocommit_retains_native_conditionals_options_and_procedure_calls() {
    check(|h| async move {
        h.configure(h.tab, TxnMode::AutoCommit).await;
        transaction(&h.ok(h.tab, &format!("IF 1=0 ROLLBACK TRANSACTION; UPDATE [{}] SET n=2", h.table)).await, 0);
        assert_eq!(h.scalar(h.observer, &format!("SELECT n FROM [{}]", h.table)).await, "2");
        transaction(&h.ok(h.tab, &format!("BEGIN TRANSACTION; BEGIN TRANSACTION; UPDATE [{}] SET n=3; COMMIT TRANSACTION", h.table)).await, 1);
        h.engine.console_rollback(h.tab).await.unwrap();
        assert_eq!(h.scalar(h.observer, &format!("SELECT n FROM [{}]", h.table)).await, "2");
        transaction(&h.ok(h.tab, &format!("IF 1=1 BEGIN TRANSACTION; UPDATE [{}] SET n=4; IF 1=1 COMMIT TRANSACTION", h.table)).await, 0);
        assert_eq!(h.scalar(h.observer, &format!("SELECT n FROM [{}]", h.table)).await, "4");
        transaction(&h.ok(h.tab, &format!("SET IMPLICIT_TRANSACTIONS ON; UPDATE [{}] SET n=5", h.table)).await, 1);
        h.engine.console_rollback(h.tab).await.unwrap();
        transaction(&h.state(h.tab), 0);
        assert_eq!(h.scalar(h.observer, &format!("SELECT n FROM [{}]", h.table)).await, "4");
        transaction(&h.ok(h.tab, "SELECT @@TRANCOUNT, XACT_STATE(), DB_NAME()").await, 0);
        transaction(&h.ok(h.tab, "SET IMPLICIT_TRANSACTIONS OFF").await, 0);

        let name = h.procedure();
        h.ok(h.observer, &format!("CREATE PROCEDURE dbo.[{name}] @n int AS UPDATE [{}] SET n=@n", h.table)).await;
        for call in [format!("EXEC dbo.[{name}] @n=6"), format!("dbo.[{name}] @n=6"), format!("{name} @n=6")] {
            transaction(&h.ok(h.tab, &call).await, 0);
            assert_eq!(h.scalar(h.observer, &format!("SELECT n FROM [{}]", h.table)).await, "6");
        }
    }.boxed()).await;
}

#[tokio::test]
async fn cancellation_does_not_claim_committed_work_was_rolled_back() {
    check(|h| async move {
        h.configure(h.tab, TxnMode::AutoCommit).await;
        for (sql, expected) in [
            (format!("UPDATE [{}] SET n=2\nGO\nWAITFOR DELAY '00:00:30'", h.table), "2"),
            (format!("BEGIN TRANSACTION; UPDATE [{}] SET n=3; COMMIT TRANSACTION; WAITFOR DELAY '00:00:30'", h.table), "3"),
        ] {
            let accepted = h.engine.console_run(h.tab, RunRequest::Script { text: sql, target: RunTarget::All, mode: RunMode::Run, params: Vec::new(), cells: Vec::new() }, None).await.unwrap();
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    let SessionState::Open { backend_pid } = h.state(h.tab).session else { continue };
                    if h.scalar(h.observer, &format!("SELECT COUNT_BIG(*) FROM sys.dm_exec_requests WHERE session_id={backend_pid} AND command='WAITFOR'")).await == "1" { break; }
                }
            }).await.unwrap();
            assert_eq!(h.scalar(h.observer, &format!("SELECT n FROM [{}]", h.table)).await, expected);
            h.engine.console_cancel(h.tab).await;
            let state = h.finish(h.tab, accepted.run_id).await;
            assert!(matches!(state.run, RunState::Finished { outcome: RunOutcome::Cancelled, .. }));
            assert!(matches!(state.session, SessionState::Lost { transaction: TransactionLoss::Unknown, .. }), "Cancellation must not claim a confirmed rollback: {:?}", state.session);
            assert_eq!(h.scalar(h.observer, &format!("SELECT n FROM [{}]", h.table)).await, expected);
        }
    }.boxed()).await;
}
