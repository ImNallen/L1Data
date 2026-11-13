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
    connection: ConnectionId,
    tab: TabId,
    _dir: tempfile::TempDir,
}
impl Harness {
    async fn open(mode: TxnMode) -> Self {
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
        let config = engine
            .save_connection(ConnectionDraft {
                database_kind: DatabaseKind::SqlServer,
                id: None,
                name: "SQL Server test".into(),
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
        engine.connect(config.id).await.unwrap();
        let tab = TabId(uuid::Uuid::new_v4());
        engine
            .console_configure(
                tab,
                ConsoleConfig { connection_id: config.id, database: "shop".into(), schema: Some("lab".into()), mode },
            )
            .await
            .unwrap();
        Self { engine, events, connection: config.id, tab, _dir: dir }
    }
    fn db(&self) -> DatabaseRef {
        DatabaseRef { connection_id: self.connection, database: "shop".into() }
    }
    async fn start(&self, sql: &str) -> RunAccepted {
        self.engine
            .console_run(
                self.tab,
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
    }
    async fn finish(&mut self, id: RunId) -> ConsoleState {
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if let AppEvent::ConsoleChanged(state) = self.events.recv().await.unwrap()
                    && state.tab_id == self.tab
                    && matches!(state.run, RunState::Finished { run_id, .. } if run_id==id)
                {
                    return state;
                }
            }
        })
        .await
        .expect("Engine run did not finish")
    }
    async fn run(&mut self, sql: &str) -> ConsoleState {
        let id = self.start(sql).await.run_id;
        self.finish(id).await
    }
    async fn ok(&mut self, sql: &str) -> ConsoleState {
        let state = self.run(sql).await;
        assert!(matches!(state.run, RunState::Finished { outcome: RunOutcome::Ok, .. }), "{sql}: {:?}", state.run);
        state
    }
    async fn logged(&mut self, sql: &str) -> (ConsoleState, Vec<OutputBody>) {
        let id = self.start(sql).await.run_id;
        let (tab, events) = (self.tab, &mut self.events);
        let mut log = Vec::new();
        let state = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                match events.recv().await.unwrap() {
                    AppEvent::ConsoleOutput { tab_id, entries } if tab_id == tab => {
                        log.extend(entries.into_iter().filter(|e| e.run_id == id).map(|e| e.body))
                    }
                    AppEvent::ConsoleChanged(state)
                        if state.tab_id == tab
                            && matches!(state.run, RunState::Finished { run_id, .. } if run_id == id) =>
                    {
                        return state;
                    }
                    _ => {}
                }
            }
        })
        .await
        .expect("Engine run did not finish");
        (state, log)
    }
    fn rowsets(state: &ConsoleState) -> Vec<&ResultMeta> {
        let RunState::Finished { statements, .. } = &state.run else { panic!("not finished") };
        statements
            .iter()
            .flat_map(|s| &s.outputs)
            .filter_map(|o| match o {
                RunOutput::Rows { result, .. } => Some(result),
                _ => None,
            })
            .collect()
    }
    fn rows(&self, meta: &ResultMeta) -> Vec<Vec<Option<String>>> {
        self.engine.result_page(meta.r#ref, 0, 100).unwrap().rows
    }
}

#[tokio::test]
async fn real_engine_streams_a_multi_result_batch_and_keeps_zero_row_sets() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    let state =
        h.ok("DECLARE @value int = 42; SELECT @value AS [a]]b], N'世界; GO' AS label; SELECT 9 AS empty WHERE 1=0; SELECT @value + 1 AS answer\nGO\nSELECT DB_NAME() AS db").await;
    let RunState::Finished { statements, .. } = &state.run else { unreachable!() };
    assert_eq!(statements.len(), 2);
    let sets = Harness::rowsets(&state);
    assert_eq!(sets.len(), 4);
    assert_eq!(sets.iter().map(|s| s.r#ref.index).collect::<Vec<_>>(), [0, 1, 2, 3]);
    assert_eq!(h.rows(sets[0]), vec![vec![Some("42".into()), Some("世界; GO".into())]]);
    assert_eq!(sets[0].columns[0].name, "a]b");
    assert!(sets[0].columns.iter().all(|c| c.source.is_none()));
    assert_eq!(sets[1].row_count, 0);
    assert_eq!(h.rows(sets[2])[0][0].as_deref(), Some("43"));
    assert_eq!(h.rows(sets[3])[0][0].as_deref(), Some("shop"));
    assert_eq!((state.txn, state.txn_depth), (TxnState::Idle, Some(0)));
    assert_eq!(sets[0].database_kind, DatabaseKind::SqlServer);
}

fn lines(log: &[OutputBody]) -> Vec<String> {
    log.iter()
        .filter_map(|body| match body {
            OutputBody::Notice { severity, message, .. } => Some(format!("{severity}: {message}")),
            OutputBody::Rows { rows, .. } => Some(format!("rows {rows}")),
            OutputBody::Command { tag, .. } => Some(tag.clone()),
            OutputBody::Error { error, .. } => Some(format!("{}: {}", error.severity, error.message)),
            _ => None,
        })
        .collect()
}

fn summary(state: &ConsoleState) -> Vec<String> {
    let RunState::Finished { statements, .. } = &state.run else { panic!("not finished") };
    statements
        .iter()
        .flat_map(|s| &s.outputs)
        .filter_map(|o| match o {
            RunOutput::Command { tag, .. } => Some(tag.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn print_and_low_severity_raiserror_reach_the_output_log_in_server_order() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    let (_, log) = h.logged("PRINT 'hello'").await;
    assert_eq!(lines(&log), ["NOTICE: hello", "Commands completed successfully"]);

    let (state, log) = h.logged("PRINT '1'; SELECT 2 AS two; PRINT '3';").await;
    assert_eq!(lines(&log), ["NOTICE: 1", "rows 1", "NOTICE: 3"]);
    assert_eq!(Harness::rowsets(&state).len(), 1);

    let (_, log) = h.logged("RAISERROR('informational', 10, 1); RAISERROR('careful', 5, 1); PRINT 'after'").await;
    assert_eq!(
        lines(&log),
        ["NOTICE: informational", "WARNING: careful", "NOTICE: after", "Commands completed successfully"]
    );

    let (state, log) = h.logged("PRINT 'a'; RAISERROR('boom', 16, 1); PRINT 'b'").await;
    assert_eq!(lines(&log), ["NOTICE: a", "ERROR: boom", "NOTICE: b"]);
    assert!(matches!(state.run, RunState::Finished { outcome: RunOutcome::Failed, .. }), "{:?}", state.run);
    let RunState::Finished { statements, .. } = &state.run else { unreachable!() };
    assert!(
        matches!(&statements[0].status, StmtStatus::Failed { error } if error.message == "boom"),
        "{:?}",
        statements[0].status
    );

    let (_, log) = h.logged("RAISERROR('e1', 16, 1); RAISERROR('e2', 16, 1)").await;
    assert_eq!(lines(&log), ["ERROR: e1", "ERROR: e2"]);

    let (_, log) = h.logged("CREATE TABLE #u (id int PRIMARY KEY); INSERT #u VALUES (1); INSERT #u VALUES (1); INSERT #u VALUES (2); DROP TABLE #u").await;
    let log = lines(&log);
    assert_eq!(log.len(), 4, "{log:?}");
    assert_eq!(log[0], "1 row affected");
    assert!(log[1].starts_with("ERROR: Violation of PRIMARY KEY constraint"), "{log:?}");
    assert_eq!(log[2..], ["NOTICE: The statement has been terminated.", "1 row affected"]);
}

#[tokio::test]
async fn a_fatal_error_in_the_batch_retires_the_session() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    let (state, log) = h.logged("PRINT 'before'; RAISERROR('fatal', 20, 1) WITH LOG; PRINT 'never'").await;
    assert_eq!(lines(&log)[..2], ["NOTICE: before", "FATAL: fatal"]);
    assert!(!lines(&log).contains(&"NOTICE: never".into()), "{log:?}");
    assert!(
        log.iter().any(|body| matches!(body, OutputBody::Info { message } if message.starts_with("Session reset"))),
        "{log:?}"
    );
    let RunState::Finished { statements, .. } = &state.run else { unreachable!() };
    assert!(
        matches!(&statements[0].status, StmtStatus::Failed { error } if error.severity == "FATAL" && error.message == "fatal"),
        "{:?}",
        statements[0].status
    );
    h.ok("SELECT 1 AS one").await;
}

fn rowset_states(state: &ConsoleState) -> Vec<(RowsState, u64)> {
    let RunState::Finished { statements, .. } = &state.run else { panic!("not finished") };
    statements
        .iter()
        .flat_map(|s| &s.outputs)
        .filter_map(|o| match o {
            RunOutput::Rows { result, state } => Some((*state, result.rows_seen)),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn an_error_cuts_off_only_the_result_set_it_ends() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    let (state, log) = h.logged("SELECT x FROM (VALUES (1),(2),(0),(3)) v(x) WHERE 10 / x > 0").await;
    assert_eq!(rowset_states(&state), [(RowsState::Failed, 2)]);
    assert_eq!(lines(&log), ["ERROR: Divide by zero error encountered."]);
    assert!(matches!(state.run, RunState::Finished { outcome: RunOutcome::Failed, .. }), "{:?}", state.run);

    let (state, log) = h.logged("SELECT 1 AS a; SELECT 1/0 AS b; SELECT 3 AS c").await;
    assert_eq!(rowset_states(&state), [(RowsState::Complete, 1), (RowsState::Failed, 0), (RowsState::Complete, 1)]);
    assert_eq!(lines(&log), ["rows 1", "ERROR: Divide by zero error encountered.", "rows 1"]);
}

#[tokio::test]
async fn a_nowait_message_streams_while_the_batch_waits_and_survives_cancel() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    let id = h.start("RAISERROR('early', 0, 1) WITH NOWAIT; WAITFOR DELAY '00:00:30'; PRINT 'never'").await.run_id;
    let (tab, events) = (h.tab, &mut h.events);
    let early = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let AppEvent::ConsoleOutput { tab_id, entries } = events.recv().await.unwrap()
                && tab_id == tab
                && let Some(entry) =
                    entries.into_iter().find(|e| e.run_id == id && matches!(&e.body, OutputBody::Notice { .. }))
            {
                return entry.body;
            }
        }
    })
    .await
    .expect("The NOWAIT message did not arrive while the batch waited");
    assert!(matches!(early, OutputBody::Notice { message, .. } if message == "early"));
    h.engine.console_cancel(h.tab).await;
    let state = h.finish(id).await;
    assert!(matches!(state.run, RunState::Finished { outcome: RunOutcome::Cancelled, .. }), "{:?}", state.run);
}

async fn notices_read_slowly(
    events: &mut broadcast::Receiver<AppEvent>,
    until: impl Fn(&AppEvent) -> bool,
) -> (Vec<String>, u64) {
    let (mut notices, mut lagged) = (Vec::new(), 0);
    loop {
        match events.recv().await {
            Ok(event) if until(&event) => return (notices, lagged),
            Ok(AppEvent::ConsoleOutput { entries, .. }) => {
                notices.extend(entries.into_iter().filter_map(|e| match e.body {
                    OutputBody::Notice { message, .. } => Some(message),
                    _ => None,
                }));
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            Ok(_) => {}
            Err(broadcast::error::RecvError::Lagged(n)) => lagged += n,
            Err(e) => panic!("{e:?}"),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_nowait_flood_reaches_a_slow_reader_whole() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    let id = h
        .start("DECLARE @i int = 0; WHILE @i < 20000 BEGIN RAISERROR('%d', 0, 1, @i) WITH NOWAIT; SET @i += 1; END")
        .await
        .run_id;
    let tab = h.tab;
    let (notices, lagged) = notices_read_slowly(&mut h.events, |event| matches!(event, AppEvent::ConsoleChanged(s) if s.tab_id == tab && matches!(s.run, RunState::Finished { run_id, .. } if run_id == id))).await;
    assert_eq!(lagged, 0);
    assert_eq!(notices.len(), 20_000);
    assert!(notices.iter().enumerate().all(|(i, n)| *n == i.to_string()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_export_sends_the_notices_of_its_rerun_in_batches() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    let state = h.ok("DECLARE @i int = 0; WHILE @i < 5000 BEGIN PRINT @i; SET @i += 1; END; SELECT 1 AS a").await;
    let meta = Harness::rowsets(&state)[0].clone();
    let mut events = h.engine.subscribe();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.csv");
    h.engine.result_export(meta.r#ref, ExportFormat::Csv, ExportScope::All, path.display().to_string()).await.unwrap();
    let (mut notices, mut lagged) = (0, 0);
    loop {
        match events.try_recv() {
            Ok(AppEvent::ConsoleOutput { entries, .. }) => {
                notices += entries.iter().filter(|e| matches!(e.body, OutputBody::Notice { .. })).count()
            }
            Ok(_) => {}
            Err(broadcast::error::TryRecvError::Lagged(n)) => lagged += n,
            Err(broadcast::error::TryRecvError::Empty) => break,
            Err(e) => panic!("{e:?}"),
        }
    }
    assert_eq!((notices, lagged), (5_000, 0));
}

#[tokio::test]
async fn sql_server_reports_each_statement_count_like_ssms() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    let (state, log) = h
        .logged("CREATE TABLE #counted (id int); DECLARE @n int = 0; INSERT #counted VALUES (1),(2),(3),(4),(5); SET @n = 1; UPDATE #counted SET id = id + 1; SELECT id FROM #counted WHERE id > 4; DELETE #counted WHERE id = 2; DROP TABLE #counted")
        .await;
    assert_eq!(lines(&log), ["5 rows affected", "5 rows affected", "rows 2", "1 row affected"]);
    assert_eq!(summary(&state), ["5 rows affected", "1 row affected"]);

    let (state, log) = h.logged("DECLARE @i int = 0; WHILE @i < 3 BEGIN PRINT @i; SET @i += 1; END").await;
    assert_eq!(lines(&log), ["NOTICE: 0", "NOTICE: 1", "NOTICE: 2", "Commands completed successfully"]);
    assert_eq!(summary(&state), ["Commands completed successfully"]);

    let (state, _) = h.logged("CREATE TABLE #loop (id int); DECLARE @i int = 0; WHILE @i < 50 BEGIN INSERT #loop VALUES (@i); SET @i += 1; END; DROP TABLE #loop").await;
    assert_eq!(summary(&state), ["1 row affected"]);

    let (state, log) = h.logged("SET NOCOUNT ON; CREATE TABLE #quiet (id int); INSERT #quiet VALUES (1); DROP TABLE #quiet; SET NOCOUNT OFF").await;
    assert_eq!(lines(&log), ["Commands completed successfully"]);
    assert_eq!(summary(&state), ["Commands completed successfully"]);
}

#[tokio::test]
async fn catalogs_details_browse_counts_and_exact_text_use_sql_server() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    let catalog = h.engine.catalog(h.db()).await.unwrap();
    assert_eq!(catalog.database_kind, DatabaseKind::SqlServer);
    assert_eq!(catalog.default_schema.as_deref(), Some("dbo"));
    let relation = catalog
        .schemas
        .iter()
        .find(|s| s.name == "dbo")
        .unwrap()
        .relations
        .iter()
        .find(|r| r.name == "orders")
        .unwrap();
    assert_eq!(relation.qualified_name, "[dbo].[orders]");
    assert!(!relation.columns.iter().find(|c| c.name == "id").unwrap().writable);
    let detail = h.engine.relation_detail(h.db(), relation.oid).await.unwrap();
    assert!(detail.keys.iter().any(|k| matches!(k.kind, KeyKind::Primary)));
    assert!(detail.keys.iter().any(|k| matches!(k.kind, KeyKind::Foreign { .. })));
    assert!(detail.keys.iter().any(|k| matches!(k.kind, KeyKind::Check { .. })));
    assert!(detail.defaults.iter().any(|d| d.column == "status"));
    assert!(detail.indexes.iter().any(|i| i.name == "IX_orders_customer"));
    let request =
        RunRequest::TablePage { schema: "lab".into(), table: "type_zoo".into(), page: 0, query: TableQuery::default() };
    let accepted = h.engine.console_run(h.tab, request, None).await.unwrap();
    let state = h.finish(accepted.run_id).await;
    assert!(matches!(state.run, RunState::Finished { outcome: RunOutcome::Ok, .. }), "{:?}", state.run);
    let sets = Harness::rowsets(&state);
    let result = sets[0];
    assert!(result.columns.iter().all(|c| c.source.is_some()));
    let rows = h.rows(result);
    let cell = |name: &str| {
        let i = result.columns.iter().position(|c| c.name == name).unwrap();
        h.engine.result_cell(result.r#ref, 0, i as u16).unwrap()
    };
    assert_eq!(cell("big").as_deref(), Some("9223372036854775807"));
    assert_eq!(cell("precise").as_deref(), Some("12345678901234567890.123456789012"));
    assert_eq!(cell("money_value").as_deref(), Some("12.3456"));
    assert_eq!(cell("bytes").as_deref(), Some("0x0001FEFF"));
    assert_eq!(cell("datetime_value").as_deref(), Some("2026-10-21 12:34:56.123"));
    assert_eq!(cell("time_value").as_deref(), Some("12:34:56.1234567"));
    assert_eq!(cell("offset_value").as_deref(), Some("2026-10-21 12:34:56.1234567+02:00"));
    assert_eq!(cell("null_value"), None);
    assert_eq!(cell("text_value").unwrap(), "😀".repeat(3000));
    let text_index = result.columns.iter().position(|c| c.name == "text_value").unwrap();
    assert!(rows[0][text_index].as_ref().unwrap().chars().count() < 3000);
    let path = h._dir.path().join("exact.csv");
    h.engine
        .result_export(result.r#ref, ExportFormat::Csv, ExportScope::Loaded, path.display().to_string())
        .await
        .unwrap();
    let csv = std::fs::read_to_string(path).unwrap();
    assert!(csv.contains("12345678901234567890.123456789012"));
    assert!(csv.contains(&"😀".repeat(3000)));
    let query = TableQuery {
        filters: vec![ColumnFilter { column: "a]b".into(), op: FilterOp::Contains, value: "%_[".into() }],
        ..Default::default()
    };
    assert_eq!(h.engine.exact_row_count(h.tab, h.db(), "lab".into(), "awkward names".into(), query).await.unwrap(), 1);
    for injection in ["1=1 DELETE FROM dbo.orders", "1=1; SELECT 2", "1=1 -- ignored"] {
        let query = TableQuery { r#where: injection.into(), ..Default::default() };
        assert!(
            h.engine.exact_row_count(h.tab, h.db(), "dbo".into(), "orders".into(), query).await.is_err(),
            "{injection}"
        );
    }
}

#[tokio::test]
async fn raw_money_sql_variant_and_clr_values_show_exactly() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    let cases = [
        ("CAST(922337203685477.5807 AS money)", "money", Some("922337203685477.5807")),
        ("CAST(-922337203685477.5808 AS money)", "money", Some("-922337203685477.5808")),
        ("CAST(-0.0001 AS money)", "money", Some("-0.0001")),
        ("CAST(0 AS money)", "money", Some("0.0000")),
        ("CAST(214748.3647 AS smallmoney)", "money", Some("214748.3647")),
        ("CAST(-214748.3648 AS smallmoney)", "money", Some("-214748.3648")),
        ("CAST(NULL AS money)", "money", None),
        ("CAST(CAST(1 AS bit) AS sql_variant)", "sql_variant", Some("1")),
        ("CAST(CAST(255 AS tinyint) AS sql_variant)", "sql_variant", Some("255")),
        ("CAST(CAST(-7 AS smallint) AS sql_variant)", "sql_variant", Some("-7")),
        ("CAST(5 AS sql_variant)", "sql_variant", Some("5")),
        ("CAST(CAST(9223372036854775807 AS bigint) AS sql_variant)", "sql_variant", Some("9223372036854775807")),
        ("CAST(CAST(1.5 AS real) AS sql_variant)", "sql_variant", Some("1.5")),
        ("CAST(CAST(1.5 AS float) AS sql_variant)", "sql_variant", Some("1.5")),
        ("CAST(CAST(-12.50 AS decimal(10,2)) AS sql_variant)", "sql_variant", Some("-12.50")),
        (
            "CAST(CAST(12345678901234567890.123456789012 AS numeric(38,12)) AS sql_variant)",
            "sql_variant",
            Some("12345678901234567890.123456789012"),
        ),
        ("CAST(CAST(922337203685477.5807 AS money) AS sql_variant)", "sql_variant", Some("922337203685477.5807")),
        ("CAST(CAST(-1.5 AS smallmoney) AS sql_variant)", "sql_variant", Some("-1.5000")),
        ("CAST(CAST('2024-01-02' AS date) AS sql_variant)", "sql_variant", Some("2024-01-02")),
        ("CAST(CAST('03:04:05.123' AS time(3)) AS sql_variant)", "sql_variant", Some("03:04:05.1230000")),
        (
            "CAST(CAST('2024-01-02T03:04:05.123' AS datetime) AS sql_variant)",
            "sql_variant",
            Some("2024-01-02 03:04:05.123"),
        ),
        (
            "CAST(CAST('2024-01-02T03:04:00' AS smalldatetime) AS sql_variant)",
            "sql_variant",
            Some("2024-01-02 03:04:00"),
        ),
        (
            "CAST(CAST('2024-01-02T03:04:05.1234567' AS datetime2(7)) AS sql_variant)",
            "sql_variant",
            Some("2024-01-02 03:04:05.1234567"),
        ),
        (
            "CAST(CAST('2024-01-02T03:04:05.123+02:00' AS datetimeoffset(3)) AS sql_variant)",
            "sql_variant",
            Some("2024-01-02 03:04:05.1230000+02:00"),
        ),
        (
            "CAST(CAST('abcd1234-5678-90ab-cdef-1234567890ab' AS uniqueidentifier) AS sql_variant)",
            "sql_variant",
            Some("abcd1234-5678-90ab-cdef-1234567890ab"),
        ),
        ("CAST(CAST('abc' AS char(3)) AS sql_variant)", "sql_variant", Some("abc")),
        ("CAST(CAST('abc' AS varchar(3)) AS sql_variant)", "sql_variant", Some("abc")),
        ("CAST(CAST(N'世' AS nchar(1)) AS sql_variant)", "sql_variant", Some("世")),
        ("CAST(N'Hej 世界 😀' AS sql_variant)", "sql_variant", Some("Hej 世界 😀")),
        ("CAST(CAST(0x00FF AS binary(2)) AS sql_variant)", "sql_variant", Some("0x00FF")),
        ("CAST(CAST(0x00FF AS varbinary(2)) AS sql_variant)", "sql_variant", Some("0x00FF")),
        ("CAST(NULL AS sql_variant)", "sql_variant", None),
        ("hierarchyid::GetRoot()", "clr", Some("0x")),
        ("hierarchyid::Parse('/1/')", "clr", Some("0x58")),
        ("CAST(NULL AS hierarchyid)", "clr", None),
        ("geography::Point(1, 1, 4326)", "clr", Some("0xE6100000010C000000000000F03F000000000000F03F")),
    ];
    for (expression, display, value) in cases {
        let state = h.ok(&format!("SELECT {expression} AS value, 7 AS after_value")).await;
        let result = Harness::rowsets(&state)[0];
        assert_eq!(result.columns[0].r#type.display, display, "{expression}");
        assert_eq!(h.rows(result)[0], vec![value.map(str::to_owned), Some("7".into())], "{expression}");
    }
    let state = h.ok("SELECT * FROM lab.type_zoo; SELECT SUM(money_value) AS total FROM lab.type_zoo; SELECT * FROM sys.configurations").await;
    let results = Harness::rowsets(&state);
    let money = results[0].columns.iter().position(|c| c.name == "money_value").unwrap();
    assert_eq!(h.rows(results[0])[0][money].as_deref(), Some("12.3456"));
    assert_eq!(h.rows(results[1])[0][0].as_deref(), Some("12.3456"));
    assert!(!h.rows(results[2]).is_empty());
    let state = h.ok("SELECT SERVERPROPERTY('ProductVersion') AS version").await;
    assert!(h.rows(Harness::rowsets(&state)[0])[0][0].as_deref().is_some_and(|v| v.starts_with("16.")));

    let table = format!("l1_exact_{}", uuid::Uuid::new_v4().simple());
    h.ok(&format!(
        "CREATE TABLE dbo.[{table}] (id int PRIMARY KEY, v sql_variant, h hierarchyid, g geography, m money)"
    ))
    .await;
    let outcome = AssertUnwindSafe(async {
        h.ok(&format!("INSERT INTO dbo.[{table}] VALUES (1, 5, hierarchyid::Parse('/1/'), geography::Point(1, 1, 4326), 922337203685477.5807)")).await;
        h.engine.refresh_catalog(h.db()).await.unwrap();
        let expected = vec![Some("1".to_owned()), Some("5".into()), Some("0x58".into()), Some("0xE6100000010C000000000000F03F000000000000F03F".into()), Some("922337203685477.5807".into())];
        let page = RunRequest::TablePage { schema: "dbo".into(), table: table.clone(), page: 0, query: TableQuery::default() };
        let accepted = h.engine.console_run(h.tab, page, None).await.unwrap();
        let state = h.finish(accepted.run_id).await;
        assert!(matches!(state.run, RunState::Finished { outcome: RunOutcome::Ok, .. }), "{:?}", state.run);
        let meta = Harness::rowsets(&state)[0].clone();
        assert_eq!(h.rows(&meta)[0], expected);
        let (_, csv) = h.export(&meta, ExportFormat::Csv, ExportScope::Loaded, "exact.csv").await;
        assert!(csv.contains("922337203685477.5807") && csv.contains("0xE6100000010C"), "{csv}");
        let (_, script) = h.export(&meta, ExportFormat::SqlInsert, ExportScope::Loaded, "exact.sql").await;
        h.ok(&format!("DELETE FROM dbo.[{table}]")).await;
        h.ok(&script).await;
        let state = h.ok(&format!("SELECT id, v, h, g, m FROM dbo.[{table}]")).await;
        assert_eq!(h.rows(Harness::rowsets(&state)[0])[0], expected, "the INSERT export replays");
    })
    .catch_unwind()
    .await;
    h.ok(&format!("DROP TABLE dbo.[{table}]")).await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test]
async fn results_before_a_server_error_survive() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    let state = h.run("SELECT 5 AS partial; RAISERROR('expected failure',16,1)").await;
    assert_eq!(h.rows(Harness::rowsets(&state)[0])[0][0].as_deref(), Some("5"));
    let RunState::Finished { statements, .. } = &state.run else { unreachable!() };
    assert!(
        matches!(&statements[0].status,StmtStatus::Failed{error} if error.sqlstate.is_none() && error.detail.as_ref().is_some_and(|s|s.contains("50000")))
    );
}

#[tokio::test]
async fn transactions_are_observed_and_commit_button_finishes_all_nesting() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    h.ok("CREATE TABLE #tx (id int)").await;
    let state = h.ok("BEGIN TRANSACTION; BEGIN TRANSACTION; INSERT INTO #tx VALUES (1)").await;
    assert_eq!((state.txn, state.txn_depth), (TxnState::Open, None));
    let state = h.ok("COMMIT TRANSACTION").await;
    assert_eq!((state.txn, state.txn_depth), (TxnState::Open, None));
    let state = h.run("RAISERROR('recoverable',16,1)").await;
    assert_eq!((state.txn, state.txn_depth), (TxnState::Open, None));
    h.engine.console_rollback(h.tab).await.unwrap();
    let state = h.ok("SELECT COUNT_BIG(*) FROM #tx").await;
    assert_eq!(h.rows(Harness::rowsets(&state)[0])[0][0].as_deref(), Some("0"));
    h.ok("BEGIN TRANSACTION; BEGIN TRANSACTION; INSERT INTO #tx VALUES (2)").await;
    h.engine.console_commit(h.tab, None).await.unwrap();
    let state = h.ok("SELECT @@TRANCOUNT, COUNT_BIG(*) FROM #tx").await;
    assert_eq!(h.rows(Harness::rowsets(&state)[0])[0], vec![Some("0".into()), Some("1".into())]);
    h.ok("SET XACT_ABORT ON; BEGIN TRANSACTION").await;
    let state = h.run("BEGIN TRY SELECT CONVERT(int,N'bad') END TRY BEGIN CATCH SELECT XACT_STATE() END CATCH").await;
    let sets = Harness::rowsets(&state);
    assert_eq!(h.rows(sets.last().unwrap())[0][0].as_deref(), Some("-1"));
    assert!(
        matches!(&state.run, RunState::Finished { outcome: RunOutcome::Failed, statements, .. } if matches!(&statements[0].status, StmtStatus::Failed { error } if error.detail.as_ref().is_some_and(|d| d.contains("3998"))))
    );
    assert_eq!((state.txn, state.txn_depth), (TxnState::Idle, Some(0)));
    let state = h.ok("SELECT @@TRANCOUNT").await;
    assert_eq!(state.txn, TxnState::Idle);
}

#[tokio::test]
async fn every_batch_is_validated_before_any_execution_and_dynamic_use_stays_scoped() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    let rejected = h
        .engine
        .console_run(
            h.tab,
            RunRequest::Script {
                text: "CREATE TABLE #must_not_exist(id int)\nGO\nUSE reporting".into(),
                target: RunTarget::All,
                mode: RunMode::Run,
                params: Vec::new(),
                cells: Vec::new(),
            },
            None,
        )
        .await;
    assert!(matches!(rejected, Err(RunError::Unsupported { .. })));
    let state = h.ok("SELECT OBJECT_ID('tempdb..#must_not_exist')").await;
    assert_eq!(h.rows(Harness::rowsets(&state)[0])[0][0], None);
    let state = h.run("EXEC ('USE reporting; SELECT 1')").await;
    // Dynamic EXEC scopes USE locally. The tab must still target shop after it returns.
    assert!(matches!(state.session, SessionState::Open { .. }));
    let state = h.ok("SELECT DB_NAME()").await;
    assert_eq!(h.rows(Harness::rowsets(&state)[0])[0][0].as_deref(), Some("shop"));
}

#[tokio::test]
async fn cancelling_waitfor_retires_the_session_and_stops_the_server_request() {
    for sql in ["WAITFOR DELAY '00:00:30'; SELECT 99", "SELECT 1 AS first; WAITFOR DELAY '00:00:30'; SELECT 99"] {
        let mut h = Harness::open(TxnMode::AutoCommit).await;
        // SQL Server gives a closed session's session_id to later logins; connection_id is never reused.
        let warm = h
            .ok("BEGIN TRANSACTION; SELECT connection_id FROM sys.dm_exec_connections WHERE session_id = @@SPID")
            .await;
        let connection = h.rows(Harness::rowsets(&warm)[0])[0][0].clone().unwrap();
        let id = h.start(sql).await.run_id;
        tokio::time::sleep(Duration::from_millis(150)).await;
        tokio::time::timeout(Duration::from_secs(1), h.engine.console_cancel(h.tab)).await.unwrap();
        let state = h.finish(id).await;
        assert!(matches!(state.run, RunState::Finished { outcome: RunOutcome::Cancelled, .. }), "{:?}", state.run);
        assert!(
            matches!(state.session, SessionState::Lost { transaction: TransactionLoss::Unknown, .. }),
            "{:?}",
            state.session
        );
        let state =
            h.ok(&format!("SELECT COUNT_BIG(*) FROM sys.dm_exec_requests WHERE connection_id = '{connection}'")).await;
        assert_eq!(h.rows(Harness::rowsets(&state)[0])[0][0].as_deref(), Some("0"));
    }
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    let id = h.start("WAITFOR DELAY '00:00:30'").await.run_id;
    h.engine.console_cancel(h.tab).await;
    let state = h.finish(id).await;
    assert!(matches!(state.session, SessionState::Lost { .. }), "{:?}", state.session);
}

#[tokio::test]
async fn native_writes_keep_precision_defaults_trigger_results_and_metadata() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    let table = format!("l1_edit_{}", uuid::Uuid::new_v4().simple());
    let trigger = format!("{table}_trigger");
    h.ok(&format!("CREATE TABLE dbo.[{table}] (id int IDENTITY PRIMARY KEY, [a]]b] nvarchar(200), n decimal(38,12), bytes varbinary(max), price money, state int DEFAULT 7, computed AS (state+1), version rowversion)")).await;
    let outcome = AssertUnwindSafe(async {
        h.ok(&format!("CREATE TRIGGER dbo.[{trigger}] ON dbo.[{table}] AFTER INSERT,UPDATE,DELETE AS BEGIN SET NOCOUNT ON; SELECT 123 AS trigger_row; END")).await;
        h.engine.refresh_catalog(h.db()).await.unwrap();
        let catalog = h.engine.catalog(h.db()).await.unwrap();
        let relation = catalog.schemas.iter().flat_map(|s| &s.relations).find(|r| r.name == table).unwrap();
        for col in ["id", "computed", "version"] {
            assert!(!relation.columns.iter().find(|c| c.name == col).unwrap().writable);
        }
        let detail = h.engine.relation_detail(h.db(), relation.oid).await.unwrap();
        assert!(matches!(&detail.triggers[0].implementation,TriggerImplementation::SqlModule {definition:Some(definition)} if definition.contains("NOCOUNT")));
        h.ok(&format!("INSERT INTO dbo.[{table}] ([a]]b],n,bytes,price) VALUES(N'O''Neil 世界 😀 %_[',12345678901234567890.123456789012,0x0001FEFF,922337203685477.5807)")).await;
        let state = h.ok(&format!("SELECT id,[a]]b],n,bytes,CAST(price AS decimal(19,4)),state,computed FROM dbo.[{table}]")).await;
        let rows = h.rows(Harness::rowsets(&state)[0]);
        assert_eq!(
            rows[0],
            vec![
                Some("1".into()),
                Some("O'Neil 世界 😀 %_[".into()),
                Some("12345678901234567890.123456789012".into()),
                Some("0x0001FEFF".into()),
                Some("922337203685477.5807".into()),
                Some("7".into()),
                Some("8".into())
            ]
        );
        let state = h.ok(&format!("UPDATE dbo.[{table}] SET [a]]b]=N'changed',n=NULL WHERE id=1; SELECT [a]]b],n FROM dbo.[{table}]")).await;
        let sets = Harness::rowsets(&state);
        assert_eq!(sets.len(), 2);
        assert_eq!(h.rows(sets[0]), vec![vec![Some("123".into())]]);
        assert_eq!(h.rows(sets[1]), vec![vec![Some("changed".into()), None]]);
        h.ok(&format!("BEGIN TRANSACTION; UPDATE dbo.[{table}] SET [a]]b]=N'uncommitted' WHERE id=1")).await;
        h.engine.console_rollback(h.tab).await.unwrap();
        let state = h.ok(&format!("SELECT [a]]b] FROM dbo.[{table}]")).await;
        assert_eq!(h.rows(Harness::rowsets(&state)[0])[0][0].as_deref(), Some("changed"));
        h.ok(&format!("DELETE FROM dbo.[{table}] WHERE id=1")).await;
        let state = h.ok(&format!("SELECT COUNT_BIG(*) FROM dbo.[{table}]")).await;
        assert_eq!(h.rows(Harness::rowsets(&state)[0])[0][0].as_deref(), Some("0"));
    }).catch_unwind().await;
    h.ok(&format!("DROP TABLE dbo.[{table}]")).await;
    let state = h.ok(&format!("SELECT COUNT_BIG(*) FROM sys.objects WHERE name IN (N'{table}',N'{trigger}')")).await;
    assert_eq!(h.rows(Harness::rowsets(&state)[0]), vec![vec![Some("0".into())]]);
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test]
async fn connection_kind_is_immutable_and_history_records_sql_server() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    let old = h.engine.boot().connections.into_iter().find(|c| c.id == h.connection).unwrap();
    let changed = ConnectionDraft {
        database_kind: DatabaseKind::Postgres,
        id: Some(h.connection),
        name: old.name,
        host: old.host,
        port: 5432,
        database: old.database,
        user: old.user,
        ssl_mode: SslMode::Disable,
        ca_cert_path: None,
        password: PasswordInput::Clear,
        color: None,
        ssh: None,
        environment: None,
        sql_server_auth: None,
    };
    assert!(matches!(h.engine.save_connection(changed).await, Err(SaveError::Invalid { .. })));
    assert!(h.engine.boot().connections.iter().find(|c| c.id == h.connection).unwrap().has_password);
    h.ok("SELECT 1 AS history_example").await;
    let history = h.engine.history(HistoryQuery {
        text: Some("history_example".into()),
        connection_id: None,
        before_id: None,
        limit: 10,
    });
    assert_eq!(history.entries[0].database_kind, DatabaseKind::SqlServer);
    let page = h
        .engine
        .history_browse(HistoryBrowseQuery {
            text: Some("history_example".into()),
            filter: HistoryFilter::All,
            before_id: None,
            limit: 10,
            revision: None,
        })
        .unwrap();
    assert_eq!(page.groups[0].database_kind, DatabaseKind::SqlServer);
    let group = h
        .engine
        .history_group(HistoryGroupQuery { id: page.groups[0].id, before_id: None, limit: 10, revision: page.revision })
        .unwrap();
    assert_eq!(group.database_kind, DatabaseKind::SqlServer);
}

#[tokio::test]
async fn clr_columns_stay_in_the_catalog_and_browse_as_hexadecimal() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    let table = format!("l1_clr_{}", uuid::Uuid::new_v4().simple());
    h.ok(&format!("CREATE TABLE dbo.[{table}] (id int PRIMARY KEY, path hierarchyid, location geography); INSERT INTO dbo.[{table}] VALUES (1, hierarchyid::GetRoot(), geography::Point(1, 1, 4326))")).await;
    h.engine.refresh_catalog(h.db()).await.unwrap();
    let catalog = h.engine.catalog(h.db()).await.unwrap();
    let relation = catalog.schemas.iter().flat_map(|s| &s.relations).find(|r| r.name == table).unwrap();
    assert_eq!(relation.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["id", "path", "location"]);
    for column in &relation.columns[1..] {
        assert_eq!(column.category, TypeCategory::Binary);
    }
    assert_eq!(relation.columns[1].type_display, "hierarchyid");
    assert_eq!(relation.columns[2].type_display, "geography");
    let run = h
        .engine
        .console_run(
            h.tab,
            RunRequest::TablePage { schema: "dbo".into(), table: table.clone(), page: 0, query: TableQuery::default() },
            None,
        )
        .await
        .unwrap();
    let state = h.finish(run.run_id).await;
    let meta = Harness::rowsets(&state)[0];
    assert_eq!(meta.columns[1].r#type.display, "hierarchyid");
    assert_eq!(
        h.rows(meta)[0],
        [Some("1".to_owned()), Some("0x".into()), Some("0xE6100000010C000000000000F03F000000000000F03F".into())]
    );
    let filters = vec![ColumnFilter { column: "location".into(), op: FilterOp::Contains, value: "E6".into() }];
    let refused = h
        .engine
        .console_run(
            h.tab,
            RunRequest::TablePage {
                schema: "dbo".into(),
                table: table.clone(),
                page: 0,
                query: TableQuery { filters, ..Default::default() },
            },
            None,
        )
        .await;
    assert!(matches!(refused, Err(RunError::BadFilter)), "{refused:?}");
    h.ok(&format!("DROP TABLE dbo.[{table}]")).await;
}

#[tokio::test]
async fn database_catalogs_stay_scoped_and_header_sort_sql_runs_with_bracket_names() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    let shop = h.engine.catalog(h.db()).await.unwrap();
    let reporting_db = DatabaseRef { connection_id: h.connection, database: "reporting".into() };
    h.engine.edit_view(h.connection, ViewEdit::Database { name: "reporting".into(), shown: true }).await.unwrap();
    let reporting = h.engine.catalog(reporting_db.clone()).await.unwrap();
    let customers = |catalog: &Catalog| {
        catalog
            .schemas
            .iter()
            .flat_map(|s| &s.relations)
            .find(|r| r.name == "customers")
            .unwrap()
            .columns
            .iter()
            .map(|c| c.name.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(customers(&shop), ["id", "name", "region"]);
    assert_eq!(customers(&reporting), ["id", "label"]);
    let oid = reporting.schemas.iter().flat_map(|s| &s.relations).find(|r| r.name == "customers").unwrap().oid;
    assert_eq!(h.engine.relation_detail(reporting_db.clone(), oid).await.unwrap().db, reporting_db);
    let script = h.engine.table_script(h.db(), "lab", "awkward names").unwrap();
    assert_eq!(script.text, "SELECT * FROM [lab].[awkward names]");
    assert_eq!(script.accepted_texts, ["SELECT * FROM [lab].[awkward names]"]);
    let sorted = h
        .engine
        .table_order(
            h.db(),
            "lab",
            "awkward names",
            "",
            TableOrderEdit::Cycle { column: "a]b".into(), additive: false },
        )
        .unwrap();
    assert_eq!(sorted.sql, "[a]]b] ASC");
    let descending = h
        .engine
        .table_order(
            h.db(),
            "lab",
            "awkward names",
            &sorted.sql,
            TableOrderEdit::Cycle { column: "a]b".into(), additive: false },
        )
        .unwrap();
    assert_eq!(descending.sql, "[a]]b] DESC");
    let advanced = "COALESCE([a]]b], N'') DESC";
    let inspected = h
        .engine
        .table_order(
            h.db(),
            "lab",
            "awkward names",
            advanced,
            TableOrderEdit::Cycle { column: "a]b".into(), additive: false },
        )
        .unwrap();
    assert_eq!(inspected.sql, advanced);
    assert!(inspected.keys.is_none());
    let accepted = h
        .engine
        .console_run(
            h.tab,
            RunRequest::TablePage {
                schema: "lab".into(),
                table: "awkward names".into(),
                page: 0,
                query: TableQuery { order_by: descending.sql, ..TableQuery::default() },
            },
            None,
        )
        .await
        .unwrap();
    let state = h.finish(accepted.run_id).await;
    assert!(matches!(state.run, RunState::Finished { outcome: RunOutcome::Ok, .. }), "{:?}", state.run);
    let set = Harness::rowsets(&state)[0];
    assert_eq!(h.rows(set)[0][1].as_deref(), Some("100%_[value]"));
    assert_eq!(set.db, h.db());
}

#[tokio::test]
async fn system_schema_views_keep_lossless_opaque_ids_in_catalog_and_detail() {
    let h = Harness::open(TxnMode::AutoCommit).await;
    h.engine
        .edit_view(h.connection, ViewEdit::ShowSystemSchemas { database: "shop".into(), show: true })
        .await
        .unwrap();
    for name in ["sys", "INFORMATION_SCHEMA"] {
        h.engine
            .edit_view(h.connection, ViewEdit::Schema { database: "shop".into(), name: name.into(), shown: true })
            .await
            .unwrap();
    }
    h.engine.refresh_catalog(h.db()).await.unwrap();
    let catalog = h.engine.catalog(h.db()).await.unwrap();
    for name in ["sys", "INFORMATION_SCHEMA"] {
        let schema = catalog.schemas.iter().find(|s| s.name == name).unwrap();
        let view = schema
            .relations
            .iter()
            .find(|r| r.oid > i32::MAX as u32 && !r.columns.is_empty())
            .expect("System views with negative native IDs must load");
        assert_eq!(view.kind, RelKind::View);
        let detail = h.engine.relation_detail(h.db(), view.oid).await.unwrap();
        assert_eq!(detail.oid, view.oid);
        assert_eq!(detail.db, h.db());
    }
}

#[tokio::test]
async fn ddl_committed_before_a_later_batch_error_refreshes_the_catalog() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    h.engine.catalog(h.db()).await.unwrap();
    let table = format!("l1_partial_{}", uuid::Uuid::new_v4().simple());
    let state = h.run(&format!("CREATE TABLE dbo.[{table}] (id int); RAISERROR('after committed DDL',16,1)")).await;
    assert!(matches!(state.run, RunState::Finished { outcome: RunOutcome::Failed, .. }));
    assert_eq!(state.txn, TxnState::Idle);
    let refreshed = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let catalog = h.engine.catalog(h.db()).await.unwrap();
            if catalog.schemas.iter().flat_map(|s| &s.relations).any(|r| r.name == table) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    h.ok(&format!("DROP TABLE dbo.[{table}]")).await;
    assert!(refreshed.is_ok(), "The catalog stayed stale after the batch committed DDL before its error");
}

impl Harness {
    fn console(&self) -> ConsoleState {
        self.engine.boot().consoles.into_iter().find(|c| c.tab_id == self.tab).expect("console")
    }

    async fn export(
        &self,
        meta: &ResultMeta,
        format: ExportFormat,
        scope: ExportScope,
        name: &str,
    ) -> (ExportSummary, String) {
        let path = self._dir.path().join(name);
        let summary = self.engine.result_export(meta.r#ref, format, scope, path.display().to_string()).await.unwrap();
        (summary, std::fs::read_to_string(path).unwrap())
    }
}

#[tokio::test]
async fn all_rows_exports_the_chosen_result_set_of_a_batch_and_keeps_the_session() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    h.engine.set_settings(Settings { max_rows: 5, ..h.engine.settings() });
    let state = h
        .ok("SELECT N'first' AS head; SELECT TOP (40) ROW_NUMBER() OVER (ORDER BY (SELECT NULL)) AS n FROM sys.all_objects ORDER BY n; SELECT N'last' AS tail")
        .await;
    let sets = Harness::rowsets(&state);
    assert_eq!(sets.len(), 3);
    assert_eq!((sets[1].row_count, sets[1].rows_seen, &sets[1].all_rows), (5, 40, &AllRows::Statement));
    let pid = match state.session {
        SessionState::Open { backend_pid } => backend_pid,
        other => panic!("{other:?}"),
    };

    let (summary, csv) = h.export(sets[1], ExportFormat::Csv, ExportScope::All, "middle.csv").await;
    let expected: String =
        std::iter::once("n".to_owned()).chain((1..=40).map(|n| n.to_string())).map(|l| l + "\r\n").collect();
    assert_eq!((summary.rows, csv), (40, expected), "only the second result set, every row");
    let (_, tail) = h.export(sets[2], ExportFormat::Json, ExportScope::All, "tail.json").await;
    assert_eq!(tail, "[\n  {\"tail\":\"last\"}\n]\n");
    let after = h.console();
    assert_eq!((after.work, after.export, after.txn), (None, None, TxnState::Idle));
    assert!(
        matches!(after.session, SessionState::Open { backend_pid } if backend_pid == pid),
        "the export ran on the tab's own session and kept it"
    );
}

#[tokio::test]
async fn all_rows_refuses_a_batch_that_controls_a_transaction() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    let state = h.ok("BEGIN TRANSACTION; SELECT 1 AS x; COMMIT").await;
    let meta = Harness::rowsets(&state)[0].clone();
    assert!(
        matches!(&meta.all_rows, AllRows::Refused { reason } if reason.contains("transaction")),
        "{:?}",
        meta.all_rows
    );
    let path = h._dir.path().join("never.csv");
    assert!(matches!(
        h.engine.result_export(meta.r#ref, ExportFormat::Csv, ExportScope::All, path.display().to_string()).await,
        Err(ExportError::NotRerunnable)
    ));
    assert!(!path.exists());
}

#[tokio::test]
async fn sql_server_inserts_round_trip_every_value_under_another_dateformat() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    let source = format!("l1_export_src_{}", uuid::Uuid::new_v4().simple());
    let copy = format!("l1_export_copy_{}", uuid::Uuid::new_v4().simple());
    let columns = "id int, big bigint, exact decimal(38,12), flag bit, label nvarchar(50), ascii varchar(20), old datetime, small smalldatetime, day date, \
                   tick time(7), modern datetime2(7), moment datetimeoffset(7), uid uniqueidentifier, bytes varbinary(16), ratio float, single real";
    h.ok(&format!("CREATE TABLE dbo.[{source}] ({columns}); CREATE TABLE dbo.[{copy}] ({columns})")).await;
    let outcome = AssertUnwindSafe(async {
        h.ok(&format!(
            "INSERT INTO dbo.[{source}] VALUES \
             (1, 9223372036854775807, 12345678901234567890123456.789012345678, 1, N'O''Brien 😀 | x', 'plain', '2024-01-02T03:04:05.123', '2024-01-02T03:04:00', '2024-01-02', \
              '03:04:05.1234567', '2024-01-02T03:04:05.1234567', '2024-01-02T03:04:05.1234567+02:00', 'abcd1234-5678-90ab-cdef-1234567890ab', 0x0001FEFF, 1e300, 1.5), \
             (2, -1, -0.000000000001, 0, N'', '', NULL, NULL, NULL, NULL, NULL, NULL, NULL, 0x, -2.5e-300, NULL), \
             (3, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL)"
        ))
        .await;
        let state = h.ok(&format!("SELECT * FROM dbo.[{source}] ORDER BY id")).await;
        let meta = Harness::rowsets(&state)[0].clone();
        let (summary, script) = h.export(&meta, ExportFormat::SqlInsert, ExportScope::All, "values.sql").await;
        assert_eq!(summary.rows, 3);
        assert!(script.starts_with("INSERT INTO target_table ([id], [big], "), "{script}");
        h.ok("SET DATEFORMAT dmy").await;
        h.ok(&script.replace("target_table", &format!("dbo.[{copy}]"))).await;
        let differ = |a: &str, b: &str| format!("SELECT COUNT_BIG(*) FROM (SELECT * FROM dbo.[{a}] EXCEPT SELECT * FROM dbo.[{b}]) d");
        let state = h.ok(&format!("{}; {}; SELECT COUNT_BIG(*) FROM dbo.[{copy}]", differ(&source, &copy), differ(&copy, &source))).await;
        let counts: Vec<_> = Harness::rowsets(&state).iter().map(|s| h.rows(s)[0][0].clone()).collect();
        assert_eq!(counts, [Some("0".into()), Some("0".into()), Some("3".into())], "the script rebuilds every row exactly");
    })
    .catch_unwind()
    .await;
    h.ok(&format!("DROP TABLE dbo.[{source}]; DROP TABLE dbo.[{copy}]")).await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test]
async fn esc_during_a_sql_server_all_rows_export_resets_the_session_and_leaves_the_file() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    let state = h.ok("SELECT 1 AS n UNION ALL SELECT 2; WAITFOR DELAY '00:00:02'; SELECT 3 AS done").await;
    let meta = Harness::rowsets(&state)[0].clone();
    let out = h._dir.path().join("out");
    std::fs::create_dir(&out).unwrap();
    let path = out.join("n.csv");
    std::fs::write(&path, "old").unwrap();
    let export = {
        let (engine, path) = (h.engine.clone(), path.display().to_string());
        tokio::spawn(async move { engine.result_export(meta.r#ref, ExportFormat::Csv, ExportScope::All, path).await })
    };
    tokio::time::timeout(Duration::from_secs(10), async {
        while h.console().work != Some(ConsoleWork::Export) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the export takes the session");
    tokio::time::sleep(Duration::from_millis(500)).await;
    h.engine.console_cancel(h.tab).await;

    assert!(matches!(export.await.unwrap(), Err(ExportError::Cancelled)));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "old");
    assert_eq!(std::fs::read_dir(&out).unwrap().count(), 1, "no temp file left beside it");
    let after = h.console();
    assert_eq!((after.work, after.export), (None, None));
    assert!(
        matches!(&after.session, SessionState::Lost { reason, .. } if reason.contains("Cancelled")),
        "{:?}",
        after.session
    );
    h.ok("SELECT 1").await;
}

#[tokio::test]
async fn insert_exports_replay_identity_and_leave_out_computed_and_rowversion_columns() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    let source = format!("l1_export_ident_{}", uuid::Uuid::new_v4().simple());
    let copy = format!("l1_export_ident_copy_{}", uuid::Uuid::new_v4().simple());
    let columns =
        "id int IDENTITY PRIMARY KEY, label nvarchar(20) NOT NULL, doubled AS (LEN(label) * 2), ver rowversion";
    h.ok(&format!("CREATE TABLE dbo.[{source}] ({columns}); CREATE TABLE dbo.[{copy}] ({columns})")).await;
    let outcome = AssertUnwindSafe(async {
        h.ok(&format!("INSERT INTO dbo.[{source}] (label) VALUES (N'a'), (N'bb'), (N'ccc'); DELETE FROM dbo.[{source}] WHERE id = 2")).await;
        h.engine.refresh_catalog(h.db()).await.unwrap();
        let page = h.engine.console_run(h.tab, RunRequest::TablePage { schema: "dbo".into(), table: source.clone(), page: 0, query: TableQuery::default() }, None).await.unwrap();
        let state = h.finish(page.run_id).await;
        let meta = Harness::rowsets(&state)[0].clone();
        let (summary, script) = h.export(&meta, ExportFormat::SqlInsert, ExportScope::All, "ident.sql").await;
        assert_eq!(summary.rows, 2);
        let table = format!("[dbo].[{source}]");
        assert_eq!(
            script,
            format!("SET IDENTITY_INSERT {table} ON;\nINSERT INTO {table} ([id], [label]) VALUES (1, N'a');\nINSERT INTO {table} ([id], [label]) VALUES (3, N'ccc');\nSET IDENTITY_INSERT {table} OFF;\n")
        );
        h.ok(&script.replace(&table, &format!("dbo.[{copy}]"))).await;
        let differ = |a: &str, b: &str| format!("SELECT COUNT_BIG(*) FROM (SELECT id, label, doubled FROM dbo.[{a}] EXCEPT SELECT id, label, doubled FROM dbo.[{b}]) d");
        let state = h.ok(&format!("{}; {}; SELECT COUNT_BIG(*) FROM dbo.[{copy}]", differ(&source, &copy), differ(&copy, &source))).await;
        let counts: Vec<_> = Harness::rowsets(&state).iter().map(|s| h.rows(s)[0][0].clone()).collect();
        assert_eq!(counts, [Some("0".into()), Some("0".into()), Some("2".into())], "the script rebuilds every row with its identity value");
    })
    .catch_unwind()
    .await;
    h.ok(&format!("DROP TABLE dbo.[{source}]; DROP TABLE dbo.[{copy}]")).await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

async fn set_environment(h: &Harness, environment: Option<Environment>) {
    let old = h.engine.boot().connections.into_iter().find(|c| c.id == h.connection).unwrap();
    let draft = ConnectionDraft {
        database_kind: DatabaseKind::SqlServer,
        id: Some(h.connection),
        name: old.name,
        host: old.host,
        port: old.port,
        database: old.database,
        user: old.user,
        ssl_mode: old.ssl_mode,
        ca_cert_path: None,
        password: PasswordInput::Keep,
        color: None,
        ssh: None,
        environment,
        sql_server_auth: old.sql_server_auth,
    };
    h.engine.save_connection(draft).await.unwrap();
}

async fn asked_to_run(h: &Harness, text: &str, confirmed: Option<WriteDigest>) -> WriteConfirmation {
    match h
        .engine
        .console_run(
            h.tab,
            RunRequest::Script {
                text: text.into(),
                target: RunTarget::All,
                mode: RunMode::Run,
                params: Vec::new(),
                cells: Vec::new(),
            },
            confirmed,
        )
        .await
    {
        Err(RunError::ConfirmWrites { confirmation }) => confirmation,
        other => panic!("{text}: expected ConfirmWrites, got {other:?}"),
    }
}

#[tokio::test]
async fn a_batch_that_runs_exec_on_production_waits_for_the_digest() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    set_environment(&h, Some(Environment::Production)).await;
    let text = "CREATE TABLE #p1_guard(id int)\nGO\nSELECT 1\nGO\nEXEC sp_who";
    let request = || RunRequest::Script {
        text: text.into(),
        target: RunTarget::All,
        mode: RunMode::Run,
        params: Vec::new(),
        cells: Vec::new(),
    };
    let asked = asked_to_run(&h, text, None).await;
    assert_eq!((asked.connection.as_str(), asked.database.as_str(), asked.sends.count), ("SQL Server test", "shop", 2));
    assert_eq!(asked.sends.statements, ["CREATE TABLE #p1_guard(id int)", "EXEC sp_who"]);
    let state = h.ok("SELECT OBJECT_ID('tempdb..#p1_guard')").await;
    assert_eq!(h.rows(Harness::rowsets(&state)[0])[0][0], None, "a refused batch runs nothing");

    let id = h.engine.console_run(h.tab, request(), Some(asked.digest)).await.unwrap().run_id;
    let state = h.finish(id).await;
    assert!(matches!(state.run, RunState::Finished { outcome: RunOutcome::Ok, .. }), "{:?}", state.run);
    let state = h.ok("SELECT OBJECT_ID('tempdb..#p1_guard')").await;
    assert!(h.rows(Harness::rowsets(&state)[0])[0][0].is_some());
}

#[tokio::test]
async fn every_batch_on_production_asks_while_the_transaction_holds_writes_that_ran_before() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    h.ok("CREATE TABLE #held (id int)").await;
    assert_eq!(h.ok("BEGIN TRAN; INSERT INTO #held VALUES (1)").await.txn, TxnState::Open);
    set_environment(&h, Some(Environment::Production)).await;
    for text in
        ["IF @@TRANCOUNT > 0 COMMIT TRANSACTION", "SELECT 1\nCOMMIT", "PRINT 'x'; COMMIT TRAN", "SELECT @@TRANCOUNT"]
    {
        asked_to_run(&h, text, None).await;
        assert_eq!(
            h.engine.boot().consoles.into_iter().find(|c| c.tab_id == h.tab).unwrap().txn,
            TxnState::Open,
            "{text}: a refused batch sends nothing"
        );
    }
    let asked = asked_to_run(&h, "IF @@TRANCOUNT > 0 COMMIT TRANSACTION", None).await;
    assert_eq!(
        asked.holds.statements,
        ["BEGIN TRAN; INSERT INTO #held VALUES (1)"],
        "the server reported the CREATE TABLE committed"
    );
    let id = h
        .engine
        .console_run(
            h.tab,
            RunRequest::Script {
                text: "IF @@TRANCOUNT > 0 COMMIT TRANSACTION".into(),
                target: RunTarget::All,
                mode: RunMode::Run,
                params: Vec::new(),
                cells: Vec::new(),
            },
            Some(asked.digest),
        )
        .await
        .unwrap()
        .run_id;
    assert_eq!(h.finish(id).await.txn, TxnState::Idle);
    let state = h.ok("SELECT @@TRANCOUNT, COUNT_BIG(*) FROM #held").await;
    assert_eq!(
        h.rows(Harness::rowsets(&state)[0])[0],
        vec![Some("0".into()), Some("1".into())],
        "the confirmed batch committed the held write"
    );
}

impl Harness {
    async fn all_rows(&mut self, sql: &str) -> Vec<Vec<Option<String>>> {
        let state = self.ok(sql).await;
        let set = Harness::rowsets(&state)[0];
        self.engine.result_page(set.r#ref, 0, set.row_count).unwrap().rows
    }

    async fn replay_in_dependency_order(&mut self, mut scripts: Vec<String>) {
        while !scripts.is_empty() {
            let mut failed = vec![];
            for script in &scripts {
                let state = self.run(script).await;
                if !matches!(state.run, RunState::Finished { outcome: RunOutcome::Ok, .. }) {
                    failed.push(script.clone());
                }
            }
            assert!(failed.len() < scripts.len(), "no script replays: {failed:#?}");
            scripts = failed;
        }
    }

    async fn catalog_facts(&mut self) -> Vec<String> {
        let seeded = format!("o.name NOT LIKE '%{}%'", "[0-9a-f]".repeat(32));
        self.all_rows(&format!(
            "SELECT s.name+'.'+o.name+' '+LEFT(f.fact COLLATE DATABASE_DEFAULT,80)+' '+CONVERT(char(64),HASHBYTES('SHA2_256',f.fact COLLATE DATABASE_DEFAULT),2)
             FROM sys.objects o JOIN sys.schemas s ON s.schema_id=o.schema_id
             CROSS APPLY (
                SELECT CONCAT(N'object ',o.type)
                UNION ALL
                SELECT CONCAT(N'column ',c.column_id,' ',c.name,' ',TYPE_NAME(c.user_type_id),' ',c.max_length,' ',c.precision,' ',c.scale,' ',c.is_nullable,' ',c.collation_name,' ',
                              CONVERT(nvarchar(40),ic.seed_value),' ',CONVERT(nvarchar(40),ic.increment_value),' ',cc.definition,' ',cc.is_persisted,' ',dc.name,' ',dc.definition)
                FROM sys.columns c LEFT JOIN sys.identity_columns ic ON ic.object_id=c.object_id AND ic.column_id=c.column_id
                LEFT JOIN sys.computed_columns cc ON cc.object_id=c.object_id AND cc.column_id=c.column_id
                LEFT JOIN sys.default_constraints dc ON dc.object_id=c.default_object_id
                WHERE c.object_id=o.object_id AND o.type='U'
                UNION ALL
                SELECT CONCAT(N'index ',i.name,' ',i.type,' ',i.is_unique,' ',i.is_primary_key,' ',i.is_unique_constraint,' ',i.filter_definition,' ',
                              (SELECT STRING_AGG(CONCAT(c.name,' ',ic.is_descending_key,' ',ic.is_included_column),',') WITHIN GROUP (ORDER BY ic.is_included_column,ic.key_ordinal,ic.index_column_id)
                               FROM sys.index_columns ic JOIN sys.columns c ON c.object_id=ic.object_id AND c.column_id=ic.column_id WHERE ic.object_id=i.object_id AND ic.index_id=i.index_id))
                FROM sys.indexes i WHERE i.object_id=o.object_id AND i.index_id>0
                UNION ALL
                SELECT CONCAT(N'foreign key ',fk.name,' ',fk.delete_referential_action,' ',fk.update_referential_action,' ',
                              (SELECT STRING_AGG(CONCAT(c.name,'>',OBJECT_SCHEMA_NAME(fc.referenced_object_id),'.',OBJECT_NAME(fc.referenced_object_id),'.',rc.name),',') WITHIN GROUP (ORDER BY fc.constraint_column_id)
                               FROM sys.foreign_key_columns fc JOIN sys.columns c ON c.object_id=fc.parent_object_id AND c.column_id=fc.parent_column_id
                               JOIN sys.columns rc ON rc.object_id=fc.referenced_object_id AND rc.column_id=fc.referenced_column_id WHERE fc.constraint_object_id=fk.object_id))
                FROM sys.foreign_keys fk WHERE fk.parent_object_id=o.object_id
                UNION ALL
                SELECT CONCAT(N'check ',k.name,' ',k.definition) FROM sys.check_constraints k WHERE k.parent_object_id=o.object_id
                UNION ALL
                SELECT CONCAT(N'trigger ',t.name,' ',t.is_disabled,' ',m.uses_ansi_nulls,' ',m.uses_quoted_identifier,' ',TRIM(CHAR(13)+CHAR(10)+CHAR(9)+' ' FROM m.definition))
                FROM sys.triggers t JOIN sys.sql_modules m ON m.object_id=t.object_id WHERE t.parent_id=o.object_id
                UNION ALL
                SELECT CONCAT(N'module ',m.uses_ansi_nulls,' ',m.uses_quoted_identifier,' ',TRIM(CHAR(13)+CHAR(10)+CHAR(9)+' ' FROM m.definition)) FROM sys.sql_modules m WHERE m.object_id=o.object_id
             ) f(fact)
             WHERE o.type IN ('U','V','P','FN','IF','TF') AND s.name IN ('dbo','lab') AND {seeded}
             ORDER BY 1"
        ))
        .await
        .into_iter()
        .map(|r| r[0].clone().unwrap())
        .collect()
    }
}

#[tokio::test]
async fn object_scripts_replay_into_an_empty_database_with_the_same_catalog() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    let name = format!("t_script_{}", uuid::Uuid::new_v4().simple());
    h.ok(&format!("CREATE DATABASE [{name}]")).await;
    let mut scratch = Harness::open(TxnMode::AutoCommit).await;
    let outcome = AssertUnwindSafe(async {
        scratch.engine.console_configure(scratch.tab, ConsoleConfig { connection_id: scratch.connection, database: name.clone(), schema: Some("dbo".into()), mode: TxnMode::AutoCommit }).await.unwrap();
        scratch.ok("CREATE SCHEMA lab").await;
        let catalog = h.engine.catalog(h.db()).await.unwrap();
        let seeded = |name: &str| !name.as_bytes().windows(32).any(|w| w.iter().all(u8::is_ascii_hexdigit));
        let mut tables = vec![];
        let mut modules = vec![];
        for schema in catalog.schemas.iter().filter(|s| s.name == "dbo" || s.name == "lab") {
            for rel in schema.relations.iter().filter(|r| seeded(&r.name)) {
                let script = h.engine.object_script(h.db(), ScriptTarget::Relation { oid: rel.oid }).await.unwrap();
                assert_eq!(script.title, format!("{}.sql", rel.name));
                if rel.kind == RelKind::Table { tables.push(script.text) } else { modules.push(script.text) }
            }
            for f in schema.functions.iter().filter(|f| seeded(&f.name)) {
                modules.push(h.engine.object_script(h.db(), ScriptTarget::Routine { oid: f.oid }).await.unwrap().text);
            }
        }
        scratch.replay_in_dependency_order(tables).await;
        scratch.replay_in_dependency_order(modules).await;
        assert_eq!(scratch.catalog_facts().await, h.catalog_facts().await);

        let relation = |schema: &str, name: &str| catalog.schemas.iter().find(|s| s.name == schema).unwrap().relations.iter().find(|r| r.name == name).unwrap().oid;
        let tickets = h.engine.object_script(h.db(), ScriptTarget::Relation { oid: relation("dbo", "tickets") }).await.unwrap().text;
        for part in [
            "CREATE TABLE [dbo].[tickets] (\n",
            "[id] int IDENTITY(100,5) NOT NULL,",
            "[code] nvarchar(20) COLLATE Latin1_General_BIN2 NOT NULL,",
            "[quantity] int NOT NULL CONSTRAINT [DF_tickets_quantity] DEFAULT ((1)),",
            "[total] AS ([price]*[quantity]) PERSISTED,",
            "CONSTRAINT [PK_tickets] PRIMARY KEY CLUSTERED ([id] ASC),",
            "SET QUOTED_IDENTIFIER ON\nGO\nCREATE   TRIGGER dbo.tickets_touch",
        ] {
            assert!(tickets.contains(part), "{part} missing from {tickets}");
        }
        let procedure = catalog.schemas.iter().flat_map(|s| &s.functions).find(|f| f.name == "cancel_order").unwrap().oid;
        let text = h.engine.object_script(h.db(), ScriptTarget::Routine { oid: procedure }).await.unwrap().text;
        assert!(text.starts_with("SET ANSI_NULLS ON\nGO\nSET QUOTED_IDENTIFIER ON\nGO\nCREATE   PROCEDURE dbo.cancel_order @order_id int AS\n") && text.ends_with("END;\nGO\n"), "{text}");
        let index = h.engine.object_script(h.db(), ScriptTarget::Index { relation_oid: relation("dbo", "orders"), name: "IX_orders_customer".into() }).await.unwrap();
        assert_eq!((index.title.as_str(), index.text.as_str()), ("IX_orders_customer.sql", "CREATE NONCLUSTERED INDEX [IX_orders_customer] ON [dbo].[orders] ([customer_id] ASC);\nGO\n"));
        let pk = h.engine.object_script(h.db(), ScriptTarget::Index { relation_oid: relation("dbo", "tickets"), name: "PK_tickets".into() }).await.unwrap();
        assert_eq!(pk.text, "ALTER TABLE [dbo].[tickets] ADD CONSTRAINT [PK_tickets] PRIMARY KEY CLUSTERED ([id] ASC);\nGO\n");
        let trigger = h.engine.object_script(h.db(), ScriptTarget::Trigger { relation_oid: relation("dbo", "tickets"), name: "tickets_touch".into() }).await.unwrap();
        assert!(trigger.text.contains("CREATE   TRIGGER dbo.tickets_touch ON dbo.tickets AFTER UPDATE AS"), "{}", trigger.text);
        assert!(matches!(h.engine.object_script(h.db(), ScriptTarget::Index { relation_oid: relation("dbo", "orders"), name: "missing".into() }).await, Err(DetailError::Gone)));
    })
    .catch_unwind()
    .await;
    drop(scratch);
    h.ok(&format!("ALTER DATABASE [{name}] SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE [{name}]")).await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

impl Harness {
    async fn target(&mut self, database: &str) -> DatabaseRef {
        self.engine
            .console_configure(
                self.tab,
                ConsoleConfig {
                    connection_id: self.connection,
                    database: database.into(),
                    schema: Some("dbo".into()),
                    mode: TxnMode::AutoCommit,
                },
            )
            .await
            .unwrap();
        DatabaseRef { connection_id: self.connection, database: database.into() }
    }

    /// Boxed, because a debug build keeps each inline call's whole future in the caller's stack
    /// frame, and enough of them overflow the test thread's stack.
    async fn script(&self, db: &DatabaseRef, target: ScriptTarget) -> Result<ObjectScript, DetailError> {
        Box::pin(self.engine.object_script(db.clone(), target)).await
    }

    async fn hostile_facts(&mut self) -> Vec<String> {
        self.all_rows(
            "SELECT s.name+'.'+o.name+' '+f.fact COLLATE DATABASE_DEFAULT
             FROM sys.objects o JOIN sys.schemas s ON s.schema_id=o.schema_id
             CROSS APPLY (
                SELECT CONCAT(N'object ',o.type,' ',t.uses_ansi_nulls,' ',t.lock_escalation_desc,' ',t.temporal_type,' ',OBJECT_SCHEMA_NAME(t.history_table_id),'.',OBJECT_NAME(t.history_table_id),' ',
                              t.history_retention_period,' ',t.history_retention_period_unit_desc,' ',
                              (SELECT CONCAT(COL_NAME(pe.object_id,pe.start_column_id),'-',COL_NAME(pe.object_id,pe.end_column_id)) FROM sys.periods pe WHERE pe.object_id=o.object_id),' ',
                              t.text_in_row_limit,' ',t.large_value_types_out_of_row,' ',(SELECT CONCAT('change tracking ',ct.is_track_columns_updated_on) FROM sys.change_tracking_tables ct WHERE ct.object_id=o.object_id))
                FROM (SELECT 1) one(x) LEFT JOIN sys.tables t ON t.object_id=o.object_id
                UNION ALL
                SELECT CONCAT(N'column ',c.column_id,' ',c.name,' ',TYPE_NAME(c.user_type_id),' ',c.max_length,' ',c.precision,' ',c.scale,' ',c.is_nullable,' ',c.collation_name,' ',
                              CONVERT(nvarchar(40),ic.seed_value),' ',CONVERT(nvarchar(40),ic.increment_value),' ',ic.is_not_for_replication,' ',cc.definition,' ',cc.is_persisted,' ',dc.name,' ',dc.definition,' ',
                              c.is_sparse,' ',c.is_column_set,' ',c.is_rowguidcol,' ',mc.masking_function,' ',c.generated_always_type,' ',c.is_hidden,' ',
                              OBJECT_NAME(c.default_object_id),' ',OBJECT_SCHEMA_NAME(c.rule_object_id),'.',OBJECT_NAME(c.rule_object_id))
                FROM sys.columns c LEFT JOIN sys.identity_columns ic ON ic.object_id=c.object_id AND ic.column_id=c.column_id
                LEFT JOIN sys.computed_columns cc ON cc.object_id=c.object_id AND cc.column_id=c.column_id
                LEFT JOIN sys.default_constraints dc ON dc.object_id=c.default_object_id
                LEFT JOIN sys.masked_columns mc ON mc.object_id=c.object_id AND mc.column_id=c.column_id
                WHERE c.object_id=o.object_id AND o.type IN ('U','V')
                UNION ALL
                SELECT CONCAT(N'index ',i.name,' ',i.type,' ',i.is_unique,' ',i.is_primary_key,' ',i.is_unique_constraint,' ',i.filter_definition,' ',i.fill_factor,' ',i.is_padded,' ',i.ignore_dup_key,' ',
                              i.allow_row_locks,' ',i.allow_page_locks,' ',i.optimize_for_sequential_key,' ',i.compression_delay,' ',i.is_disabled,' ',st.no_recompute,' ',
                              (SELECT TOP 1 CONCAT(p.data_compression_desc,' ',p.xml_compression) FROM sys.partitions p WHERE p.object_id=i.object_id AND p.index_id=i.index_id ORDER BY p.partition_number),' ',
                              (SELECT STRING_AGG(CONCAT(c.name,' ',ic.key_ordinal,' ',ic.is_descending_key,' ',ic.is_included_column,' ',ic.column_store_order_ordinal),',') WITHIN GROUP (ORDER BY ic.is_included_column,ic.key_ordinal,ic.index_column_id)
                               FROM sys.index_columns ic JOIN sys.columns c ON c.object_id=ic.object_id AND c.column_id=ic.column_id
                               WHERE ic.object_id=i.object_id AND ic.index_id=i.index_id AND i.index_id>0 AND (ic.key_ordinal>0 OR ic.is_included_column=1 OR ic.partition_ordinal=0)))
                FROM sys.indexes i LEFT JOIN sys.stats st ON st.object_id=i.object_id AND st.stats_id=i.index_id WHERE i.object_id=o.object_id AND i.type NOT IN (3,4) AND i.is_hypothetical=0
                UNION ALL
                SELECT CONCAT(N'foreign key ',fk.name,' ',fk.delete_referential_action,' ',fk.update_referential_action,' ',fk.is_disabled,' ',fk.is_not_trusted,' ',fk.is_not_for_replication,' ',
                              (SELECT STRING_AGG(CONCAT(c.name,'>',OBJECT_SCHEMA_NAME(fc.referenced_object_id),'.',OBJECT_NAME(fc.referenced_object_id),'.',rc.name),',') WITHIN GROUP (ORDER BY fc.constraint_column_id)
                               FROM sys.foreign_key_columns fc JOIN sys.columns c ON c.object_id=fc.parent_object_id AND c.column_id=fc.parent_column_id
                               JOIN sys.columns rc ON rc.object_id=fc.referenced_object_id AND rc.column_id=fc.referenced_column_id WHERE fc.constraint_object_id=fk.object_id))
                FROM sys.foreign_keys fk WHERE fk.parent_object_id=o.object_id
                UNION ALL
                SELECT CONCAT(N'check ',k.name,' ',k.definition,' ',k.is_disabled,' ',k.is_not_trusted,' ',k.is_not_for_replication) FROM sys.check_constraints k WHERE k.parent_object_id=o.object_id
                UNION ALL
                SELECT CONCAT(N'trigger ',t.name,' ',t.is_disabled,' ',m.uses_ansi_nulls,' ',m.uses_quoted_identifier,' ',TRIM(CHAR(13)+CHAR(10)+CHAR(9)+' ' FROM m.definition),' ',
                              (SELECT STRING_AGG(CONCAT(te.type_desc,te.is_first,te.is_last),',') WITHIN GROUP (ORDER BY te.type) FROM sys.trigger_events te WHERE te.object_id=t.object_id))
                FROM sys.triggers t JOIN sys.sql_modules m ON m.object_id=t.object_id WHERE t.parent_id=o.object_id AND t.name NOT IN ('enc_trg','renamed_trg')
                UNION ALL
                SELECT CONCAT(N'module ',m.uses_ansi_nulls,' ',m.uses_quoted_identifier,' ',TRIM(CHAR(13)+CHAR(10)+CHAR(9)+' ' FROM m.definition))
                FROM sys.sql_modules m WHERE m.object_id=o.object_id AND o.name NOT IN ('newname','moved','bare_moved','bare_in','renamed_trg')
                UNION ALL
                SELECT CONCAT(N'property ',ep.class,' ',CASE ep.class WHEN 7 THEN (SELECT name FROM sys.indexes WHERE object_id=ep.major_id AND index_id=ep.minor_id)
                                                                  WHEN 2 THEN (SELECT name FROM sys.parameters WHERE object_id=ep.major_id AND parameter_id=ep.minor_id)
                                                                  ELSE COL_NAME(ep.major_id,ep.minor_id) END,' ',ep.name,' ',CONVERT(nvarchar(max),ep.value),' ',CONVERT(sysname,SQL_VARIANT_PROPERTY(ep.value,'BaseType')))
                FROM sys.extended_properties ep WHERE ep.class IN (1,2,7) AND ep.major_id=o.object_id AND SQL_VARIANT_PROPERTY(ep.value,'BaseType') IN ('nvarchar','varchar')
                  AND NOT EXISTS (SELECT 1 FROM sys.indexes x WHERE ep.class=7 AND x.object_id=ep.major_id AND x.index_id=ep.minor_id AND x.type=3)
                UNION ALL
                SELECT CONCAT(N'statistics ',st.name,' ',st.no_recompute,' ',st.has_filter,' ',st.filter_definition,' ',st.auto_drop,' ',
                              (SELECT STRING_AGG(COL_NAME(sc.object_id,sc.column_id),',') WITHIN GROUP (ORDER BY sc.stats_column_id) FROM sys.stats_columns sc WHERE sc.object_id=st.object_id AND sc.stats_id=st.stats_id))
                FROM sys.stats st WHERE st.object_id=o.object_id AND st.user_created=1
                UNION ALL
                SELECT CONCAT(N'classification ',COL_NAME(sc.major_id,sc.minor_id),' ',sc.label,' ',sc.label_id,' ',sc.information_type,' ',sc.information_type_id,' ',sc.rank)
                FROM sys.sensitivity_classifications sc WHERE sc.class=1 AND sc.major_id=o.object_id
             ) f(fact)
             WHERE o.is_ms_shipped=0 AND s.name<>'cdc' AND o.type IN ('U','V','P','FN','IF','TF','TR','C','F','PK','UQ','D') AND o.name NOT IN ('enc','node_t','enc_trg')
             ORDER BY 1",
        )
        .await
        .into_iter()
        .map(|r| r[0].clone().unwrap())
        .collect()
    }
}

fn refusal(result: Result<ObjectScript, DetailError>) -> String {
    match result {
        Err(DetailError::Failed { message }) => message,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[tokio::test]
async fn hostile_object_scripts_replay_every_property_or_warn() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    let source = format!("t_hostile_{}", uuid::Uuid::new_v4().simple());
    let replay = format!("{source}_replay");
    h.ok(&format!("CREATE DATABASE [{source}]; CREATE DATABASE [{replay}]")).await;
    let mut src = Harness::open(TxnMode::AutoCommit).await;
    let mut dst = Harness::open(TxnMode::AutoCommit).await;
    let outcome = AssertUnwindSafe(Box::pin(async {
        let db = src.target(&source).await;
        src.ok(include_str!("fixtures/script-hostile-mssql.sql")).await;
        dst.target(&replay).await;
        dst.ok("CREATE SCHEMA [we]]ird s]").await;
        dst.ok("CREATE TYPE [we]]ird s].phone FROM nvarchar(20) NOT NULL").await;
        dst.ok("ALTER DATABASE CURRENT SET CHANGE_TRACKING = ON").await;
        dst.ok("CREATE DEFAULT dbo.d_zero AS 0;").await;
        dst.ok("CREATE RULE dbo.r_pos AS @v > 0").await;
        src.engine.refresh_catalog(db.clone()).await.unwrap();
        let catalog = src.engine.catalog(db.clone()).await.unwrap();

        let warned = |name: &str| -> Vec<&str> {
            match name {
                "shapes" => vec!["spatial index [SIX] on [dbo].[shapes]"],
                "on_fg" => vec!["filegroup [l1_fg] for large values on [dbo].[on_fg]", "filegroup [l1_fg] on [dbo].[on_fg]"],
                "warned" => vec![
                    "ANSI_PADDING OFF on [dbo].[warned].[pad]",
                    "XML index [IX x] on [dbo].[warned]",
                    "trigger [dbo].[enc_trg] on [dbo].[warned], whose text is encrypted or hidden from this login",
                    "extended property [Version] of type int on [dbo].[warned]",
                ],
                "parted" => vec![
                    "partition scheme [ps_hostile] on [dbo].[parted]",
                    "data compression that differs by partition on [dbo].[parted]",
                    "partition scheme [ps_hostile] for index [IX_parted] on [dbo].[parted]",
                    "STATISTICS_INCREMENTAL for index [IX_parted] on [dbo].[parted]",
                    "partition scheme [ps_hostile] for index [IX_parted_kind] on [dbo].[parted]",
                    "INCREMENTAL for statistics [st_parted] on [dbo].[parted]",
                ],
                "lob" => vec!["PERSIST_SAMPLE_PERCENT for statistics [st_sampled] on [dbo].[lob]"],
                "refd" => vec!["owner [app_reader] of [dbo].[refd], set by ALTER AUTHORIZATION"],
                "as_owner" => vec!["the owner [dbo] that EXECUTE AS OWNER on [dbo].[as_owner] runs as", "signature by certificate [cert_hostile] on [dbo].[as_owner]"],
                "np" => vec!["numbered procedure [dbo].[np];2"],
                "tree" => vec!["change data capture on [dbo].[tree]"],
                "heap_pk" => vec!["hypothetical index [hx] on [dbo].[heap_pk]"],
                "temporal_history" => vec!["history table role for [dbo].[temporal] on [dbo].[temporal_history]"],
                "Par ent" => vec!["privileges on [we]]ird s].[Par ent] (DELETE denied to [app_reader], SELECT granted to [app_reader], UPDATE ([select]) granted with grant option to [app_reader])"],
                "v_bound" => vec!["privileges on [dbo].[v_bound] (SELECT granted to [public])"],
                "f_scalar" => vec!["privileges on [dbo].[f_scalar] (EXECUTE granted to [app_reader])"],
                _ => vec![],
            }
        };
        let mut tables = vec![];
        let mut modules = vec![];
        let mut scripted = vec![];
        // Change data capture adds the cdc schema and the system table dbo.systranschemas.
        for schema in catalog.schemas.iter().filter(|s| s.name != "cdc") {
            for rel in schema.relations.iter().filter(|r| r.name != "systranschemas") {
                let result = src.script(&db, ScriptTarget::Relation { oid: rel.oid }).await;
                if rel.name == "node_t" {
                    assert_eq!(refusal(result), "Graph node tables cannot be scripted yet.");
                    continue;
                }
                let script = result.unwrap();
                assert_eq!(script.warnings, warned(&rel.name), "{}", rel.name);
                scripted.push(rel.name.clone());
                match (rel.kind, rel.name.as_str()) {
                    (_, "temporal_history") => {}
                    (RelKind::Table, _) => tables.push(script.text),
                    _ => modules.push(script.text),
                }
            }
            for f in &schema.functions {
                let result = src.script(&db, ScriptTarget::Routine { oid: f.oid }).await;
                if f.name == "enc" {
                    let message = "SQL Server has no readable definition for [dbo].[enc]. The module is encrypted, or this login lacks VIEW DEFINITION permission.";
                    assert_eq!(refusal(result), message);
                    continue;
                }
                let script = result.unwrap();
                assert_eq!(script.warnings, warned(&f.name), "{}", f.name);
                scripted.push(f.name.clone());
                modules.push(script.text);
            }
        }
        scripted.sort();
        assert_eq!(
            scripted,
            ["Par ent", "as_owner", "bare_in", "bare_moved", "bound t", "cci", "f_scalar", "go_in_comment", "heap_pk", "ledgerish", "lob", "moved", "newname", "np", "on_fg", "parted", "qi_off", "refd", "refr", "renamed_t", "shapes", "sparse_set", "temporal", "temporal_history", "tree", "v_bound", "warned"]
        );

        dst.replay_in_dependency_order(tables).await;
        dst.replay_in_dependency_order(modules).await;
        assert_eq!(dst.hostile_facts().await, src.hostile_facts().await);

        let retargeted = dst.all_rows("SELECT OBJECT_DEFINITION(OBJECT_ID(N'dbo.renamed_trg')),OBJECT_NAME(parent_id) FROM sys.triggers WHERE name=N'renamed_trg'").await;
        assert_eq!(retargeted, [[Some("CREATE TRIGGER dbo.renamed_trg ON [dbo].[renamed_t] AFTER INSERT AS SET NOCOUNT ON;".to_owned()), Some("renamed_t".to_owned())]]);
        let renamed = dst
            .all_rows(
                "SELECT OBJECT_DEFINITION(OBJECT_ID(N'dbo.newname')),OBJECT_ID(N'dbo.oldname'),OBJECT_DEFINITION(OBJECT_ID(N'[we]]ird s].moved')),OBJECT_ID(N'dbo.moved'),
                        OBJECT_DEFINITION(OBJECT_ID(N'[we]]ird s].bare_moved')),OBJECT_DEFINITION(OBJECT_ID(N'[we]]ird s].bare_in')),OBJECT_ID(N'dbo.bare_moved'),OBJECT_ID(N'dbo.bare_in')",
            )
            .await;
        assert_eq!(
            renamed,
            [[
                Some("CREATE PROCEDURE [dbo].[newname] AS SELECT 1".to_owned()),
                None,
                Some("CREATE PROCEDURE /* dbo.x */ [we]]ird s].[moved] @p int AS SELECT @p".to_owned()),
                None,
                Some("CREATE PROCEDURE [we]]ird s].[bare_moved] AS SELECT 2".to_owned()),
                Some("CREATE PROCEDURE [we]]ird s].[bare_in] AS SELECT 3".to_owned()),
                None,
                None,
            ]]
        );
        let schema_oid = |name: &str| catalog.schemas.iter().flat_map(|s| &s.relations).find(|r| r.name == name).unwrap().oid;
        let xml = src.script(&db, ScriptTarget::Index { relation_oid: schema_oid("warned"), name: "IX x".into() }).await;
        assert_eq!(refusal(xml), "Only clustered, nonclustered, and columnstore indexes can be scripted, and IX x is none of these.");
        let ordered = src.script(&db, ScriptTarget::Trigger { relation_oid: schema_oid("refr"), name: "refr_a".into() }).await.unwrap();
        assert!(
            ordered.text.ends_with("EXEC sys.sp_settriggerorder @triggername = N'[dbo].[refr_a]', @order = N'Last', @stmttype = N'INSERT';\nEXEC sys.sp_settriggerorder @triggername = N'[dbo].[refr_a]', @order = N'First', @stmttype = N'UPDATE';\nGO\n"),
            "{}",
            ordered.text
        );
        let encrypted = src.script(&db, ScriptTarget::Trigger { relation_oid: schema_oid("warned"), name: "enc_trg".into() }).await;
        assert_eq!(refusal(encrypted), "SQL Server has no readable definition for [dbo].[enc_trg]. The module is encrypted, or this login lacks VIEW DEFINITION permission.");
        let unique = src.script(&db, ScriptTarget::Index { relation_oid: schema_oid("refd"), name: "UQ_refd".into() }).await.unwrap();
        assert_eq!(
            unique.text,
            "ALTER TABLE [dbo].[refd] ADD CONSTRAINT [UQ_refd] UNIQUE NONCLUSTERED ([a] ASC, [b] ASC) WITH (IGNORE_DUP_KEY = ON, ALLOW_PAGE_LOCKS = OFF, DATA_COMPRESSION = PAGE);\nGO\n\nEXEC sys.sp_addextendedproperty @name = N'MS_Description', @value = N'the key', @level0type = N'SCHEMA', @level0name = N'dbo', @level1type = N'TABLE', @level1name = N'refd', @level2type = N'CONSTRAINT', @level2name = N'UQ_refd';\nGO\n"
        );
        let parted = src.script(&db, ScriptTarget::Index { relation_oid: schema_oid("parted"), name: "IX_parted_kind".into() }).await.unwrap();
        assert_eq!(parted.text, "-- L1 Data could not script: partition scheme [ps_hostile] for index [IX_parted_kind] on [dbo].[parted]\n\nCREATE NONCLUSTERED INDEX [IX_parted_kind] ON [dbo].[parted] ([kind] ASC);\nGO\n");
        let pk = src.script(&db, ScriptTarget::Index { relation_oid: schema_oid("Par ent"), name: "PK Par ent".into() }).await.unwrap();
        assert_eq!(pk.text, "ALTER TABLE [we]]ird s].[Par ent] ADD CONSTRAINT [PK Par ent] PRIMARY KEY NONCLUSTERED ([Id] DESC) WITH (FILLFACTOR = 80);\nGO\n");
        let computed = src.script(&db, ScriptTarget::Index { relation_oid: schema_oid("Par ent"), name: "IX total".into() }).await.unwrap();
        assert_eq!(
            computed.text,
            "SET ANSI_NULLS, ANSI_PADDING, ANSI_WARNINGS, ARITHABORT, CONCAT_NULL_YIELDS_NULL, QUOTED_IDENTIFIER ON;\nSET NUMERIC_ROUNDABORT OFF;\nGO\nCREATE NONCLUSTERED INDEX [IX total] ON [we]]ird s].[Par ent] ([total] ASC);\nGO\n"
        );
        let filtered = src.script(&db, ScriptTarget::Index { relation_oid: schema_oid("Par ent"), name: "UX ü".into() }).await.unwrap();
        assert_eq!(
            filtered.text,
            "SET ANSI_NULLS, ANSI_PADDING, ANSI_WARNINGS, ARITHABORT, CONCAT_NULL_YIELDS_NULL, QUOTED_IDENTIFIER ON;\nSET NUMERIC_ROUNDABORT OFF;\nGO\nCREATE UNIQUE NONCLUSTERED INDEX [UX ü] ON [we]]ird s].[Par ent] ([unicode ü] ASC) WHERE ([unicode ü] IS NOT NULL);\nGO\n\nEXEC sys.sp_addextendedproperty @name = N'MS_Description', @value = N'unique when set', @level0type = N'SCHEMA', @level0name = N'we]ird s', @level1type = N'TABLE', @level1name = N'Par ent', @level2type = N'INDEX', @level2name = N'UX ü';\nGO\n"
        );
    }))
    .catch_unwind()
    .await;
    drop(src);
    drop(dst);
    for name in [&source, &replay] {
        h.ok(&format!("ALTER DATABASE [{name}] SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE [{name}]")).await;
    }
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test]
async fn sequences_types_and_synonyms_load_and_script() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    let stem = format!("p10_{}", uuid::Uuid::new_v4().simple());
    let alias = format!("{stem}_phone");
    let synonym = format!("{stem}_customers");
    let sequence = format!("{stem}_n");
    let outcome = AssertUnwindSafe(async {
        h.ok(&format!("CREATE TYPE dbo.[{alias}] FROM nvarchar(20) NULL\nGO\nCREATE SYNONYM dbo.[{synonym}] FOR dbo.customers\nGO\nCREATE SEQUENCE dbo.[{sequence}] AS int START WITH 1000 INCREMENT BY 10")).await;
        h.engine.refresh_catalog(h.db()).await.unwrap();
        let catalog = h.engine.catalog(h.db()).await.unwrap();
        assert!(catalog.extensions.is_empty(), "SQL Server runs no extension query");
        let dbo = catalog.schemas.iter().find(|s| s.name == "dbo").unwrap();
        let ty = dbo.types.iter().find(|t| t.name == alias).expect("alias type");
        assert_eq!(ty.kind, TypeKind::Alias { base: "nvarchar(20)".into(), nullable: true });
        let syn = dbo.synonyms.iter().find(|s| s.name == synonym).expect("synonym");
        assert!(syn.target.to_ascii_lowercase().contains("customers"), "{}", syn.target);
        let seq = dbo.sequences.iter().find(|s| s.name == sequence).expect("sequence");
        assert_eq!(seq.type_name, "int");
        let type_sql = h.engine.object_script(h.db(), ScriptTarget::Type { oid: ty.oid }).await.unwrap().text;
        assert!(type_sql.contains("CREATE TYPE") && type_sql.contains("nvarchar(20)") && type_sql.contains("NULL"), "{type_sql}");
        let syn_sql = h.engine.object_script(h.db(), ScriptTarget::Synonym { oid: syn.oid }).await.unwrap().text;
        assert!(syn_sql.contains("CREATE SYNONYM") && syn_sql.to_ascii_lowercase().contains("customers"), "{syn_sql}");
        let seq_sql = h.engine.object_script(h.db(), ScriptTarget::Sequence { oid: seq.oid }).await.unwrap().text;
        assert!(seq_sql.contains("CREATE SEQUENCE") && seq_sql.contains("AS int") && seq_sql.contains("START WITH 1000") && seq_sql.contains("INCREMENT BY 10"), "{seq_sql}");
        let refused = h.engine.object_script(h.db(), ScriptTarget::Extension { oid: 1 }).await.unwrap_err();
        assert!(matches!(refused, DetailError::Failed { message } if message == "SQL Server has no extensions."));
    })
    .catch_unwind()
    .await;
    let _ = h.run(&format!("DROP SYNONYM IF EXISTS dbo.[{synonym}]")).await;
    let _ = h.run(&format!("DROP SEQUENCE IF EXISTS dbo.[{sequence}]")).await;
    let _ = h.run(&format!("IF TYPE_ID(N'dbo.{alias}') IS NOT NULL DROP TYPE dbo.[{alias}]")).await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

async fn run_mode(h: &mut Harness, sql: &str, mode: RunMode) -> Result<ConsoleState, RunError> {
    match h
        .engine
        .console_run(
            h.tab,
            RunRequest::Script {
                text: sql.into(),
                target: RunTarget::All,
                mode,
                params: Vec::new(),
                cells: Vec::new(),
            },
            None,
        )
        .await
    {
        Ok(accepted) => Ok(h.finish(accepted.run_id).await),
        Err(error) => Err(error),
    }
}

fn assert_plan(h: &Harness, state: &ConsoleState) -> Plan {
    let RunState::Finished { statements, outcome, .. } = &state.run else { panic!("not finished: {:?}", state.run) };
    assert!(matches!(outcome, RunOutcome::Ok), "{outcome:?} {statements:?}");
    let Some(RunOutput::Plan { result, format }) = statements.first().and_then(|statement| statement.outputs.first())
    else {
        panic!("no plan: {statements:?}")
    };
    assert_eq!(*format, PlanFormat::Xml);
    assert_eq!(result.database_kind, DatabaseKind::SqlServer);
    let cell = h.engine.result_cell(result.r#ref, 0, 0).unwrap().expect("plan cell");
    let name = result.columns.first().map(|column| column.name.as_str()).unwrap_or("");
    eprintln!("showplan column {name:?} prefix {}", &cell[..cell.len().min(160)]);
    assert!(cell.contains("<ShowPlan"), "{cell}");
    h.engine.result_plan(result.r#ref).expect("showplan parses")
}

fn any_actual(node: &PlanNode) -> bool {
    node.actual_rows.is_some() || node.children.iter().any(any_actual)
}

#[tokio::test]
async fn explain_returns_a_plan_and_the_next_select_returns_rows() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    let state = run_mode(&mut h, "select 1", RunMode::Explain).await.unwrap();
    let plan = assert_plan(&h, &state);
    assert!(!plan.statements.is_empty());
    assert!(plan.statements.iter().all(|statement| !statement.statement_text.is_empty()));
    let state = h.ok("select 2").await;
    assert_eq!(h.rows(Harness::rowsets(&state)[0]), vec![vec![Some("2".into())]]);
}

#[tokio::test]
async fn explain_of_two_statements_returns_two_roots() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    let state = run_mode(&mut h, "select 1; select 2", RunMode::Explain).await.unwrap();
    let plan = assert_plan(&h, &state);
    assert_eq!(plan.statements.len(), 2);
    eprintln!(
        "statement texts: {:?}",
        plan.statements.iter().map(|statement| &statement.statement_text).collect::<Vec<_>>()
    );
    assert!(plan.statements.iter().all(|statement| !statement.statement_text.is_empty()));
}

#[tokio::test]
async fn explain_analyze_of_a_select_records_actual_rows() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    let state = run_mode(&mut h, "SELECT TOP (1) id FROM dbo.orders", RunMode::ExplainAnalyze).await.unwrap();
    let plan = assert_plan(&h, &state);
    assert!(plan.statements.iter().any(|statement| any_actual(&statement.root)));
    let state = h.ok("select 2").await;
    assert_eq!(h.rows(Harness::rowsets(&state)[0]), vec![vec![Some("2".into())]]);
}

#[tokio::test]
async fn explain_of_an_update_does_not_ask_and_analyze_does() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    let update = "UPDATE dbo.orders SET status = status WHERE 1 = 0";
    let state = run_mode(&mut h, update, RunMode::Explain).await.expect("plain explain does not ask");
    let plan = assert_plan(&h, &state);
    assert!(!plan.statements.is_empty());
    let err = h
        .engine
        .console_run(
            h.tab,
            RunRequest::Script {
                text: update.into(),
                target: RunTarget::All,
                mode: RunMode::ExplainAnalyze,
                params: Vec::new(),
                cells: Vec::new(),
            },
            None,
        )
        .await
        .unwrap_err();
    let RunError::ConfirmWrites { confirmation } = err else { panic!("{err:?}") };
    assert!(!confirmation.on_production);
    assert_eq!(confirmation.sends.statements, [update]);
    let id = h
        .engine
        .console_run(
            h.tab,
            RunRequest::Script {
                text: update.into(),
                target: RunTarget::All,
                mode: RunMode::ExplainAnalyze,
                params: Vec::new(),
                cells: Vec::new(),
            },
            Some(confirmation.digest),
        )
        .await
        .unwrap()
        .run_id;
    let finished = h.finish(id).await;
    let _plan = assert_plan(&h, &finished);
}

#[tokio::test]
async fn explain_refuses_several_go_batches() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    let err = run_mode(&mut h, "select 1\nGO\nselect 2", RunMode::Explain).await.unwrap_err();
    assert!(matches!(err, RunError::Unsupported { ref what } if what == "Explain one batch."), "{err:?}");
}

#[tokio::test]
async fn explain_of_a_temp_table_finishes_and_the_next_select_returns_rows() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    let sql = "CREATE TABLE #lane7 (id int); SELECT * FROM #lane7";
    let state = run_mode(&mut h, sql, RunMode::Explain).await.expect("explain runs");
    let RunState::Finished { outcome, .. } = &state.run else { panic!("not finished") };
    eprintln!("lane 7 outcome {outcome:?}");
    match outcome {
        RunOutcome::Ok => {
            let _plan = assert_plan(&h, &state);
        }
        RunOutcome::Failed => {}
        other => panic!("{other:?}"),
    }
    let state = h.ok("SELECT 2 AS n").await;
    assert_eq!(h.rows(Harness::rowsets(&state)[0]), vec![vec![Some("2".into())]]);
}

#[tokio::test]
async fn plain_explain_on_production_does_not_ask() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    set_environment(&h, Some(Environment::Production)).await;
    let update = "UPDATE dbo.orders SET status = status WHERE 1 = 0";
    let state = run_mode(&mut h, update, RunMode::Explain).await.expect("plain explain does not ask on Production");
    let plan = assert_plan(&h, &state);
    assert!(!plan.statements.is_empty());
}

#[tokio::test]
async fn parameters_preserve_declarations_and_quote_unicode_across_go() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    let text = "DECLARE @x int = 7; SELECT @X AS declared, @input AS value;\nGO\nSELECT @x AS [external], @input AS repeated, @nil AS nil";
    let params = h.engine.script_params(text, RunTarget::All, DatabaseKind::SqlServer).unwrap();
    assert_eq!(params.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["@input", "@x", "@nil"]);
    let value = "世界'; SELECT 99; -- \\";
    let accepted = h
        .engine
        .console_run(
            h.tab,
            RunRequest::Script {
                text: text.into(),
                target: RunTarget::All,
                mode: RunMode::Run,
                params: vec![
                    ParamValue { name: "@input".into(), value: Some(value.into()) },
                    ParamValue { name: "@x".into(), value: Some("outside".into()) },
                    ParamValue { name: "@nil".into(), value: None },
                ],
                cells: Vec::new(),
            },
            None,
        )
        .await
        .unwrap();
    let state = h.finish(accepted.run_id).await;
    assert!(matches!(state.run, RunState::Finished { outcome: RunOutcome::Ok, .. }), "{:?}", state.run);
    let sets = Harness::rowsets(&state);
    assert_eq!(h.rows(sets[0]), [vec![Some("7".into()), Some(value.into())]]);
    assert_eq!(h.rows(sets[1]), [vec![Some("outside".into()), Some(value.into()), None]]);
}

#[path = "common/database_map.rs"]
mod database_map_cases;

#[tokio::test]
async fn database_map_pages_declared_neighborhoods() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    let stem = format!("map_{}", uuid::Uuid::new_v4().simple());
    h.ok(&database_map_cases::fixture("dbo", &stem, DatabaseKind::SqlServer)).await;
    database_map_cases::verify(&h.engine, h.db(), "dbo", &stem, DatabaseKind::SqlServer).await;
    h.ok(&format!("DROP TABLE dbo.{stem}_fan,dbo.{stem}_child; ALTER TABLE dbo.{stem}_root DROP CONSTRAINT {stem}_self_pair; DROP TABLE dbo.{stem}_root,dbo.{stem}_anchor,dbo.{stem}_alone")).await;
}

#[path = "mssql/import.rs"]
mod import_tests;

#[tokio::test]
async fn a_reference_chain_runs_inside_a_batch_with_hoisted_ctes() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    let cells = vec![
        CellSource { name: "paid".into(), text: "WITH p AS (SELECT customer_id, total FROM dbo.orders WHERE status = N'paid')\nSELECT customer_id, total FROM p ORDER BY total DESC;".into() },
        CellSource { name: "paid regions".into(), text: "SELECT c.region, SUM(p.total) AS revenue FROM {{paid}} p JOIN dbo.customers c ON c.id = p.customer_id GROUP BY c.region".into() },
    ];
    let request = |text: &str| RunRequest::Script {
        text: text.into(),
        target: RunTarget::All,
        mode: RunMode::Run,
        params: Vec::new(),
        cells: cells.clone(),
    };
    let text = "DECLARE @min decimal(10, 2) = 10\nSELECT region, revenue FROM {{paid regions}} WHERE revenue > @min";
    let accepted = h.engine.console_run(h.tab, request(text), None).await.unwrap();
    assert_eq!(accepted.sources, cells, "both cells, the one read first first");
    let state = h.finish(accepted.run_id).await;
    assert!(matches!(state.run, RunState::Finished { outcome: RunOutcome::Ok, .. }), "{:?}", state.run);
    let sets = Harness::rowsets(&state);
    assert_eq!(sets.len(), 1);
    assert_eq!(h.rows(sets[0]), vec![vec![Some("Europe".into()), Some("125.50".into())]]);
    let history = h.engine.history(HistoryQuery {
        text: Some("{{paid regions}}".into()),
        connection_id: Some(h.connection),
        before_id: None,
        limit: 10,
    });
    let ran = &history.entries.first().expect("the batch is in history").sql;
    assert!(ran.starts_with("DECLARE @min decimal(10, 2) = 10\n;WITH p AS (SELECT customer_id, total FROM dbo.orders WHERE status = N'paid'),\n[{{paid}}] AS (\nSELECT customer_id, total FROM p\n),"), "{ran}");

    let refused = h.engine.console_run(h.tab, request("IF 1 = 1 SELECT * FROM {{paid}}"), None).await.unwrap_err();
    assert!(
        matches!(refused, RunError::Reference { ref message } if message.starts_with("Put BEGIN and END")),
        "{refused:?}"
    );
    let wrapped = h
        .engine
        .console_run(h.tab, request("IF 1 = 1\nBEGIN\n  SELECT customer_id FROM {{paid}}\nEND"), None)
        .await
        .unwrap();
    let state = h.finish(wrapped.run_id).await;
    assert!(matches!(state.run, RunState::Finished { outcome: RunOutcome::Ok, .. }), "{:?}", state.run);
    assert_eq!(h.rows(Harness::rowsets(&state)[0]), vec![vec![Some("1".into())]]);
}
