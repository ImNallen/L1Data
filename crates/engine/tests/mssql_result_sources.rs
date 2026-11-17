#![cfg(feature = "mssql-tests")]

use engine::model::*;
use engine::{AgentPath, Engine, EngineConfig, MemoryKeyStore};
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
        let events = engine.subscribe();
        let connection = engine
            .save_connection(ConnectionDraft {
                database_kind: DatabaseKind::SqlServer,
                id: None,
                name: "SQL Server result sources".into(),
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
            .unwrap();
        engine.connect(connection.id).await.unwrap();
        let db = DatabaseRef { connection_id: connection.id, database: "shop".into() };
        let tab = TabId(uuid::Uuid::new_v4());
        engine
            .console_configure(
                tab,
                ConsoleConfig {
                    connection_id: connection.id,
                    database: "shop".into(),
                    schema: None,
                    mode: TxnMode::AutoCommit,
                },
            )
            .await
            .unwrap();
        Self { engine, events, db, tab, _dir: dir }
    }

    async fn run(&mut self, text: &str) -> ConsoleState {
        let request = RunRequest::Script {
            text: text.into(),
            target: RunTarget::All,
            mode: RunMode::Run,
            params: Vec::new(),
            cells: Vec::new(),
        };
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

    async fn ok(&mut self, text: &str) -> Vec<ResultMeta> {
        let state = self.run(text).await;
        assert!(matches!(state.run, RunState::Finished { outcome: RunOutcome::Ok, .. }), "{text}: {:?}", state.run);
        result_sets(&state)
    }

    async fn scalar(&mut self, text: &str) -> Option<String> {
        let sets = self.ok(text).await;
        self.engine.result_page(sets[0].r#ref, 0, 1).unwrap().rows[0][0].clone()
    }

    async fn column(&self, table: &str, column: &str) -> Option<ColumnSource> {
        let catalog = self.engine.catalog(self.db.clone()).await.unwrap();
        let relation = catalog.relation("dbo", table).unwrap();
        let attnum = relation.columns.iter().find(|c| c.name == column).unwrap().attnum;
        Some(ColumnSource { table_oid: relation.oid, attnum })
    }
}

fn result_sets(state: &ConsoleState) -> Vec<ResultMeta> {
    let RunState::Finished { statements, .. } = &state.run else { panic!("not finished") };
    statements
        .iter()
        .flat_map(|s| &s.outputs)
        .filter_map(|o| match o {
            RunOutput::Rows { result, .. } => Some(result.clone()),
            _ => None,
        })
        .collect()
}

fn sources(set: &ResultMeta) -> Vec<Option<ColumnSource>> {
    set.columns.iter().map(|c| c.source).collect()
}

#[tokio::test]
async fn each_key_column_of_a_join_names_its_own_table_column() {
    let mut h = Harness::open().await;
    let sets = h
        .ok("SELECT o.id, o.customer_id, c.id AS customer, c.name, o.customer_id + 0 AS plus, \
             CASE WHEN o.id > 0 THEN o.customer_id ELSE 0 END AS chosen, COUNT(*) OVER () AS n \
             FROM dbo.orders o JOIN dbo.customers c ON c.id = o.customer_id")
        .await;
    assert_eq!(
        sources(&sets[0]),
        [
            h.column("orders", "id").await,
            h.column("orders", "customer_id").await,
            h.column("customers", "id").await,
            h.column("customers", "name").await,
            None,
            None,
            None,
        ]
    );
    let sets =
        h.ok("WITH o AS (SELECT customer_id FROM dbo.orders) SELECT * FROM (SELECT TOP 5 customer_id FROM o) q").await;
    assert_eq!(sources(&sets[0]), [h.column("orders", "customer_id").await]);
    h.engine.shutdown().await;
}

#[tokio::test]
async fn columns_without_one_exact_table_column_have_no_source() {
    let mut h = Harness::open().await;
    for sql in [
        "SELECT customer_id + 0 AS customer_id FROM dbo.orders",
        "SELECT MAX(customer_id) AS customer_id, COUNT(*) AS n FROM dbo.orders",
        "SELECT customer_id FROM dbo.orders UNION ALL SELECT id FROM dbo.customers",
        "DECLARE @pick bit = 1; IF @pick = 1 SELECT customer_id AS id FROM dbo.orders ELSE SELECT id FROM dbo.customers",
        "SELECT id FROM reporting.dbo.customers",
        "SELECT CAST(customer_id AS bigint) AS customer_id FROM dbo.orders",
    ] {
        let sets = h.ok(sql).await;
        assert!(sources(&sets[0]).iter().all(Option::is_none), "{sql}: {:?}", sources(&sets[0]));
    }

    let customer_id = h.column("orders", "customer_id").await;
    let sets = h.ok("SELECT customer_id FROM dbo.orders; SELECT customer_id FROM dbo.orders").await;
    assert_eq!((sources(&sets[0]), sources(&sets[1])), (vec![customer_id], vec![None]));
    let sets = h.ok("SELECT customer_id FROM dbo.orders\nGO\nSELECT id FROM dbo.customers").await;
    assert_eq!((sources(&sets[0]), sources(&sets[1])), (vec![customer_id], vec![h.column("customers", "id").await]));

    let sets = h.ok("SELECT id, customer_id FROM dbo.paid_orders").await;
    assert_eq!(sources(&sets[0]), [h.column("paid_orders", "id").await, h.column("paid_orders", "customer_id").await]);

    let sets = h.ok("CREATE TABLE #held (customer_id int); SELECT customer_id FROM #held").await;
    assert_eq!(sources(&sets[0]), [None]);
    let sets = h.ok("SELECT customer_id FROM #held").await;
    assert_eq!(sources(&sets[0]), [None]);

    let state =
        h.run("SELECT customer_id AS id FROM dbo.orders WHERE 1 / 0 = 1; SELECT TOP 1 id FROM dbo.customers").await;
    let failed = result_sets(&state);
    assert_eq!(
        failed.iter().map(|s| (sources(s), s.row_count)).collect::<Vec<_>>(),
        [(vec![customer_id], 0), (vec![None], 1)],
        "the failed statement keeps its own source and the next result takes none"
    );
    h.engine.shutdown().await;
}

#[tokio::test]
async fn the_description_never_runs_the_batch_or_changes_the_session() {
    let mut h = Harness::open().await;
    let sequence = format!("l1_sources_{}", uuid::Uuid::new_v4().simple());
    h.ok(&format!("CREATE SEQUENCE dbo.[{sequence}] START WITH 1")).await;
    let first = h.scalar(&format!("SELECT NEXT VALUE FOR dbo.[{sequence}] AS v")).await;
    let second = h.scalar(&format!("SELECT NEXT VALUE FOR dbo.[{sequence}] AS v")).await;
    h.ok(&format!("DROP SEQUENCE dbo.[{sequence}]")).await;
    assert_eq!((first.as_deref(), second.as_deref()), (Some("1"), Some("2")));

    h.ok("SELECT v FROM (VALUES (1), (2), (3), (4), (5)) AS t(v)").await;
    assert_eq!(h.scalar("SELECT @@ROWCOUNT AS n").await.as_deref(), Some("5"));

    h.ok("SET XACT_ABORT ON; BEGIN TRANSACTION; CREATE TABLE #kept (id int)").await;
    let sets = h.ok("SELECT id FROM #kept").await;
    assert_eq!(sources(&sets[0]), [None]);
    assert_eq!(h.scalar("SELECT @@TRANCOUNT AS n").await.as_deref(), Some("1"));
    h.ok("ROLLBACK; SET XACT_ABORT OFF").await;

    h.ok("SET IMPLICIT_TRANSACTIONS ON").await;
    h.ok("SELECT 1 AS one").await;
    assert_eq!(h.scalar("SELECT @@TRANCOUNT AS n").await.as_deref(), Some("0"));
    h.ok("SET IMPLICIT_TRANSACTIONS OFF").await;
    h.engine.shutdown().await;
}
