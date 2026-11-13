#![cfg(any(feature = "mssql-tests", feature = "pg-tests"))]

use engine::model::*;
use engine::{AgentPath, Engine, EngineConfig, MemoryKeyStore, draft_from_url};
use futures_util::{FutureExt, future::BoxFuture};
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

struct Harness {
    engine: Arc<Engine>,
    kind: DatabaseKind,
    connection: ConnectionId,
    tab: TabId,
    observer: TabId,
    other_database: TabId,
    tables: Vec<String>,
    databases: Vec<String>,
    _dir: tempfile::TempDir,
}

impl Harness {
    async fn open(kind: DatabaseKind) -> Self {
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
        let draft = match kind {
            DatabaseKind::Postgres => draft_from_url(
                &std::env::var("L1_DATA_TEST_PG")
                    .unwrap_or_else(|_| "postgres://l1data:l1data@localhost:5433/shop".into()),
                "PostgreSQL refresh outcomes",
            )
            .unwrap(),
            DatabaseKind::SqlServer => ConnectionDraft {
                database_kind: kind,
                id: None,
                name: "SQL Server refresh outcomes".into(),
                host: std::env::var("L1_DATA_TEST_MSSQL_HOST").unwrap_or_else(|_| "localhost".into()),
                port: std::env::var("L1_DATA_TEST_MSSQL_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(1434),
                database: "shop".into(),
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
            },
        };
        assert_eq!(draft.database, "shop", "Refresh regressions only create disposable objects in shop");
        let connection = engine.save_connection(draft).await.unwrap().id;
        engine.connect(connection).await.unwrap();
        let h = Self {
            engine,
            kind,
            connection,
            tab: TabId(uuid::Uuid::new_v4()),
            observer: TabId(uuid::Uuid::new_v4()),
            other_database: TabId(uuid::Uuid::new_v4()),
            tables: Vec::new(),
            databases: Vec::new(),
            _dir: dir,
        };
        h.configure(h.tab, "shop", if kind == DatabaseKind::Postgres { TxnMode::Manual } else { TxnMode::AutoCommit })
            .await;
        h.configure(h.observer, "shop", TxnMode::AutoCommit).await;
        h.configure(h.other_database, "reporting", TxnMode::AutoCommit).await;
        let guard = match kind {
            DatabaseKind::Postgres => "SELECT current_database()",
            DatabaseKind::SqlServer => "SELECT DB_NAME()",
        };
        assert_eq!(h.scalar(guard).await, "shop");
        h.engine.catalog(h.db("shop")).await.unwrap();
        h.engine.catalog(h.db("reporting")).await.unwrap();
        h
    }

    async fn configure(&self, tab: TabId, database: &str, mode: TxnMode) {
        self.engine
            .console_configure(
                tab,
                ConsoleConfig { connection_id: self.connection, database: database.into(), schema: None, mode },
            )
            .await
            .unwrap();
    }

    fn db(&self, database: &str) -> DatabaseRef {
        DatabaseRef { connection_id: self.connection, database: database.into() }
    }

    fn table(&mut self) -> String {
        let name = format!("l1_refresh_end_{}", uuid::Uuid::new_v4().simple());
        self.tables.push(name.clone());
        name
    }

    fn qualified(&self, table: &str) -> String {
        match self.kind {
            DatabaseKind::Postgres => format!("public.\"{table}\""),
            DatabaseKind::SqlServer => format!("dbo.[{table}]"),
        }
    }

    fn state(&self, tab: TabId) -> ConsoleState {
        self.engine.boot().consoles.into_iter().find(|state| state.tab_id == tab).unwrap()
    }

    async fn finish(&self, tab: TabId, run: RunId) -> ConsoleState {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let state = self.state(tab);
                if matches!(state.run, RunState::Finished { run_id, .. } if run_id == run) {
                    return state;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("Refresh outcome run did not finish")
    }

    async fn run(&self, tab: TabId, sql: &str) -> ConsoleState {
        let accepted = self
            .engine
            .console_run(
                tab,
                RunRequest::Script {
                    text: sql.into(),
                    target: RunTarget::All,
                    mode: RunMode::Run,
                    params: Vec::new(),
                    cells: Vec::new(),
                },
                None,
            )
            .await
            .unwrap();
        self.finish(tab, accepted.run_id).await
    }

    async fn ok(&self, tab: TabId, sql: &str) -> ConsoleState {
        let state = self.run(tab, sql).await;
        assert!(matches!(state.run, RunState::Finished { outcome: RunOutcome::Ok, .. }), "{sql}: {:?}", state.run);
        state
    }

    async fn scalar(&self, sql: &str) -> String {
        let state = self.ok(self.observer, sql).await;
        let RunState::Finished { statements, .. } = state.run else { unreachable!() };
        let result = statements
            .iter()
            .flat_map(|s| &s.outputs)
            .find_map(|o| match o {
                RunOutput::Rows { result, .. } => Some(result.r#ref),
                _ => None,
            })
            .expect("Observer query returned no rows");
        self.engine.result_page(result, 0, 1).unwrap().rows[0][0].clone().unwrap()
    }

    async fn table_exists(&self, table: &str) -> bool {
        let sql = match self.kind {
            DatabaseKind::Postgres => format!(
                "SELECT count(*) FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname='public' AND c.relname='{table}'"
            ),
            DatabaseKind::SqlServer => {
                format!("SELECT COUNT_BIG(*) FROM sys.tables WHERE schema_id=SCHEMA_ID(N'dbo') AND name=N'{table}'")
            }
        };
        self.scalar(&sql).await == "1"
    }

    async fn catalog_after(&self, database: &str, before: u64) -> Arc<Catalog> {
        tokio::time::timeout(Duration::from_secs(4), async {
            loop {
                let catalog = self.engine.catalog(self.db(database)).await.unwrap();
                assert_eq!(catalog.db, self.db(database));
                if catalog.version > before {
                    return catalog;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{database} catalog did not refresh after version {before}"))
    }

    #[cfg(feature = "mssql-tests")]
    async fn refreshed(&self, before: u64, table: &str) -> Arc<Catalog> {
        let result = tokio::time::timeout(Duration::from_secs(4), async {
            loop {
                let catalog = self.engine.catalog(self.db("shop")).await.unwrap();
                if catalog.version > before && catalog.relation("dbo", table).is_some() {
                    return catalog;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        let catalog = self.engine.catalog(self.db("shop")).await.unwrap();
        assert!(
            result.is_ok(),
            "Committed table {table} exists={}, but catalog stayed at version {} (before {before}), cached={}",
            self.table_exists(table).await,
            catalog.version,
            catalog.relation("dbo", table).is_some()
        );
        let catalog = result.unwrap();
        assert!(self.table_exists(table).await);
        eprintln!("{table}: observer exists=true, catalog {before} -> {}, cached=true", catalog.version);
        catalog
    }

    #[cfg(feature = "mssql-tests")]
    fn live_databases(&self) -> Vec<DatabaseState> {
        let live = self.engine.boot().live.into_iter().find(|s| s.id == self.connection).unwrap();
        let ConnStatus::Connected { instance, .. } = live.status else { panic!("Expected a connected instance") };
        instance.databases
    }

    /// A database only this test writes to, so a lock it holds cannot stall another test's catalog.
    #[cfg(feature = "mssql-tests")]
    async fn disposable_database(&mut self) -> String {
        let database = format!("l1_refresh_db_{}", uuid::Uuid::new_v4().simple());
        self.databases.push(database.clone());
        self.ok(self.observer, &format!("CREATE DATABASE [{database}]")).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            while !self.live_databases().iter().any(|d| d.name == database) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("The disposable database never reached the database list");
        database
    }

    /// Reads the connection snapshot, as the UI does, so a Failed refresh is not reloaded here.
    #[cfg(feature = "mssql-tests")]
    async fn ready_after(&self, database: &str, before: u64) -> Arc<Catalog> {
        let status = tokio::time::timeout(Duration::from_secs(4), async {
            loop {
                let status = self.live_databases().into_iter().find(|d| d.name == database).unwrap().catalog;
                if matches!(status, CatalogStatus::Ready { version } if version > before) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        let last = self.live_databases().into_iter().find(|d| d.name == database).unwrap().catalog;
        assert!(status.is_ok(), "{database} catalog stayed at {last:?} after version {before}");
        self.engine.catalog(self.db(database)).await.unwrap()
    }

    #[cfg(feature = "mssql-tests")]
    async fn waiting(&self, pid: i32) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if self
                    .scalar(&format!(
                        "SELECT COUNT_BIG(*) FROM sys.dm_exec_requests WHERE session_id={pid} AND command='WAITFOR'"
                    ))
                    .await
                    == "1"
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("Native batch did not reach WAITFOR");
    }

    async fn cleanup(self) {
        for state in self.engine.boot().consoles {
            if state.tab_id != self.observer && state.tab_id != self.other_database {
                self.engine.console_close(state.tab_id, true).await.unwrap();
            }
        }
        for database in &self.databases {
            self.ok(self.observer, &format!("IF DB_ID(N'{database}') IS NOT NULL BEGIN ALTER DATABASE [{database}] SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE [{database}]; END")).await;
            assert_eq!(
                self.scalar(&format!("SELECT COUNT_BIG(*) FROM sys.databases WHERE name=N'{database}'")).await,
                "0",
                "Disposable database {database} survived cleanup"
            );
        }
        for table in &self.tables {
            self.ok(self.observer, &format!("DROP TABLE IF EXISTS {}", self.qualified(table))).await;
            assert!(!self.table_exists(table).await, "Disposable table {table} survived cleanup");
        }
        self.engine.refresh_catalog(self.db("shop")).await.unwrap();
        let catalog = self.engine.catalog(self.db("shop")).await.unwrap();
        for table in &self.tables {
            assert!(
                catalog.schemas.iter().all(|schema| schema.relations.iter().all(|r| r.name != *table)),
                "Disposable table {table} remains cached"
            );
        }
        for tab in [self.observer, self.other_database] {
            self.engine.console_close(tab, true).await.unwrap();
        }
        assert!(self.engine.boot().consoles.is_empty());
        self.engine.disconnect(self.connection, true).await.unwrap();
        self.engine.shutdown().await;
    }
}

async fn check(kind: DatabaseKind, test: impl for<'a> FnOnce(&'a mut Harness) -> BoxFuture<'a, ()>) {
    let mut h = Harness::open(kind).await;
    let result = AssertUnwindSafe(test(&mut h)).catch_unwind().await;
    h.cleanup().await;
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

#[cfg(feature = "mssql-tests")]
#[tokio::test]
async fn rollback_button_refreshes_ddl_committed_before_native_begin() {
    check(DatabaseKind::SqlServer, |h| {
        async move {
            let table = h.table();
            let before = h.engine.catalog(h.db("shop")).await.unwrap().version;
            let other = h.engine.catalog(h.db("reporting")).await.unwrap().version;
            let state = h.ok(h.tab, &format!("CREATE TABLE {} (id int); BEGIN TRANSACTION", h.qualified(&table))).await;
            assert_ne!(state.txn, TxnState::Idle);
            assert!(h.table_exists(&table).await, "DDL before BEGIN must already be committed");
            assert_eq!(h.engine.catalog(h.db("shop")).await.unwrap().version, before);
            h.engine.console_rollback(h.tab).await.unwrap();
            assert_eq!(h.state(h.tab).txn, TxnState::Idle);
            h.configure(h.tab, "reporting", TxnMode::AutoCommit).await;
            h.refreshed(before, &table).await;
            h.catalog_after("reporting", other).await;
        }
        .boxed()
    })
    .await;
}

#[cfg(feature = "mssql-tests")]
#[tokio::test]
async fn cancelled_native_batch_refreshes_its_committed_ddl() {
    check(DatabaseKind::SqlServer, |h| {
        async move {
            let table = h.table();
            let before = h.engine.catalog(h.db("shop")).await.unwrap().version;
            let state = h.ok(h.tab, "SELECT 1").await;
            let SessionState::Open { backend_pid } = state.session else { panic!("Expected native session") };
            let text =
                format!("CREATE TABLE {} (id int); BEGIN TRANSACTION; WAITFOR DELAY '00:00:30'", h.qualified(&table));
            let run = h
                .engine
                .console_run(
                    h.tab,
                    RunRequest::Script {
                        text,
                        target: RunTarget::All,
                        mode: RunMode::Run,
                        params: Vec::new(),
                        cells: Vec::new(),
                    },
                    None,
                )
                .await
                .unwrap()
                .run_id;
            h.waiting(backend_pid).await;
            assert!(h.table_exists(&table).await);
            h.engine.console_cancel(h.tab).await;
            let state = h.finish(h.tab, run).await;
            assert!(matches!(state.run, RunState::Finished { outcome: RunOutcome::Cancelled, .. }));
            assert!(matches!(state.session, SessionState::Lost { transaction: TransactionLoss::Unknown, .. }));
            h.refreshed(before, &table).await;
        }
        .boxed()
    })
    .await;
}

#[cfg(feature = "mssql-tests")]
#[tokio::test]
async fn lost_native_session_refreshes_ddl_from_an_earlier_batch() {
    check(DatabaseKind::SqlServer, |h| {
        async move {
            let table = h.table();
            let before = h.engine.catalog(h.db("shop")).await.unwrap().version;
            let state = h.ok(h.tab, &format!("CREATE TABLE {} (id int); BEGIN TRANSACTION", h.qualified(&table))).await;
            let SessionState::Open { backend_pid } = state.session else { panic!("Expected native session") };
            h.ok(h.observer, &format!("KILL {backend_pid}")).await;
            let state = h.run(h.tab, "SELECT 1").await;
            assert!(
                matches!(state.session, SessionState::Lost { transaction: TransactionLoss::Unknown, .. }),
                "{:?}",
                state.session
            );
            h.refreshed(before, &table).await;
        }
        .boxed()
    })
    .await;
}

#[cfg(feature = "mssql-tests")]
#[tokio::test]
async fn force_closing_native_transaction_refreshes_committed_ddl() {
    check(DatabaseKind::SqlServer, |h| {
        async move {
            let table = h.table();
            let before = h.engine.catalog(h.db("shop")).await.unwrap().version;
            let state = h.ok(h.tab, &format!("CREATE TABLE {} (id int); BEGIN TRANSACTION", h.qualified(&table))).await;
            assert_ne!(state.txn, TxnState::Idle);
            h.engine.console_close(h.tab, true).await.unwrap();
            h.refreshed(before, &table).await;
        }
        .boxed()
    })
    .await;
}

#[cfg(feature = "mssql-tests")]
#[tokio::test]
async fn force_closing_running_native_batch_refreshes_its_committed_ddl() {
    check(DatabaseKind::SqlServer, |h| {
        async move {
            let table = h.table();
            let before = h.engine.catalog(h.db("shop")).await.unwrap().version;
            let state = h.ok(h.tab, "SELECT 1").await;
            let SessionState::Open { backend_pid } = state.session else { panic!("Expected native session") };
            let text =
                format!("CREATE TABLE {} (id int); BEGIN TRANSACTION; WAITFOR DELAY '00:00:30'", h.qualified(&table));
            h.engine
                .console_run(
                    h.tab,
                    RunRequest::Script {
                        text,
                        target: RunTarget::All,
                        mode: RunMode::Run,
                        params: Vec::new(),
                        cells: Vec::new(),
                    },
                    None,
                )
                .await
                .unwrap();
            h.waiting(backend_pid).await;
            assert!(h.table_exists(&table).await);
            h.engine.console_close(h.tab, true).await.unwrap();
            h.refreshed(before, &table).await;
        }
        .boxed()
    })
    .await;
}

#[cfg(feature = "mssql-tests")]
#[tokio::test]
async fn refresh_that_loses_a_deadlock_to_another_users_ddl_still_converges() {
    check(DatabaseKind::SqlServer, |h| async move {
        let database = h.disposable_database().await;
        let writer = TabId(uuid::Uuid::new_v4());
        h.configure(writer, &database, TxnMode::AutoCommit).await;
        let before = h.engine.catalog(h.db(&database)).await.unwrap().version;
        h.ok(writer, "CREATE TABLE dbo.dropped (id int)").await;
        let shown = h.ready_after(&database, before).await.version;
        let other = Harness::open(DatabaseKind::SqlServer).await;
        other.configure(other.tab, &database, TxnMode::AutoCommit).await;
        let state = other.ok(other.tab, "BEGIN TRANSACTION; TRUNCATE TABLE dbo.dropped").await;
        let SessionState::Open { backend_pid } = state.session else { panic!("Expected native session") };
        h.ok(writer, "CREATE TABLE dbo.created (id int)").await;
        tokio::time::timeout(Duration::from_secs(5), async {
            while h.scalar(&format!("SELECT COUNT_BIG(*) FROM sys.dm_exec_requests WHERE blocking_session_id={backend_pid} AND wait_type='LCK_M_SCH_S'")).await == "0" {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.expect("Catalog refresh did not wait behind the other user's TRUNCATE");
        other.ok(other.tab, "DROP TABLE dbo.dropped").await;
        other.engine.console_commit(other.tab, None).await.unwrap();
        other.cleanup().await;
        let catalog = h.ready_after(&database, shown).await;
        assert!(catalog.relation("dbo", "created").is_some() && catalog.relation("dbo", "dropped").is_none());
    }.boxed()).await;
}

#[cfg(feature = "mssql-tests")]
#[tokio::test]
async fn commit_and_typed_rollback_refresh_tracked_catalogs() {
    check(DatabaseKind::SqlServer, |h| {
        async move {
            for commit in [false, true] {
                let table = h.table();
                let before = h.engine.catalog(h.db("shop")).await.unwrap().version;
                let other = h.engine.catalog(h.db("reporting")).await.unwrap().version;
                h.ok(h.tab, &format!("CREATE TABLE {} (id int); BEGIN TRANSACTION", h.qualified(&table))).await;
                if commit {
                    h.engine.console_commit(h.tab, None).await.unwrap();
                } else {
                    h.ok(h.tab, "ROLLBACK TRANSACTION").await;
                }
                h.refreshed(before, &table).await;
                let other = h.catalog_after("reporting", other).await.version;
                h.ok(h.tab, "SELECT 1").await;
                let version = h.engine.catalog(h.db("shop")).await.unwrap().version;
                h.ok(h.tab, "SELECT 2").await;
                assert_eq!(
                    h.engine.catalog(h.db("shop")).await.unwrap().version,
                    version,
                    "Reads scheduled a catalog refresh"
                );
                assert_eq!(h.engine.catalog(h.db("reporting")).await.unwrap().version, other);
            }
        }
        .boxed()
    })
    .await;
}

#[cfg(feature = "pg-tests")]
#[tokio::test]
async fn postgres_commit_refreshes_only_its_database() {
    check(DatabaseKind::Postgres, |h| {
        async move {
            let table = h.table();
            let before = h.engine.catalog(h.db("shop")).await.unwrap().version;
            let other = h.engine.catalog(h.db("reporting")).await.unwrap().version;
            let state = h.ok(h.tab, &format!("CREATE TABLE {} (id int)", h.qualified(&table))).await;
            assert_eq!(state.txn, TxnState::Active);
            assert_eq!(h.engine.catalog(h.db("shop")).await.unwrap().version, before);
            assert!(!h.table_exists(&table).await);
            h.engine.console_commit(h.tab, None).await.unwrap();
            let catalog = h.catalog_after("shop", before).await;
            assert!(catalog.relation("public", &table).is_some());
            assert!(h.table_exists(&table).await);
            for _ in 0..10 {
                assert_eq!(
                    h.engine.catalog(h.db("reporting")).await.unwrap().version,
                    other,
                    "PostgreSQL refresh escaped its DatabaseRef"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            eprintln!("PostgreSQL commit: shop {before} -> {}, reporting unchanged at {other}", catalog.version);
        }
        .boxed()
    })
    .await;
}

#[cfg(feature = "pg-tests")]
#[tokio::test]
async fn postgres_rollback_and_retirement_do_not_publish_uncommitted_objects() {
    check(DatabaseKind::Postgres, |h| {
        async move {
            for finish in ["rollback", "typed rollback", "loss", "close"] {
                let table = h.table();
                let before = h.engine.catalog(h.db("shop")).await.unwrap().version;
                let state = h.ok(h.tab, &format!("CREATE TABLE {} (id int)", h.qualified(&table))).await;
                assert_eq!(state.txn, TxnState::Active);
                assert!(!h.table_exists(&table).await);
                match finish {
                    "rollback" => h.engine.console_rollback(h.tab).await.unwrap(),
                    "typed rollback" => {
                        h.ok(h.tab, "ROLLBACK").await;
                    }
                    "loss" => {
                        let SessionState::Open { backend_pid } = state.session else {
                            panic!("Expected PostgreSQL session")
                        };
                        assert_eq!(h.scalar(&format!("SELECT pg_terminate_backend({backend_pid})")).await, "t");
                        let state = h.run(h.tab, "SELECT 1").await;
                        assert!(
                            matches!(
                                state.session,
                                SessionState::Lost { transaction: TransactionLoss::RolledBack, .. }
                            ),
                            "{:?}",
                            state.session
                        );
                    }
                    "close" => h.engine.console_close(h.tab, true).await.unwrap(),
                    _ => unreachable!(),
                }
                assert!(!h.table_exists(&table).await);
                for _ in 0..10 {
                    let catalog = h.engine.catalog(h.db("shop")).await.unwrap();
                    assert_eq!(catalog.version, before, "PostgreSQL {finish} published a rolled-back invalidation");
                    assert!(catalog.relation("public", &table).is_none());
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                eprintln!("PostgreSQL {finish}: observer exists=false, catalog unchanged at {before}, cached=false");
            }
        }
        .boxed()
    })
    .await;
}
