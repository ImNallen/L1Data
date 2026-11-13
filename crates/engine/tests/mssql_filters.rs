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
                name: "Canonical SQL Server filters".into(),
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

    async fn run(&mut self, request: RunRequest) -> Vec<Vec<Option<String>>> {
        let accepted = self.engine.console_run(self.tab, request, None).await.unwrap();
        let state = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if let AppEvent::ConsoleChanged(state) = self.events.recv().await.unwrap()
                    && matches!(state.run, RunState::Finished { run_id, .. } if run_id == accepted.run_id)
                {
                    return state;
                }
            }
        })
        .await
        .expect("SQL Server run did not finish");
        let RunState::Finished { statements, outcome, .. } = &state.run else { unreachable!() };
        assert!(matches!(outcome, RunOutcome::Ok), "{:?}", state.run);
        statements
            .iter()
            .flat_map(|s| &s.outputs)
            .filter_map(|o| match o {
                RunOutput::Rows { result, .. } => {
                    let mut page = self.engine.result_page(result.r#ref, 0, 100).unwrap();
                    for (row, column) in page.cut {
                        page.rows[row as usize][usize::from(column)] =
                            self.engine.result_cell(result.r#ref, row, column).unwrap();
                    }
                    Some(page.rows)
                }
                _ => None,
            })
            .flatten()
            .collect()
    }

    async fn sql(&mut self, text: &str) -> Vec<Vec<Option<String>>> {
        self.run(RunRequest::Script {
            text: text.into(),
            target: RunTarget::All,
            mode: RunMode::Run,
            params: Vec::new(),
            cells: Vec::new(),
        })
        .await
    }

    async fn page(&mut self, table: &str, query: TableQuery) -> Vec<Vec<Option<String>>> {
        self.run(RunRequest::TablePage { schema: "dbo".into(), table: table.into(), page: 0, query }).await
    }

    async fn filter(&mut self, table: &str, column: &str, op: FilterOp, value: &str, expected: &[&str]) {
        let query = TableQuery {
            filters: vec![ColumnFilter { column: column.into(), op, value: value.into() }],
            ..Default::default()
        };
        let rows = self.page(table, query.clone()).await;
        let count =
            self.engine.exact_row_count(self.tab, self.db.clone(), "dbo".into(), table.into(), query).await.unwrap();
        assert_eq!(
            rows.iter().map(|r| r[0].as_deref().unwrap()).collect::<Vec<_>>(),
            expected,
            "{column} {op:?} {value}"
        );
        assert_eq!(count, expected.len() as u64, "{column} {op:?} {value}");
    }

    async fn selected(&mut self, table: &str, columns: &[(&str, &str)]) {
        let rows = self.page(table, TableQuery::default()).await;
        assert_eq!(rows.len(), 3);
        for (i, (column, expected)) in columns.iter().enumerate() {
            let selected = rows[0][i + 1].as_deref().unwrap();
            assert_eq!(selected, *expected, "canonical display for {column}");
            for op in [FilterOp::Eq, FilterOp::Contains, FilterOp::StartsWith] {
                self.filter(table, column, op, selected, &["1"]).await;
            }
            self.filter(table, column, FilterOp::Ne, selected, &["2"]).await;
            self.filter(table, column, FilterOp::IsNull, "ignored", &["3"]).await;
            self.filter(table, column, FilterOp::NotNull, "ignored", &["1", "2"]).await;
        }
    }

    async fn cleanup(&mut self, table: &str, outcome: std::thread::Result<()>) {
        assert_eq!(
            self.sql(&format!("DROP TABLE IF EXISTS dbo.[{table}]; SELECT OBJECT_ID(N'dbo.{table}')")).await,
            vec![vec![None]]
        );
        self.engine.shutdown().await;
        if let Err(panic) = outcome {
            std::panic::resume_unwind(panic);
        }
    }
}

#[tokio::test]
async fn displayed_text_lobs_and_xml_support_page_and_count_filters() {
    let mut h = Harness::open().await;
    let table = format!("l1_display_text_{}", uuid::Uuid::new_v4().simple());
    let outcome = AssertUnwindSafe(async {
        h.sql(&format!("CREATE TABLE dbo.[{table}] (id int PRIMARY KEY, legacy text, unicode_legacy ntext, doc xml, plain varchar(max), unicode_plain nvarchar(max), fixed char(16), unicode_fixed nchar(16)); INSERT INTO dbo.[{table}] VALUES (1,'O''Brien ~%_[x]',N'O''Brien ~%_[x] 旅',N'<x>O''Brien ~%_[x] 旅</x>','O''Brien ~%_[x]',N'O''Brien ~%_[x] 旅','O''Brien',N'O''Brien 旅'),(2,'different',N'different',N'<y/>','different',N'different','different',N'different'),(3,NULL,NULL,NULL,NULL,NULL,NULL,NULL)")).await;
        h.engine.refresh_catalog(h.db.clone()).await.unwrap();
        h.selected(&table, &[
            ("legacy", "O'Brien ~%_[x]"), ("unicode_legacy", "O'Brien ~%_[x] 旅"),
            ("doc", "<x>O'Brien ~%_[x] 旅</x>"), ("plain", "O'Brien ~%_[x]"), ("unicode_plain", "O'Brien ~%_[x] 旅"),
            ("fixed", "O'Brien         "), ("unicode_fixed", "O'Brien 旅       "),
        ]).await;
        h.filter(&table, "doc", FilterOp::Contains, "O'Brien ~%_[x]", &["1"]).await;
        h.filter(&table, "unicode_legacy", FilterOp::StartsWith, "O'Brien", &["1"]).await;
        h.filter(&table, "legacy", FilterOp::Contains, "'; DROP TABLE dbo.orders;--", &[]).await;
        let long = format!("{} O'Brien ~%_[x] 旅", "x".repeat(4500));
        let literal = long.replace('\'', "''");
        h.sql(&format!("UPDATE dbo.[{table}] SET unicode_legacy=N'{literal}', unicode_plain=N'{literal}', doc=N'<x>{literal}</x>' WHERE id=1")).await;
        let selected = h.page(&table, TableQuery::default()).await;
        assert_eq!(selected[0][2].as_deref(), Some(long.as_str()));
        assert_eq!(selected[0][5].as_deref(), Some(long.as_str()));
        assert_eq!(selected[0][3], Some(format!("<x>{long}</x>")));
        for (column, index) in [("unicode_legacy", 2), ("doc", 3), ("unicode_plain", 5)] {
            let value = selected[0][index].as_deref().unwrap();
            assert!(value.len() > 4500);
            h.filter(&table, column, FilterOp::Eq, value, &["1"]).await;
            h.filter(&table, column, FilterOp::Ne, value, &["2"]).await;
            h.filter(&table, column, FilterOp::Contains, "O'Brien ~%_[x] 旅", &["1"]).await;
            h.filter(&table, column, FilterOp::StartsWith, &value[..60], &["1"]).await;
            for op in [FilterOp::Contains, FilterOp::StartsWith] {
                let query = TableQuery { filters: vec![ColumnFilter { column: column.into(), op, value: value.into() }], ..Default::default() };
                assert!(matches!(h.engine.exact_row_count(h.tab, h.db.clone(), "dbo".into(), table.clone(), query.clone()).await,
                    Err(CountError::Failed { message }) if message.contains("4000")));
                assert!(matches!(h.engine.console_run(h.tab, RunRequest::TablePage { schema: "dbo".into(), table: table.clone(), page: 0, query }, None).await,
                    Err(RunError::BadFilter)));
            }
        }
    }).catch_unwind().await;
    h.cleanup(&table, outcome).await;
}

#[tokio::test]
async fn temporal_search_uses_displayed_precision_and_offsets_under_dmy_and_mdy() {
    let mut h = Harness::open().await;
    let table = format!("l1_display_time_{}", uuid::Uuid::new_v4().simple());
    let outcome = AssertUnwindSafe(async {
        h.sql(&format!("CREATE TABLE dbo.[{table}] (id int PRIMARY KEY, old datetime, small smalldatetime, day date, tick time(7), tick_zero time(0), tick_millis time(3), modern datetime2(7), modern_zero datetime2(0), modern_millis datetime2(3), moment datetimeoffset(7), moment_zero datetimeoffset(0), moment_millis datetimeoffset(3)); INSERT INTO dbo.[{table}] VALUES (1,N'2024-01-02T03:04:05.123',N'2024-01-02T03:04:00',N'20240102',N'03:04:05.1234567',N'03:04:05',N'03:04:05.123',N'2024-01-02T03:04:05.1234567',N'2024-01-02T03:04:05',N'2024-01-02T03:04:05.123',N'2024-01-02T03:04:05.1234567-03:30',N'2024-01-02T03:04:05+02:00',N'2024-01-02T03:04:05.123+00:00'),(2,N'2024-02-01T04:03:06.127',N'2024-02-01T04:03:00',N'20240201',N'04:03:06.7654321',N'04:03:06',N'04:03:06.765',N'2024-02-01T04:03:06.7654321',N'2024-02-01T04:03:06',N'2024-02-01T04:03:06.765',N'2024-02-01T04:03:06.7654321+02:00',N'2024-02-01T04:03:06-03:30',N'2024-02-01T04:03:06.765+01:00'),(3,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL)")).await;
        h.engine.refresh_catalog(h.db.clone()).await.unwrap();
        for (language, dateformat) in [("French", "dmy"), ("us_english", "mdy")] {
            h.sql(&format!("SET LANGUAGE {language}; SET DATEFORMAT {dateformat}")).await;
            h.selected(&table, &[
                ("old", "2024-01-02 03:04:05.123"), ("small", "2024-01-02 03:04:00"), ("day", "2024-01-02"),
                ("tick", "03:04:05.1234567"), ("tick_zero", "03:04:05.0000000"), ("tick_millis", "03:04:05.1230000"),
                ("modern", "2024-01-02 03:04:05.1234567"), ("modern_zero", "2024-01-02 03:04:05.0000000"), ("modern_millis", "2024-01-02 03:04:05.1230000"),
                ("moment", "2024-01-02 03:04:05.1234567-03:30"), ("moment_zero", "2024-01-02 03:04:05.0000000+02:00"), ("moment_millis", "2024-01-02 03:04:05.1230000+00:00"),
            ]).await;
            for column in ["old", "small", "day", "modern", "modern_zero", "moment", "moment_zero"] {
                h.filter(&table, column, FilterOp::StartsWith, "2024-01-02", &["1"]).await;
            }
            h.filter(&table, "moment", FilterOp::Contains, "-03:30", &["1"]).await;
            h.filter(&table, "tick", FilterOp::StartsWith, "03:04", &["1"]).await;
        }
    }).catch_unwind().await;
    h.cleanup(&table, outcome).await;
}

#[tokio::test]
async fn approximate_numbers_keep_typed_equality_and_refuse_inexact_text_search() {
    let mut h = Harness::open().await;
    let table = format!("l1_display_float_{}", uuid::Uuid::new_v4().simple());
    let outcome = AssertUnwindSafe(async {
        h.sql(&format!("CREATE TABLE dbo.[{table}] (id int PRIMARY KEY, double_value float, single_value real); INSERT INTO dbo.[{table}] VALUES (1,1.2345678912345679E8,1.2345678E0),(2,2.0E0,2.0E0),(3,NULL,NULL)")).await;
        h.engine.refresh_catalog(h.db.clone()).await.unwrap();
        let rows = h.page(&table, TableQuery::default()).await;
        assert_eq!(rows[0], vec![Some("1".into()), Some("123456789.12345679".into()), Some("1.2345678".into())]);
        for (index, column) in [(1, "double_value"), (2, "single_value")] {
            let selected = rows[0][index].as_deref().unwrap();
            h.filter(&table, column, FilterOp::Eq, selected, &["1"]).await;
            h.filter(&table, column, FilterOp::Ne, selected, &["2"]).await;
            h.filter(&table, column, FilterOp::IsNull, "ignored", &["3"]).await;
            for op in [FilterOp::Contains, FilterOp::StartsWith] {
                let query = TableQuery { filters: vec![ColumnFilter { column: column.into(), op, value: selected.into() }], ..Default::default() };
                let count = h.engine.exact_row_count(h.tab, h.db.clone(), "dbo".into(), table.clone(), query.clone()).await;
                assert!(matches!(&count, Err(CountError::Failed { message }) if message.contains("canonical") && message.contains("comparison")), "{column} {op:?}: {count:?}");
                assert!(matches!(h.engine.console_run(h.tab, RunRequest::TablePage { schema: "dbo".into(), table: table.clone(), page: 0, query }, None).await,
                    Err(RunError::BadFilter)));
            }
        }
    }).catch_unwind().await;
    h.cleanup(&table, outcome).await;
}

#[tokio::test]
async fn exact_numeric_and_binary_searches_use_all_displayed_digits() {
    let mut h = Harness::open().await;
    let table = format!("l1_display_native_{}", uuid::Uuid::new_v4().simple());
    let outcome = AssertUnwindSafe(async {
        h.sql(&format!("CREATE TABLE dbo.[{table}] (id int PRIMARY KEY, amount money, small_amount smallmoney, precise decimal(38,12), integer_value bigint, flag bit, uid uniqueidentifier, bytes varbinary(max), legacy image); INSERT INTO dbo.[{table}] VALUES (1,-922337203685477.5808,-214748.3648,12345678901234567890.123456789012,9223372036854775807,1,N'abcd1234-5678-90ab-cdef-1234567890ab',0xDEADBEEF,0xDEADBEEF),(2,1.0001,1.0001,2.000000000001,-9223372036854775808,0,N'def01234-5678-90ab-cdef-1234567890ab',0xDEADCAFE,0xDEADCAFE),(3,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL)")).await;
        h.engine.refresh_catalog(h.db.clone()).await.unwrap();
        h.selected(&table, &[
            ("amount", "-922337203685477.5808"), ("small_amount", "-214748.3648"),
            ("precise", "12345678901234567890.123456789012"), ("integer_value", "9223372036854775807"),
            ("flag", "1"), ("uid", "abcd1234-5678-90ab-cdef-1234567890ab"), ("bytes", "0xDEADBEEF"), ("legacy", "0xDEADBEEF"),
        ]).await;
        h.filter(&table, "amount", FilterOp::Contains, "5808", &["1"]).await;
        h.filter(&table, "small_amount", FilterOp::StartsWith, "-214748", &["1"]).await;
        h.filter(&table, "bytes", FilterOp::Contains, "BEEF", &["1"]).await;
    }).catch_unwind().await;
    h.cleanup(&table, outcome).await;
}
