#![cfg(feature = "mssql-tests")]

use engine::model::*;
use engine::{AgentPath, Engine, EngineConfig, MemoryKeyStore};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;

struct Harness {
    engine: Arc<Engine>,
    events: broadcast::Receiver<AppEvent>,
    connection: ConnectionId,
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
                name: "SQL Server refresh and paging test".into(),
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
            .unwrap()
            .id;
        engine.connect(connection).await.unwrap();
        let tab = TabId(uuid::Uuid::new_v4());
        engine
            .console_configure(
                tab,
                ConsoleConfig {
                    connection_id: connection,
                    database: database.into(),
                    schema: None,
                    mode: TxnMode::AutoCommit,
                },
            )
            .await
            .unwrap();
        let mut settings = engine.settings();
        settings.table_page_size = 2;
        engine.set_settings(settings);
        Self { engine, events, connection, tab, _dir: dir }
    }

    fn db(&self, database: &str) -> DatabaseRef {
        DatabaseRef { connection_id: self.connection, database: database.into() }
    }

    async fn request(&mut self, request: RunRequest) -> ConsoleState {
        let id = self.engine.console_run(self.tab, request, None).await.unwrap().run_id;
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if let AppEvent::ConsoleChanged(state) = self.events.recv().await.unwrap()
                    && state.tab_id == self.tab
                    && matches!(state.run, RunState::Finished { run_id, .. } if run_id == id)
                {
                    return state;
                }
            }
        })
        .await
        .expect("Engine run did not finish")
    }

    async fn run(&mut self, sql: &str) -> ConsoleState {
        self.request(RunRequest::Script {
            text: sql.into(),
            target: RunTarget::All,
            mode: RunMode::Run,
            params: Vec::new(),
            cells: Vec::new(),
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

    async fn catalog_after(&self, database: &str, version: u64) -> Option<Arc<Catalog>> {
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let catalog = self.engine.catalog(self.db(database)).await.unwrap();
                if catalog.version > version {
                    return catalog;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .ok()
    }

    async fn database_list_has(&self, database: &str, expected: bool) -> bool {
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let state = self.engine.boot().live.into_iter().find(|s| s.id == self.connection).unwrap();
                if let ConnStatus::Connected { instance, .. } = state.status
                    && instance.databases.iter().any(|d| d.name == database) == expected
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .is_ok()
    }
}

#[tokio::test]
async fn mixed_batch_refreshes_its_table_catalog_without_manual_refresh() {
    let mut h = Harness::open("shop").await;
    let table = format!("l1_refresh_{}", uuid::Uuid::new_v4().simple());
    Harness::assert_ok(&h.run(&format!("CREATE TABLE dbo.[{table}] (id int PRIMARY KEY)")).await);
    let before = h.engine.refresh_catalog(h.db("shop")).await.unwrap();
    let state =
        h.run(&format!("ALTER TABLE dbo.[{table}] ADD refreshed int; DROP DATABASE IF EXISTS [missing_{table}]")).await;
    let catalog = h.catalog_after("shop", before).await;
    Harness::assert_ok(&h.run(&format!("DROP TABLE dbo.[{table}]")).await);
    Harness::assert_ok(&state);
    let catalog = catalog.expect("mixed DDL batch must publish a new table catalog");
    assert!(catalog.relation("dbo", &table).unwrap().columns.iter().any(|c| c.name == "refreshed"));
}

#[tokio::test]
async fn procedure_body_database_ddl_does_not_hide_the_new_routine() {
    let mut h = Harness::open("shop").await;
    let routine = format!("l1_refresh_proc_{}", uuid::Uuid::new_v4().simple());
    let before = h.engine.catalog(h.db("shop")).await.unwrap().version;
    let state = h
        .run(&format!("CREATE PROCEDURE dbo.[{routine}] AS BEGIN DROP DATABASE IF EXISTS [missing_{routine}]; END"))
        .await;
    let catalog = h.catalog_after("shop", before).await;
    Harness::assert_ok(&h.run(&format!("DROP PROCEDURE dbo.[{routine}]")).await);
    Harness::assert_ok(&state);
    let catalog = catalog.expect("CREATE PROCEDURE must refresh routines even with database DDL in its body");
    assert!(catalog.schemas.iter().flat_map(|s| &s.functions).any(|f| f.name == routine));
}

#[tokio::test]
async fn paging_resolves_case_variants_and_keeps_primary_key_tiebreakers() {
    let mut h = Harness::open("shop").await;
    let table = format!("l1_paging_{}", uuid::Uuid::new_v4().simple());
    Harness::assert_ok(
        &h.run(&format!("CREATE TABLE dbo.[{table}] (id int PRIMARY KEY, priority int); INSERT INTO dbo.[{table}] VALUES (1,1),(2,1),(3,2),(4,2)")).await,
    );
    h.engine.refresh_catalog(h.db("shop")).await.unwrap();
    let mut results = Vec::new();
    for order in ["ID DESC", "[ID] DESC", "\"ID\" DESC", "(ID) DESC", "id DESC", "priority DESC"] {
        for page in 0..2 {
            let state = h
                .request(RunRequest::TablePage {
                    schema: "dbo".into(),
                    table: table.clone(),
                    page,
                    query: TableQuery { order_by: order.into(), ..Default::default() },
                })
                .await;
            let rows = matches!(state.run, RunState::Finished { outcome: RunOutcome::Ok, .. }).then(|| h.rows(&state));
            results.push((order, page, state, rows));
        }
    }
    Harness::assert_ok(&h.run(&format!("DROP TABLE dbo.[{table}]")).await);
    for (order, page, state, rows) in results {
        Harness::assert_ok(&state);
        let rows = rows.unwrap();
        let expected = if order == "priority DESC" { [3, 4, 1, 2] } else { [4, 3, 2, 1] };
        let start = page as usize * 2;
        assert_eq!(
            rows.iter().map(|r| r[0].as_deref().unwrap().parse::<u32>().unwrap()).collect::<Vec<_>>(),
            expected[start..start + 2],
            "order {order}, page {page}"
        );
    }
}

#[tokio::test]
async fn paging_keeps_case_sensitive_identifiers_and_mixed_ddl_refreshes_both_snapshots() {
    let mut admin = Harness::open("shop").await;
    let name = format!("l1_cs_paging_{}", uuid::Uuid::new_v4().simple());
    Harness::assert_ok(&admin.run(&format!("CREATE DATABASE [{name}] COLLATE Latin1_General_100_CS_AS")).await);
    admin.engine.refresh_connection(admin.connection).await.unwrap();
    assert!(admin.database_list_has(&name, true).await);
    let mut h = Harness::open(&name).await;
    Harness::assert_ok(&h.run("CREATE TABLE dbo.paging ([id] int PRIMARY KEY NONCLUSTERED, [ID] int); CREATE CLUSTERED INDEX reverse_ties ON dbo.paging ([ID] DESC, [id] DESC); INSERT INTO dbo.paging VALUES (1,20),(2,10),(3,20),(4,10)").await);
    h.engine.refresh_catalog(h.db(&name)).await.unwrap();
    let mut results = Vec::new();
    for order in ["ID DESC", "[ID] DESC", "id DESC", "[id] DESC"] {
        for page in 0..2 {
            let state = h
                .request(RunRequest::TablePage {
                    schema: "dbo".into(),
                    table: "paging".into(),
                    page,
                    query: TableQuery { order_by: order.into(), ..Default::default() },
                })
                .await;
            let rows = matches!(state.run, RunState::Finished { outcome: RunOutcome::Ok, .. }).then(|| h.rows(&state));
            results.push((order, page, state, rows));
        }
    }
    let wrong_case = h
        .request(RunRequest::TablePage {
            schema: "dbo".into(),
            table: "paging".into(),
            page: 0,
            query: TableQuery { order_by: "[Id] DESC".into(), ..Default::default() },
        })
        .await;
    h.engine.disconnect(h.connection, true).await.unwrap();
    Harness::assert_ok(&admin.run(&format!("CREATE TABLE dbo.[{name}] (id int PRIMARY KEY)")).await);
    let before = admin.engine.refresh_catalog(admin.db("shop")).await.unwrap();
    let mixed = admin.run(&format!("ALTER TABLE dbo.[{name}] ADD refreshed int; DROP DATABASE [{name}]")).await;
    let catalog = admin.catalog_after("shop", before).await;
    let removed = admin.database_list_has(&name, false).await;
    Harness::assert_ok(&admin.run(&format!("DROP TABLE dbo.[{name}]")).await);
    Harness::assert_ok(&mixed);
    assert!(removed, "mixed DDL must remove the dropped database from the connection snapshot");
    let catalog = catalog.expect("mixed DDL must also refresh the current database catalog");
    assert!(catalog.relation("dbo", &name).unwrap().columns.iter().any(|c| c.name == "refreshed"));
    for (order, page, state, rows) in results {
        Harness::assert_ok(&state);
        let expected = if order.contains("ID") { [1, 3, 2, 4] } else { [4, 3, 2, 1] };
        let start = page as usize * 2;
        assert_eq!(
            rows.unwrap().iter().map(|r| r[0].as_deref().unwrap().parse::<u32>().unwrap()).collect::<Vec<_>>(),
            expected[start..start + 2],
            "case-sensitive order {order}, page {page}"
        );
    }
    assert!(matches!(wrong_case.run, RunState::Finished { outcome: RunOutcome::Failed, .. }), "{:?}", wrong_case.run);
}
