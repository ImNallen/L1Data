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
    other_connection: ConnectionId,
    tab: TabId,
    shop: TabId,
    reporting: TabId,
    other_shop: TabId,
    other_reporting: TabId,
    table: String,
    _dir: tempfile::TempDir,
}

#[derive(Debug, Clone, Copy)]
enum Execution {
    ThreePart,
    SpExecuteSql,
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
        let mut connections = Vec::new();
        for name in ["Cross-database refresh", "Independent catalog identity"] {
            let id = engine
                .save_connection(ConnectionDraft {
                    database_kind: DatabaseKind::SqlServer,
                    id: None,
                    name: name.into(),
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
                })
                .await
                .unwrap()
                .id;
            engine.connect(id).await.unwrap();
            connections.push(id);
        }
        let h = Self {
            engine,
            connection: connections[0],
            other_connection: connections[1],
            tab: TabId(uuid::Uuid::new_v4()),
            shop: TabId(uuid::Uuid::new_v4()),
            reporting: TabId(uuid::Uuid::new_v4()),
            other_shop: TabId(uuid::Uuid::new_v4()),
            other_reporting: TabId(uuid::Uuid::new_v4()),
            table: format!("l1_cross_refresh_{}", uuid::Uuid::new_v4().simple()),
            _dir: dir,
        };
        for (tab, connection, database) in [
            (h.tab, h.connection, "shop"),
            (h.shop, h.connection, "shop"),
            (h.reporting, h.connection, "reporting"),
            (h.other_shop, h.other_connection, "shop"),
            (h.other_reporting, h.other_connection, "reporting"),
        ] {
            h.configure(tab, connection, database).await;
            assert_eq!(h.scalar(tab, "SELECT DB_NAME()").await, database);
        }
        h.catalogs().await;
        h
    }

    async fn configure(&self, tab: TabId, connection_id: ConnectionId, database: &str) {
        self.engine
            .console_configure(
                tab,
                ConsoleConfig { connection_id, database: database.into(), schema: None, mode: TxnMode::AutoCommit },
            )
            .await
            .unwrap();
    }

    fn db(connection_id: ConnectionId, database: &str) -> DatabaseRef {
        DatabaseRef { connection_id, database: database.into() }
    }

    async fn catalogs(&self) -> [Arc<Catalog>; 4] {
        let mut catalogs = Vec::new();
        for connection in [self.connection, self.other_connection] {
            for database in ["shop", "reporting"] {
                let db = Self::db(connection, database);
                let catalog = self.engine.catalog(db.clone()).await.unwrap();
                assert_eq!(catalog.db, db);
                catalogs.push(catalog);
            }
        }
        catalogs.try_into().unwrap()
    }

    fn state(&self, tab: TabId) -> ConsoleState {
        self.engine.boot().consoles.into_iter().find(|s| s.tab_id == tab).unwrap()
    }

    async fn ok(&self, tab: TabId, sql: &str) -> ConsoleState {
        let run = self
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
            .unwrap()
            .run_id;
        let state = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let state = self.state(tab);
                if matches!(state.run, RunState::Finished { run_id, .. } if run_id == run) {
                    return state;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("Cross-database DDL run did not finish");
        assert!(matches!(state.run, RunState::Finished { outcome: RunOutcome::Ok, .. }), "{sql}: {:?}", state.run);
        state
    }

    async fn scalar(&self, tab: TabId, sql: &str) -> String {
        let state = self.ok(tab, sql).await;
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

    fn ddl(&self, execution: Execution, verb: &str, suffix: &str) -> String {
        match execution {
            Execution::ThreePart => format!("{verb} TABLE [reporting].[dbo].[{}] {suffix}", self.table),
            Execution::SpExecuteSql => {
                format!("EXEC [reporting].[sys].[sp_executesql] N'{verb} TABLE [dbo].[{}] {suffix}'", self.table)
            }
        }
    }

    fn columns_match(&self, catalog: &Catalog, expected: Option<&[&str]>) -> bool {
        match (catalog.relation("dbo", &self.table), expected) {
            (None, None) => true,
            (Some(table), Some(columns)) => table.columns.iter().map(|c| c.name.as_str()).eq(columns.iter().copied()),
            _ => false,
        }
    }

    fn only_tracked_catalogs(&self) {
        for state in self.engine.boot().live {
            let ConnStatus::Connected { instance, .. } = state.status else { panic!("Connection was lost") };
            for database in instance.databases {
                if database.name != "shop" && database.name != "reporting" {
                    assert_eq!(
                        database.catalog,
                        CatalogStatus::Idle,
                        "Refresh opened untracked database {}",
                        database.name
                    );
                }
            }
        }
    }

    async fn converged(&self, before: &[Arc<Catalog>; 4], reporting_columns: Option<&[&str]>) -> [Arc<Catalog>; 4] {
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let catalogs = self.catalogs().await;
                if catalogs[0].version > before[0].version
                    && catalogs[1].version > before[1].version
                    && self.columns_match(&catalogs[0], None)
                    && self.columns_match(&catalogs[1], reporting_columns)
                {
                    return catalogs;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        let last = self.catalogs().await;
        assert!(
            result.is_ok(),
            "Cross-database catalogs did not converge: shop {} -> {}, reporting {} -> {}, reporting shape={}",
            before[0].version,
            last[0].version,
            before[1].version,
            last[1].version,
            self.columns_match(&last[1], reporting_columns)
        );
        let catalogs = result.unwrap();
        for index in [2, 3] {
            assert_eq!(catalogs[index].version, before[index].version, "Refresh escaped the original connection");
        }
        self.only_tracked_catalogs();
        eprintln!(
            "{}: shop {} -> {}, reporting {} -> {}, reporting shape=true, second connection unchanged",
            self.table, before[0].version, catalogs[0].version, before[1].version, catalogs[1].version
        );
        catalogs
    }

    async fn ordinary_reads(&self) {
        let before = self.catalogs().await.map(|c| c.version);
        assert_eq!(self.scalar(self.tab, "SELECT DB_NAME()").await, "shop");
        assert_eq!(self.scalar(self.reporting, "SELECT DB_NAME()").await, "reporting");
        for _ in 0..10 {
            tokio::time::sleep(Duration::from_millis(20)).await;
            assert_eq!(self.catalogs().await.map(|c| c.version), before, "Ordinary SELECT scheduled a catalog refresh");
        }
        self.only_tracked_catalogs();
        eprintln!("{}: ordinary reads left all four catalog versions unchanged", self.table);
    }

    async fn cleanup(self) {
        self.engine.console_close(self.tab, true).await.unwrap();
        for (tab, database) in [(self.shop, "shop"), (self.reporting, "reporting")] {
            self.ok(tab, &format!("DROP TABLE IF EXISTS [dbo].[{}]", self.table)).await;
            assert_eq!(
                self.scalar(
                    tab,
                    &format!(
                        "SELECT COUNT_BIG(*) FROM sys.tables WHERE schema_id=SCHEMA_ID(N'dbo') AND name=N'{}'",
                        self.table
                    )
                )
                .await,
                "0",
                "Disposable table survived in {database}"
            );
        }
        eprintln!("{}: cleanup verified absent in shop and reporting", self.table);
        for tab in [self.shop, self.reporting, self.other_shop, self.other_reporting] {
            self.engine.console_close(tab, true).await.unwrap();
        }
        for connection in [self.connection, self.other_connection] {
            self.engine.disconnect(connection, true).await.unwrap();
        }
        self.engine.shutdown().await;
    }
}

async fn check(test: impl for<'a> FnOnce(&'a Harness) -> BoxFuture<'a, ()>) {
    let h = Harness::open().await;
    let result = AssertUnwindSafe(test(&h)).catch_unwind().await;
    h.cleanup().await;
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

async fn exercise_ddl(execution: Execution) {
    check(|h| {
        async move {
            let mut before = h.catalogs().await;
            h.ordinary_reads().await;
            for (verb, suffix, columns) in [
                ("CREATE", "(id int)", Some(&["id"][..])),
                ("ALTER", "ADD added_column nvarchar(20)", Some(&["id", "added_column"][..])),
                ("DROP", "", None),
            ] {
                h.ok(h.tab, &h.ddl(execution, verb, suffix)).await;
                assert_eq!(
                    h.scalar(
                        h.reporting,
                        &format!(
                            "SELECT COUNT_BIG(*) FROM sys.tables WHERE schema_id=SCHEMA_ID(N'dbo') AND name=N'{}'",
                            h.table
                        )
                    )
                    .await,
                    if columns.is_some() { "1" } else { "0" }
                );
                before = h.converged(&before, columns).await;
                assert_eq!(h.scalar(h.tab, "SELECT DB_NAME()").await, "shop");
                eprintln!("{execution:?} {verb}: reporting catalog converged while console remained in shop");
            }
            h.ordinary_reads().await;
        }
        .boxed()
    })
    .await;
}

#[tokio::test]
async fn three_part_ddl_refreshes_tracked_catalogs_on_its_connection() {
    exercise_ddl(Execution::ThreePart).await;
}

#[tokio::test]
async fn cross_database_sp_executesql_refreshes_tracked_catalogs_on_its_connection() {
    exercise_ddl(Execution::SpExecuteSql).await;
}

#[tokio::test]
async fn retiring_native_transaction_refreshes_the_original_connection() {
    check(|h| {
        async move {
            let before = h.catalogs().await;
            let state =
                h.ok(h.tab, &format!("{}; BEGIN TRANSACTION", h.ddl(Execution::ThreePart, "CREATE", "(id int)"))).await;
            assert_ne!(state.txn, TxnState::Idle);
            assert_eq!(
                h.catalogs().await.map(|c| c.version),
                before.clone().map(|c| c.version),
                "Native invalidation settled before transaction end"
            );
            assert_eq!(
                h.scalar(h.reporting, &format!("SELECT COUNT_BIG(*) FROM sys.tables WHERE name=N'{}'", h.table)).await,
                "1"
            );
            h.engine.console_close(h.tab, true).await.unwrap();
            h.configure(h.tab, h.other_connection, "shop").await;
            h.converged(&before, Some(&["id"])).await;
            assert_eq!(h.state(h.tab).config.connection_id, h.other_connection);
            h.ordinary_reads().await;
        }
        .boxed()
    })
    .await;
}
