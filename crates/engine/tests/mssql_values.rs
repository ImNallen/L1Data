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
                name: "Canonical SQL Server values".into(),
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
        let tab = TabId(uuid::Uuid::new_v4());
        let db = DatabaseRef { connection_id: connection.id, database: "shop".into() };
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

    async fn finish(&mut self, accepted: RunAccepted) -> ConsoleState {
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if let AppEvent::ConsoleChanged(state) = self.events.recv().await.unwrap()
                    && matches!(state.run, RunState::Finished { run_id, .. } if run_id == accepted.run_id)
                {
                    assert!(matches!(state.run, RunState::Finished { outcome: RunOutcome::Ok, .. }), "{:?}", state.run);
                    return state;
                }
            }
        })
        .await
        .expect("SQL Server run did not finish")
    }

    fn rows(&self, state: &ConsoleState) -> Vec<Vec<Option<String>>> {
        let RunState::Finished { statements, .. } = &state.run else { unreachable!() };
        let result = statements
            .iter()
            .flat_map(|s| &s.outputs)
            .find_map(|o| match o {
                RunOutput::Rows { result, .. } => Some(result),
                _ => None,
            })
            .unwrap();
        self.engine.result_page(result.r#ref, 0, 100).unwrap().rows
    }

    async fn sql(&mut self, text: &str) -> Vec<Vec<Option<String>>> {
        let accepted = self
            .engine
            .console_run(
                self.tab,
                RunRequest::Script {
                    text: text.into(),
                    target: RunTarget::All,
                    mode: RunMode::Run,
                    params: Vec::new(),
                    cells: Vec::new(),
                },
                None,
            )
            .await
            .unwrap();
        let state = self.finish(accepted).await;
        let RunState::Finished { statements, .. } = &state.run else { unreachable!() };
        if statements.iter().flat_map(|s| &s.outputs).any(|o| matches!(o, RunOutput::Rows { .. })) {
            self.rows(&state)
        } else {
            vec![]
        }
    }

    async fn filtered(&mut self, table: &str, column: &str, op: FilterOp, value: &str) -> Vec<Vec<Option<String>>> {
        let query = TableQuery {
            filters: vec![ColumnFilter { column: column.into(), op, value: value.into() }],
            ..Default::default()
        };
        let count = self
            .engine
            .exact_row_count(self.tab, self.db.clone(), "dbo".into(), table.into(), query.clone())
            .await
            .unwrap();
        let accepted = self
            .engine
            .console_run(
                self.tab,
                RunRequest::TablePage { schema: "dbo".into(), table: table.into(), page: 0, query },
                None,
            )
            .await
            .unwrap();
        let state = self.finish(accepted).await;
        let rows = self.rows(&state);
        assert_eq!(count, rows.len() as u64, "count and page disagree for {column} {op:?} {value}");
        rows
    }

    async fn cleanup(&mut self, table: &str, outcome: std::thread::Result<()>) {
        assert_eq!(
            self.sql(&format!("DROP TABLE dbo.[{table}]; SELECT OBJECT_ID(N'dbo.{table}')")).await,
            vec![vec![None]]
        );
        if let Err(panic) = outcome {
            std::panic::resume_unwind(panic);
        }
    }
}

#[tokio::test]
async fn native_temporal_values_and_typed_filters_round_trip_under_dmy_and_mdy() {
    let mut h = Harness::open().await;
    for (native, january, february, inserted) in [
        ("datetime", "2024-01-02 03:04:05.123", "2024-02-01 03:04:05.123", "2024-03-04 05:06:07.127"),
        ("smalldatetime", "2024-01-02 03:04:00", "2024-02-01 03:04:00", "2024-03-04 05:06:00"),
        ("date", "2024-01-02", "2024-02-01", "2024-03-04"),
        ("datetime2(7)", "2024-01-02 03:04:05.1234567", "2024-02-01 03:04:05.1234567", "2024-03-04 05:06:07.7654321"),
        (
            "datetimeoffset(7)",
            "2024-01-02 03:04:05.1234567+02:00",
            "2024-02-01 03:04:05.1234567+02:00",
            "2024-03-04 05:06:07.7654321-03:30",
        ),
        ("time(7)", "03:04:05.1234567", "04:03:05.1234567", "05:06:07.7654321"),
    ] {
        for dateformat in ["dmy", "mdy"] {
            let table = format!("l1_canonical_{}", uuid::Uuid::new_v4().simple());
            h.sql(&format!("SET DATEFORMAT {dateformat}; CREATE TABLE dbo.[{table}] (id {native} PRIMARY KEY, assigned {native} NULL, n int NOT NULL); INSERT INTO dbo.[{table}] (id,n) VALUES (CONVERT({native},N'{}',126),1),(CONVERT({native},N'{}',126),2)", january.replace(' ', "T"), february.replace(' ', "T"))).await;
            let outcome = AssertUnwindSafe(async {
                h.engine.refresh_catalog(h.db.clone()).await.unwrap();
                h.sql(&format!("UPDATE dbo.[{table}] SET assigned=CONVERT({native},N'{}',126),n=11 WHERE id=CONVERT({native},N'{}',126)", january.replace(' ', "T"), january.replace(' ', "T"))).await;
                assert_eq!(
                    h.sql(&format!("SELECT id,assigned,n FROM dbo.[{table}] ORDER BY id")).await,
                    vec![vec![Some(january.into()), Some(january.into()), Some("11".into())], vec![Some(february.into()), None, Some("2".into())],],
                    "January key must update January only for {native} under {dateformat}"
                );
                for column in ["id", "assigned"] {
                    let rows = h.filtered(&table, column, FilterOp::Eq, january).await;
                    assert_eq!(rows.len(), 1, "{native} under {dateformat}, {column}");
                    assert_eq!(rows[0][0].as_deref(), Some(january));
                }
                h.sql(&format!("INSERT INTO dbo.[{table}] (id,assigned,n) VALUES(CONVERT({native},N'{}',126),CONVERT({native},N'{}',126),3)", inserted.replace(' ', "T"), inserted.replace(' ', "T"))).await;
                let rows = h.filtered(&table, "id", FilterOp::Eq, inserted).await;
                assert_eq!(rows, vec![vec![Some(inserted.into()), Some(inserted.into()), Some("3".into())]]);
                h.sql(&format!("DELETE FROM dbo.[{table}] WHERE id=CONVERT({native},N'{}',126)", january.replace(' ', "T"))).await;
                assert_eq!(
                    h.sql(&format!("SELECT id,n FROM dbo.[{table}] ORDER BY id")).await,
                    vec![vec![Some(february.into()), Some("2".into())], vec![Some(inserted.into()), Some("3".into())],],
                    "January delete must leave February untouched for {native} under {dateformat}"
                );
            })
            .catch_unwind()
            .await;
            h.cleanup(&table, outcome).await;
        }
    }
}

#[tokio::test]
async fn selected_binary_filters_compare_bytes_and_search_displayed_hex() {
    let mut h = Harness::open().await;
    let table = format!("l1_binary_filter_{}", uuid::Uuid::new_v4().simple());
    h.sql(&format!("CREATE TABLE dbo.[{table}] (id int PRIMARY KEY, bytes varbinary(4), fixed binary(4), label nvarchar(80), precise decimal(38,12), legacy image); INSERT INTO dbo.[{table}] VALUES (1,0xDEADBEEF,0xDEADBEEF,N'O''Brien ~%_[x]',12345678901234567890.123456789012,0xDEADBEEF),(2,0xDEADCAFE,0xDEADCAFE,N'plain',12345678901234567890.123456789013,0xDEADCAFE),(3,NULL,NULL,NULL,NULL,NULL)")).await;
    let outcome = AssertUnwindSafe(async {
        h.engine.refresh_catalog(h.db.clone()).await.unwrap();
        for column in ["bytes", "fixed", "legacy"] {
            let selected =
                h.sql(&format!("SELECT [{column}] FROM dbo.[{table}] WHERE id=1")).await[0][0].clone().unwrap();
            for (op, expected) in [(FilterOp::Eq, "1"), (FilterOp::Ne, "2")] {
                let rows = h.filtered(&table, column, op, &selected).await;
                assert_eq!(rows.len(), 1, "{column} {op:?} {selected}");
                assert_eq!(rows[0][0].as_deref(), Some(expected));
            }
            for (op, value) in [(FilterOp::Contains, "BEEF"), (FilterOp::StartsWith, "0xDEADB")] {
                let rows = h.filtered(&table, column, op, value).await;
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0][0].as_deref(), Some("1"));
            }
            assert_eq!(h.filtered(&table, column, FilterOp::IsNull, "ignored").await[0][0].as_deref(), Some("3"));
        }
        for (column, op, value) in [
            ("label", FilterOp::Eq, "O'Brien ~%_[x]"),
            ("label", FilterOp::Contains, "~%_[x]"),
            ("label", FilterOp::StartsWith, "O'Brien"),
            ("precise", FilterOp::Eq, "12345678901234567890.123456789012"),
            ("precise", FilterOp::Lt, "12345678901234567890.123456789013"),
        ] {
            let rows = h.filtered(&table, column, op, value).await;
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0][0].as_deref(), Some("1"));
        }
        for (column, value) in [
            ("bytes", "DEADBEEF"),
            ("bytes", "0xABC"),
            ("bytes", "0x0001020304"),
            ("bytes", "0x'; DROP TABLE dbo.orders;--"),
            ("missing", "1"),
        ] {
            let query = TableQuery {
                filters: vec![ColumnFilter { column: column.into(), op: FilterOp::Eq, value: value.into() }],
                ..Default::default()
            };
            assert!(
                h.engine
                    .exact_row_count(h.tab, h.db.clone(), "dbo".into(), table.clone(), query.clone())
                    .await
                    .is_err()
            );
            assert!(matches!(
                h.engine
                    .console_run(
                        h.tab,
                        RunRequest::TablePage { schema: "dbo".into(), table: table.clone(), page: 0, query },
                        None
                    )
                    .await,
                Err(RunError::BadFilter)
            ));
        }
    })
    .catch_unwind()
    .await;
    h.cleanup(&table, outcome).await;
}

#[tokio::test]
async fn rowversion_metadata_and_filters_track_native_updates() {
    let mut h = Harness::open().await;
    let table = format!("l1_rowversion_key_{}", uuid::Uuid::new_v4().simple());
    h.sql(&format!("CREATE TABLE dbo.[{table}] (version rowversion PRIMARY KEY, n int NULL); INSERT INTO dbo.[{table}] (n) VALUES (1)")).await;
    let outcome = AssertUnwindSafe(async {
        h.engine.refresh_catalog(h.db.clone()).await.unwrap();
        let catalog = h.engine.catalog(h.db.clone()).await.unwrap();
        let relation = catalog.relation("dbo", &table).unwrap();
        let version = relation.columns.iter().find(|c| c.name == "version").unwrap();
        assert!(version.pk);
        assert!(!version.writable);
        let original = h.sql(&format!("SELECT version FROM dbo.[{table}]")).await[0][0].clone().unwrap();
        h.sql(&format!("UPDATE dbo.[{table}] SET n=NULL WHERE version=CONVERT(varbinary(8),N'{original}',1)")).await;
        let rows = h.sql(&format!("SELECT version,n FROM dbo.[{table}]")).await;
        let current = rows[0][0].clone().unwrap();
        assert_ne!(original, current);
        assert_eq!(rows[0][1], None);
        assert_eq!(h.filtered(&table, "version", FilterOp::Eq, &current).await.len(), 1);
        assert!(h.filtered(&table, "version", FilterOp::Eq, &original).await.is_empty());
        h.sql(&format!("DELETE FROM dbo.[{table}] WHERE version=CONVERT(varbinary(8),N'{current}',1)")).await;
        assert_eq!(h.sql(&format!("SELECT COUNT_BIG(*) FROM dbo.[{table}]")).await, vec![vec![Some("0".into())]]);
    })
    .catch_unwind()
    .await;
    h.cleanup(&table, outcome).await;
}
