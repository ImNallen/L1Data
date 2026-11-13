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
    renamed: String,
    procedure: String,
    database: String,
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
        let connection = engine
            .save_connection(ConnectionDraft {
                database_kind: DatabaseKind::SqlServer,
                id: None,
                name: "SQL Server opaque execution refresh".into(),
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
        engine.connect(connection).await.unwrap();
        let prefix = format!("l1_opaque_{}", uuid::Uuid::new_v4().simple());
        let h = Self {
            engine,
            connection,
            tab: TabId(uuid::Uuid::new_v4()),
            observer: TabId(uuid::Uuid::new_v4()),
            table: format!("{prefix}_table"),
            renamed: format!("{prefix}_renamed"),
            procedure: format!("{prefix}_proc"),
            database: format!("{prefix}_db"),
            _dir: dir,
        };
        for tab in [h.tab, h.observer] {
            h.engine
                .console_configure(
                    tab,
                    ConsoleConfig {
                        connection_id: h.connection,
                        database: "shop".into(),
                        schema: None,
                        mode: TxnMode::AutoCommit,
                    },
                )
                .await
                .unwrap();
        }
        assert_eq!(h.scalar("SELECT DB_NAME()").await, "shop");
        h.engine.catalog(h.db()).await.unwrap();
        h
    }

    fn db(&self) -> DatabaseRef {
        DatabaseRef { connection_id: self.connection, database: "shop".into() }
    }

    fn state(&self, tab: TabId) -> ConsoleState {
        self.engine.boot().consoles.into_iter().find(|state| state.tab_id == tab).unwrap()
    }

    async fn request(&self, tab: TabId, request: RunRequest) -> ConsoleState {
        let run = self.engine.console_run(tab, request, None).await.unwrap().run_id;
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
        .expect("Opaque execution run did not finish");
        assert!(matches!(state.run, RunState::Finished { outcome: RunOutcome::Ok, .. }), "{:?}", state.run);
        state
    }

    async fn ok(&self, tab: TabId, sql: &str) -> ConsoleState {
        self.request(
            tab,
            RunRequest::Script {
                text: sql.into(),
                target: RunTarget::All,
                mode: RunMode::Run,
                params: Vec::new(),
                cells: Vec::new(),
            },
        )
        .await
    }

    fn rows(&self, state: &ConsoleState) -> Vec<Vec<Option<String>>> {
        let RunState::Finished { statements, .. } = &state.run else { unreachable!() };
        let result = statements
            .iter()
            .flat_map(|s| &s.outputs)
            .find_map(|o| match o {
                RunOutput::Rows { result, .. } => Some(result.r#ref),
                _ => None,
            })
            .expect("Query returned no rows");
        self.engine.result_page(result, 0, 100).unwrap().rows
    }

    async fn scalar(&self, sql: &str) -> String {
        self.rows(&self.ok(self.observer, sql).await)[0][0].clone().unwrap()
    }

    async fn table_exists(&self, name: &str) -> bool {
        self.scalar(&format!(
            "SELECT COUNT_BIG(*) FROM sys.tables WHERE schema_id=SCHEMA_ID(N'dbo') AND name=N'{name}'"
        ))
        .await
            == "1"
    }

    async fn catalog_after(&self, before: u64, expected: impl Fn(&Catalog) -> bool) -> Arc<Catalog> {
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let catalog = self.engine.catalog(self.db()).await.unwrap();
                if catalog.version > before && expected(&catalog) {
                    return catalog;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        let last = self.engine.catalog(self.db()).await.unwrap();
        assert!(
            result.is_ok(),
            "Catalog did not converge after version {before}; last version={}, expected snapshot={}",
            last.version,
            expected(&last)
        );
        let catalog = result.unwrap();
        eprintln!("{}: catalog {before} -> {}, expected snapshot=true", self.table, catalog.version);
        catalog
    }

    async fn table_after(&self, before: u64, exists: bool) -> Arc<Catalog> {
        assert_eq!(
            self.table_exists(&self.table).await,
            exists,
            "Native table state differs from the requested effect"
        );
        eprintln!("{}: observer exists={exists}", self.table);
        self.catalog_after(before, |c| c.relation("dbo", &self.table).is_some() == exists).await
    }

    async fn database_after(&self, exists: bool) {
        assert_eq!(
            self.scalar(&format!("SELECT COUNT_BIG(*) FROM sys.databases WHERE name=N'{}'", self.database)).await,
            if exists { "1" } else { "0" }
        );
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let state = self.engine.boot().live.into_iter().find(|s| s.id == self.connection).unwrap();
                if let ConnStatus::Connected { instance, .. } = state.status
                    && instance.databases.iter().any(|d| d.name == self.database) == exists
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("Database list did not converge after opaque execution");
        eprintln!("{}: observer and connection list exists={exists}", self.database);
    }

    async fn table_procedure(&self) -> u64 {
        let before = self.engine.catalog(self.db()).await.unwrap().version;
        self.ok(self.tab, &format!("CREATE PROCEDURE dbo.[{}] @create bit AS BEGIN SET NOCOUNT ON; IF @create=1 CREATE TABLE dbo.[{}] (id int PRIMARY KEY); ELSE DROP TABLE dbo.[{}]; END", self.procedure, self.table, self.table)).await;
        self.catalog_after(before, |c| c.schemas.iter().flat_map(|s| &s.functions).any(|f| f.name == self.procedure))
            .await
            .version
    }

    async fn page(&self, table: &str) -> ConsoleState {
        self.request(
            self.tab,
            RunRequest::TablePage { schema: "dbo".into(), table: table.into(), page: 0, query: TableQuery::default() },
        )
        .await
    }

    async fn cleanup(self) {
        self.engine.console_close(self.tab, true).await.unwrap();
        self.ok(self.observer, &format!("DROP PROCEDURE IF EXISTS dbo.[{}]", self.procedure)).await;
        for table in [&self.table, &self.renamed] {
            self.ok(self.observer, &format!("DROP TABLE IF EXISTS dbo.[{table}]")).await;
            assert!(!self.table_exists(table).await, "Disposable table {table} survived cleanup");
        }
        self.ok(self.observer, &format!("DROP DATABASE IF EXISTS [{}]", self.database)).await;
        assert_eq!(
            self.scalar(&format!(
                "SELECT COUNT_BIG(*) FROM sys.procedures WHERE schema_id=SCHEMA_ID(N'dbo') AND name=N'{}'",
                self.procedure
            ))
            .await,
            "0"
        );
        assert_eq!(
            self.scalar(&format!("SELECT COUNT_BIG(*) FROM sys.databases WHERE name=N'{}'", self.database)).await,
            "0"
        );
        eprintln!("{}: cleanup verified both table names, procedure and database absent", self.table);
        self.engine.console_close(self.observer, true).await.unwrap();
        self.engine.disconnect(self.connection, true).await.unwrap();
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

#[tokio::test]
async fn select_into_refreshes_created_table_and_plain_select_does_not_refresh() {
    check(|h| {
        async move {
            let before = h.engine.catalog(h.db()).await.unwrap().version;
            h.ok(h.tab, &format!("SELECT CAST(41 AS int) AS id, N'readable' AS value INTO dbo.[{}]", h.table)).await;
            let catalog = h.table_after(before, true).await;
            assert_eq!(
                catalog.relation("dbo", &h.table).unwrap().columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
                ["id", "value"]
            );
            assert_eq!(h.rows(&h.page(&h.table).await), vec![vec![Some("41".into()), Some("readable".into())]]);
            h.ok(h.tab, "SELECT 1 AS ordinary_read").await;
            tokio::time::sleep(Duration::from_millis(100)).await;
            assert_eq!(h.engine.catalog(h.db()).await.unwrap().version, catalog.version);
        }
        .boxed()
    })
    .await;
}

#[tokio::test]
async fn select_into_refreshes_after_native_commit_and_rollback() {
    check(|h| {
        async move {
            for commit in [true, false] {
                let before = h.engine.catalog(h.db()).await.unwrap().version;
                let state =
                    h.ok(h.tab, &format!("BEGIN TRANSACTION; IF 1=1 SELECT 42 AS id INTO dbo.[{}]", h.table)).await;
                assert_eq!(state.txn, TxnState::Open);
                if commit {
                    h.engine.console_commit(h.tab, None).await.unwrap();
                } else {
                    h.engine.console_rollback(h.tab).await.unwrap();
                }
                let catalog = h.table_after(before, commit).await;
                if commit {
                    h.ok(h.tab, &format!("DROP TABLE dbo.[{}]", h.table)).await;
                    h.table_after(catalog.version, false).await;
                }
            }
        }
        .boxed()
    })
    .await;
}

#[tokio::test]
async fn procedure_exec_and_execute_refresh_created_and_dropped_tables() {
    check(|h| {
        async move {
            let before = h.table_procedure().await;
            h.ok(h.tab, &format!("EXEC dbo.[{}] @create=1", h.procedure)).await;
            let created = h.table_after(before, true).await;
            h.ok(h.tab, &format!("EXECUTE dbo.[{}] @create=0", h.procedure)).await;
            h.table_after(created.version, false).await;
        }
        .boxed()
    })
    .await;
}

#[tokio::test]
async fn implicit_procedure_calls_refresh_tables_and_appear_in_write_history() {
    check(|h| {
        async move {
            let before = h.table_procedure().await;
            h.engine.clear_history();
            h.ok(h.tab, &format!("/* first code token */ dbo.[{}] @create=1", h.procedure)).await;
            let created = h.table_after(before, true).await;
            let history = h
                .engine
                .history_browse(HistoryBrowseQuery {
                    text: None,
                    filter: HistoryFilter::Writes,
                    before_id: None,
                    limit: 10,
                    revision: None,
                })
                .unwrap();
            assert_eq!(history.groups.len(), 1, "Implicit procedure execution was omitted from write history");
            assert!(history.groups[0].is_write);
            h.ok(h.tab, &format!("[dbo].[{}] @create=0", h.procedure)).await;
            h.table_after(created.version, false).await;
        }
        .boxed()
    })
    .await;
}

#[tokio::test]
async fn sp_executesql_refreshes_table_effects_across_go_batches() {
    check(|h| async move {
        let before = h.engine.catalog(h.db()).await.unwrap().version;
        let state = h.ok(h.tab, &format!("SELECT 7;\nGO\nEXEC sys.sp_executesql N'CREATE TABLE dbo.[{}] (id int PRIMARY KEY)';\nGO\nSELECT 8;", h.table)).await;
        let RunState::Finished { statements, .. } = state.run else { unreachable!() };
        assert_eq!(statements.len(), 3);
        let created = h.table_after(before, true).await;
        h.ok(h.tab, &format!("SELECT 9; EXECUTE sys.sp_executesql N'DROP TABLE dbo.[{}]'", h.table)).await;
        h.table_after(created.version, false).await;
    }.boxed()).await;
}

#[tokio::test]
async fn procedure_and_sp_executesql_refresh_database_creation_and_removal() {
    check(|h| async move {
        let before = h.engine.catalog(h.db()).await.unwrap().version;
        h.ok(h.tab, &format!("CREATE PROCEDURE dbo.[{}] @create bit AS BEGIN SET NOCOUNT ON; IF @create=1 EXEC(N'CREATE DATABASE [{}]'); ELSE EXEC(N'DROP DATABASE [{}]'); END", h.procedure, h.database, h.database)).await;
        h.catalog_after(before, |c| c.schemas.iter().flat_map(|s| &s.functions).any(|f| f.name == h.procedure)).await;
        for procedure in [true, false] {
            for create in [true, false] {
                let sql = if procedure { format!("EXEC dbo.[{}] @create={}", h.procedure, u8::from(create)) }
                    else { format!("EXEC sys.sp_executesql N'{} DATABASE [{}]'", if create { "CREATE" } else { "DROP" }, h.database) };
                h.ok(h.tab, &sql).await;
                h.database_after(create).await;
            }
        }
    }.boxed()).await;
}

#[tokio::test]
async fn procedure_refreshes_settle_on_commit_and_typed_rollback() {
    check(|h| {
        async move {
            let mut before = h.table_procedure().await;
            for commit in [true, false] {
                let state = h.ok(h.tab, &format!("BEGIN TRANSACTION; EXEC dbo.[{}] @create=1", h.procedure)).await;
                assert_ne!(state.txn, TxnState::Idle);
                assert_eq!(h.engine.catalog(h.db()).await.unwrap().version, before);
                if commit {
                    h.engine.console_commit(h.tab, None).await.unwrap();
                } else {
                    h.ok(h.tab, "ROLLBACK TRANSACTION").await;
                }
                let settled = h.table_after(before, commit).await;
                before = if commit {
                    h.ok(h.tab, &format!("EXEC dbo.[{}] @create=0", h.procedure)).await;
                    h.table_after(settled.version, false).await.version
                } else {
                    settled.version
                };
            }
        }
        .boxed()
    })
    .await;
}

#[tokio::test]
async fn sp_rename_refreshes_table_and_column_names_and_keeps_table_pages_readable() {
    check(|h| async move {
        let before = h.engine.catalog(h.db()).await.unwrap().version;
        h.ok(h.tab, &format!("CREATE TABLE dbo.[{}] (id int PRIMARY KEY, old_column nvarchar(20)); INSERT INTO dbo.[{}] VALUES (41, N'readable')", h.table, h.table)).await;
        let created = h.table_after(before, true).await;
        let expected = vec![vec![Some("41".into()), Some("readable".into())]];
        assert_eq!(h.rows(&h.page(&h.table).await), expected);
        h.ok(h.tab, &format!("EXEC sys.sp_rename N'dbo.{}', N'{}', N'OBJECT'", h.table, h.renamed)).await;
        assert!(!h.table_exists(&h.table).await);
        assert!(h.table_exists(&h.renamed).await);
        let renamed = h.catalog_after(created.version, |c| c.relation("dbo", &h.table).is_none() && c.relation("dbo", &h.renamed).is_some()).await;
        assert_eq!(h.rows(&h.page(&h.renamed).await), expected);
        h.ok(h.tab, &format!("EXECUTE sys.sp_rename N'dbo.{}.old_column', N'new_column', N'COLUMN'", h.renamed)).await;
        h.catalog_after(renamed.version, |c| c.relation("dbo", &h.renamed).is_some_and(|r| r.columns.iter().any(|c| c.name == "new_column") && r.columns.iter().all(|c| c.name != "old_column"))).await;
        let state = h.page(&h.renamed).await;
        assert_eq!(h.rows(&state), expected);
        let RunState::Finished { statements, .. } = state.run else { unreachable!() };
        let columns = statements.iter().flat_map(|s| &s.outputs).find_map(|o| match o {
            RunOutput::Rows { result, .. } => Some(&result.columns),
            _ => None,
        }).unwrap();
        assert_eq!(columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["id", "new_column"]);
    }.boxed()).await;
}
