#![cfg(feature = "mssql-tests")]

use engine::model::*;
use engine::{AgentPath, Engine, EngineConfig, MemoryKeyStore};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;

fn draft(database: &str) -> ConnectionDraft {
    ConnectionDraft {
        database_kind: DatabaseKind::SqlServer,
        id: None,
        name: "SQL Server identity test".into(),
        host: std::env::var("L1_DATA_TEST_MSSQL_HOST").unwrap_or_else(|_| "localhost".into()),
        port: std::env::var("L1_DATA_TEST_MSSQL_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(1434),
        database: database.into(),
        user: "sa".into(),
        password: PasswordInput::Set(std::env::var("L1_DATA_TEST_MSSQL_PASSWORD").unwrap_or_else(|_| "L1Data_dev_Str0ng".into())),
        ssl_mode: SslMode::Require,
        ca_cert_path: None,
        color: None,
        ssh: None,
        environment: None,
        sql_server_auth: None,
    }
}

async fn open(dir: &std::path::Path, keys: Arc<MemoryKeyStore>) -> Arc<Engine> {
    Engine::open(EngineConfig { data_dir: dir.into(), keys, agent_path: AgentPath::Fixed("".into()), az_binary: None, user_known_hosts: None, updates: Arc::new(engine::ScriptedTransport::named("off").unwrap()) }).await.unwrap()
}

fn console(connection_id: ConnectionId, database: &str) -> ConsoleConfig {
    ConsoleConfig { connection_id, database: database.into(), schema: None, mode: TxnMode::AutoCommit }
}

fn saved(connection_id: ConnectionId, database: &str, table: bool) -> TabSnapshot {
    TabSnapshot {
        id: TabId(uuid::Uuid::new_v4()),
        title: "identity".into(),
        text: Some(if table { "SELECT * FROM [dbo].[orders]" } else { "SELECT DB_NAME() AS identity_home" }.into()),
        console: ConsoleConfigSnapshot { connection_id: Some(connection_id), database: Some(database.into()), schema: None, mode: TxnMode::AutoCommit },
        table: table.then(|| TableSnapshot { schema: "dbo".into(), table: "orders".into(), page: 0, query: TableQuery::default() }),
        file: None,
    }
}

async fn run(engine: &Engine, tab: TabId, request: RunRequest) -> ConsoleState {
    let mut events = engine.subscribe();
    let accepted = engine.console_run(tab, request, None).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match events.recv().await {
                Ok(AppEvent::ConsoleChanged(state)) if state.tab_id == tab && matches!(state.run, RunState::Finished { run_id, .. } if run_id == accepted.run_id) => {
                    assert!(matches!(state.run, RunState::Finished { outcome: RunOutcome::Ok, .. }), "{:?}", state.run);
                    return state;
                }
                Err(broadcast::error::RecvError::Closed) => panic!("Engine events closed"),
                _ => {}
            }
        }
    })
    .await
    .expect("Identity query did not finish")
}

fn result(state: &ConsoleState) -> &ResultMeta {
    let RunState::Finished { statements, .. } = &state.run else { panic!("run not finished") };
    statements.iter().flat_map(|statement| &statement.outputs).find_map(|output| match output {
        RunOutput::Rows { result, .. } => Some(result),
        _ => None,
    }).expect("query returned rows")
}

fn query() -> RunRequest {
    RunRequest::Script { text: "SELECT DB_NAME() AS identity_home".into(), target: RunTarget::All, mode: RunMode::Run, params: Vec::new() }
}

#[tokio::test]
async fn accepted_login_alias_tests_and_connects_with_a_canonical_home() {
    let dir = tempfile::tempdir().unwrap();
    let engine = open(dir.path(), Arc::new(MemoryKeyStore::default())).await;
    engine.test_connection(draft("SHOP")).await.expect("SQL Server accepts SHOP as the login database");
    let config = engine.save_connection(draft("SHOP")).await.unwrap();
    engine.connect(config.id).await.unwrap();
    let snapshot = engine.boot();
    let connection = snapshot.live.iter().find(|c| c.id == config.id).unwrap();
    let ConnStatus::Connected { instance, .. } = &connection.status else { panic!("{connection:?}") };
    assert_eq!(instance.home, "shop");
    assert_eq!(instance.configured_database, "SHOP");
    let alias = DatabaseRef { connection_id: config.id, database: "SHOP".into() };
    let catalog = engine.catalog(alias).await.unwrap();
    assert_eq!(catalog.db.database, "shop");
    assert_eq!(engine.boot().connections[0].database, "SHOP");
    engine.disconnect(config.id, true).await.unwrap();
}

#[tokio::test]
async fn restored_targets_catalogs_table_results_and_history_share_the_canonical_identity() {
    let dir = tempfile::tempdir().unwrap();
    let keys = Arc::new(MemoryKeyStore::default());
    let engine = open(dir.path(), keys.clone()).await;
    let config = engine.save_connection(draft("SHOP")).await.unwrap();
    let table = saved(config.id, "SHOP", true);
    let distinct = saved(config.id, "Shop", false);
    let notebook = |database: &str, cells: Vec<TabId>| NotebookSnapshot {
        id: NotebookId(uuid::Uuid::new_v4()),
        title: database.into(),
        db: Some(DatabaseRef { connection_id: config.id, database: database.into() }),
        active: cells.first().copied(),
        cells,
        shown_at_ms: 0,
        file: None,
    };
    let notebooks = vec![notebook("SHOP", vec![table.id]), notebook("Shop", vec![distinct.id])];
    engine.save_workspace(Workspace { tabs: vec![table.clone(), distinct.clone()], active: Some(table.id), notebooks: notebooks.clone(), ..Workspace::default() });
    let before = engine.console_configure(table.id, console(config.id, "SHOP")).await.unwrap();
    assert_eq!(before.config.database, "SHOP");
    let state = run(&engine, table.id, RunRequest::TablePage { schema: "dbo".into(), table: "orders".into(), page: 0, query: TableQuery::default() }).await;
    assert_eq!(state.config.database, "shop");
    assert!(state.rev > before.rev);
    let meta = result(&state);
    let db = DatabaseRef { connection_id: config.id, database: "shop".into() };
    assert_eq!(meta.db, db);
    assert!(!engine.result_page(meta.r#ref, 0, 10).unwrap().rows.is_empty());
    let alias = DatabaseRef { connection_id: config.id, database: "SHOP".into() };
    let catalog = engine.catalog(alias.clone()).await.unwrap();
    assert_eq!(catalog.db, db);
    assert_eq!(engine.catalog(db.clone()).await.unwrap().version, catalog.version);
    let orders = catalog.schemas.iter().find(|s| s.name == "dbo").unwrap().relations.iter().find(|r| r.name == "orders").unwrap();
    assert_eq!(engine.relation_detail(alias.clone(), orders.oid).await.unwrap().db, db);
    assert_eq!(engine.exact_row_count(table.id, alias, "dbo".into(), "orders".into(), TableQuery::default()).await.unwrap(), u64::from(meta.row_count));
    let reconfigured = engine.console_configure(table.id, console(config.id, "SHOP")).await.unwrap();
    assert_eq!(reconfigured.config.database, "shop");
    assert!(matches!((state.session, reconfigured.session), (SessionState::Open { backend_pid: a }, SessionState::Open { backend_pid: b }) if a == b));
    let state = run(&engine, table.id, query()).await;
    assert_eq!(result(&state).db, db);
    assert_eq!(engine.result_page(result(&state).r#ref, 0, 1).unwrap().rows, vec![vec![Some("shop".into())]]);
    let history = engine.history(HistoryQuery { text: None, connection_id: Some(config.id), before_id: None, limit: 10 });
    assert_eq!(history.entries.len(), 2);
    assert!(history.entries.iter().all(|entry| entry.database == "shop" && entry.database_kind == DatabaseKind::SqlServer));
    let restored = engine.boot().workspace;
    assert_eq!(restored.tabs[0].console.database.as_deref(), Some("shop"));
    assert_eq!(restored.tabs[1].console.database.as_deref(), Some("Shop"));
    assert_eq!(restored.tabs[0].table, table.table);
    let databases = |w: &Workspace| w.notebooks.iter().map(|n| n.db.as_ref().unwrap().database.clone()).collect::<Vec<_>>();
    assert_eq!(databases(&restored), ["shop", "Shop"], "a notebook resolves as its cells do");
    engine.save_workspace(Workspace { tabs: vec![table, distinct], active: restored.active, layout: restored.layout, notebooks, shown: None });
    engine.disconnect(config.id, true).await.unwrap();
    drop(engine);
    let reopened = open(dir.path(), keys).await;
    let snapshot = reopened.boot();
    assert_eq!(snapshot.connections[0].database, "SHOP");
    let restored = &snapshot.workspace.tabs[0];
    assert_eq!(restored.console.database.as_deref(), Some("shop"));
    assert_eq!(snapshot.workspace.tabs[1].console.database.as_deref(), Some("Shop"));
    assert_eq!(databases(&snapshot.workspace), ["shop", "Shop"]);
    reopened.console_configure(restored.id, console(config.id, restored.console.database.as_deref().unwrap())).await.unwrap();
    let state = run(&reopened, restored.id, query()).await;
    assert_eq!(state.config.database, "shop");
    assert_eq!(result(&state).db, db);
    reopened.disconnect(config.id, true).await.unwrap();
}

#[tokio::test]
async fn saved_config_edits_and_other_case_variants_do_not_retarget_the_connected_alias() {
    let dir = tempfile::tempdir().unwrap();
    let engine = open(dir.path(), Arc::new(MemoryKeyStore::default())).await;
    let config = engine.save_connection(draft("SHOP")).await.unwrap();
    engine.connect(config.id).await.unwrap();
    let mut edited = draft("reporting");
    edited.id = Some(config.id);
    edited.password = PasswordInput::Keep;
    engine.save_connection(edited).await.unwrap();
    let boot = engine.boot();
    assert_eq!(boot.connections[0].database, "reporting");
    let ConnStatus::Connected { instance, .. } = &boot.live[0].status else { panic!("not connected") };
    assert_eq!((&*instance.configured_database, &*instance.home), ("SHOP", "shop"));
    let tab = TabId(uuid::Uuid::new_v4());
    let configured = engine.console_configure(tab, console(config.id, "reporting")).await.unwrap();
    assert_eq!(configured.config.database, "reporting");
    let state = run(&engine, tab, query()).await;
    assert_eq!(result(&state).db.database, "reporting");
    assert_eq!(engine.result_page(result(&state).r#ref, 0, 1).unwrap().rows, vec![vec![Some("reporting".into())]]);
    assert_eq!(engine.catalog(DatabaseRef { connection_id: config.id, database: "SHOP".into() }).await.unwrap().db.database, "shop");
    for database in ["Shop", "sHoP", "REPORTING"] {
        assert!(matches!(engine.catalog(DatabaseRef { connection_id: config.id, database: database.into() }).await, Err(CatalogError::UnknownDatabase { .. })));
        let state = engine.console_configure(tab, console(config.id, database)).await.unwrap();
        assert_eq!(state.config.database, database);
        assert!(matches!(engine.console_run(tab, query(), None).await, Err(RunError::NotConfigured)));
    }
    engine.console_configure(tab, console(config.id, "reporting")).await.unwrap();
    engine.disconnect(config.id, true).await.unwrap();
    engine.connect(config.id).await.unwrap();
    let boot = engine.boot();
    let ConnStatus::Connected { instance, .. } = &boot.live[0].status else { panic!("not connected") };
    assert_eq!((&*instance.configured_database, &*instance.home), ("reporting", "reporting"));
    assert!(matches!(engine.catalog(DatabaseRef { connection_id: config.id, database: "SHOP".into() }).await, Err(CatalogError::UnknownDatabase { .. })));
    engine.disconnect(config.id, true).await.unwrap();
}
