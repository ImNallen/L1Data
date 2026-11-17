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
                name: "SQL Server drill-down".into(),
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
        Self { engine, events, db, _dir: dir }
    }

    async fn tab(&self) -> TabId {
        let tab = TabId(uuid::Uuid::new_v4());
        let config = ConsoleConfig {
            connection_id: self.db.connection_id,
            database: self.db.database.clone(),
            schema: None,
            mode: TxnMode::AutoCommit,
        };
        self.engine.console_configure(tab, config).await.unwrap();
        tab
    }

    async fn run(&mut self, tab: TabId, request: RunRequest) -> Vec<ResultMeta> {
        let accepted = self.engine.console_run(tab, request, None).await.unwrap();
        let state = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if let AppEvent::ConsoleChanged(state) = self.events.recv().await.unwrap()
                    && state.tab_id == tab
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
                RunOutput::Rows { result, .. } => Some(result.clone()),
                _ => None,
            })
            .collect()
    }

    async fn sql(&mut self, tab: TabId, text: &str) -> Vec<ResultMeta> {
        let request = RunRequest::Script {
            text: text.into(),
            target: RunTarget::All,
            mode: RunMode::Run,
            params: Vec::new(),
            cells: Vec::new(),
        };
        self.run(tab, request).await
    }

    fn rows(&self, meta: &ResultMeta) -> Vec<Vec<Option<String>>> {
        let page = self.engine.result_page(meta.r#ref, 0, meta.row_count).unwrap();
        let mut rows = page.rows;
        for (row, column) in page.cut {
            rows[row as usize][usize::from(column)] = self.engine.result_cell(meta.r#ref, row, column).unwrap();
        }
        rows
    }

    fn drill(&self, meta: &ResultMeta, row: usize) -> DrillQuery {
        let point = DrillPoint::Row { row: row as u32, title: "Untitled-1.sql".into() };
        self.engine.result_drill(meta.r#ref, point).unwrap_or_else(|e| panic!("{e:?}"))
    }

    fn refusal(&self, meta: &ResultMeta) -> String {
        match self.engine.result_drill(meta.r#ref, DrillPoint::Probe) {
            Err(DrillError::Unsupported { reason }) => reason,
            other => panic!("{other:?}"),
        }
    }

    /// Drills into every row of the grouped `meta`, whose `n` column counts the rows behind it, and
    /// checks that the drilled SQL reads exactly that many rows, each with the row's key values.
    async fn drills_exactly(&mut self, rows_tab: TabId, meta: &ResultMeta, keys: &[(&str, &str)]) {
        let probe = self.engine.result_drill(meta.r#ref, DrillPoint::Probe).unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(probe.sql, "");
        let n = meta.columns.iter().position(|c| c.name == "n").unwrap();
        let rows = self.rows(meta);
        assert!(rows.len() > 1, "groups to drill into");
        for (row, cells) in rows.iter().enumerate() {
            let drilled = self.drill(meta, row);
            assert_eq!((&drilled.db, &drilled.schema), (&meta.db, &None), "the drill reads where the result was read");
            let behind = &self.sql(rows_tab, &drilled.sql).await[0];
            assert_eq!(Some(behind.rows_seen.to_string()), cells[n], "{}", drilled.sql);
            for (grouped, read) in keys {
                let g = meta.columns.iter().position(|c| c.name == *grouped).unwrap();
                let r = behind.columns.iter().position(|c| c.name == *read).unwrap();
                for behind_row in self.rows(behind) {
                    assert_eq!(cells[g], behind_row[r], "{grouped}\n{}", drilled.sql);
                }
            }
        }
    }
}

#[tokio::test]
async fn a_drilled_group_reads_exactly_the_rows_it_counted() {
    let mut h = Harness::open().await;
    let (tab, rows_tab) = (h.tab().await, h.tab().await);
    let table = format!("l1_drill_{}", uuid::Uuid::new_v4().simple());
    let outcome = AssertUnwindSafe(async {
        let shop = "SELECT TOP (10) c.region, COUNT(*) AS n FROM dbo.orders o JOIN dbo.customers c ON c.id = o.customer_id GROUP BY c.region ORDER BY n DESC";
        let meta = h.sql(tab, shop).await.remove(0);
        h.drills_exactly(rows_tab, &meta, &[("region", "region")]).await;
        let europe = h.rows(&meta).iter().position(|r| r[0].as_deref() == Some("Europe")).unwrap();
        assert_eq!(
            h.drill(&meta, europe).sql,
            "-- Rows behind Europe in Untitled-1.sql\nSELECT *\nFROM dbo.orders o JOIN dbo.customers c ON c.id = o.customer_id\nWHERE (c.region) = N'Europe'"
        );

        h.sql(tab, &format!(
            "CREATE TABLE dbo.[{table}] (
                id int IDENTITY PRIMARY KEY,
                word nvarchar(20) COLLATE Latin1_General_CI_AS,
                legacy varchar(20) COLLATE SQL_Latin1_General_CP1_CI_AS,
                padded char(6),
                amount decimal(10, 2),
                price money,
                ratio float,
                small real,
                flag bit,
                day date,
                old datetime,
                modern datetime2(7),
                moment datetimeoffset(7),
                tick time(7),
                uid uniqueidentifier,
                bytes varbinary(4),
                variant sql_variant
            );
            INSERT INTO dbo.[{table}] (word, legacy, padded, amount, price, ratio, small, flag, day, old, modern, moment, tick, uid, bytes, variant) VALUES
              (N'Straße', 'Straße', 'ab', 1.50, 125.5, 0.1, 0.3, 1, '2024-01-02', '2024-01-02T03:04:05.123', '2024-01-02T03:04:05.1234567', '2024-01-02T03:04:05.1234567+02:00', '03:04:05.1234567', 'abcd1234-5678-90ab-cdef-1234567890ab', 0xDEAD, 1),
              (N'strasse', 'strasse', 'ab  ', 1.50, 125.5, 0.1, 0.3, 1, '2024-01-02', '2024-01-02T03:04:05.123', '2024-01-02T03:04:05.1234567', '2024-01-02T01:04:05.1234567+00:00', '03:04:05.1234567', 'ABCD1234-5678-90AB-CDEF-1234567890AB', 0xDEAD, 2),
              (N'it''s', 'O''Brien', 'cd', -0.25, 0.0001, 1e300, 3.4e38, 0, '1999-12-31', '1999-12-31T23:59:59.997', '1999-12-31T23:59:59.9999999', '1999-12-31T23:59:59.9999999-08:00', '23:59:59.9999999', '00000000-0000-0000-0000-000000000000', 0x00, 3),
              (NULL, 'o''brien', NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL),
              (NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL)"
        )).await;
        // The drill must not lean on the session's date format to read its literals.
        h.sql(rows_tab, "SET DATEFORMAT dmy").await;
        for column in ["word", "padded", "amount", "price", "ratio", "small", "flag", "day", "old", "modern", "moment", "tick", "uid", "bytes"] {
            let meta = h.sql(tab, &format!("SELECT [{column}], COUNT(*) AS n FROM dbo.[{table}] GROUP BY [{column}]")).await.remove(0);
            let exact: &[(&str, &str)] = match column {
                // Text groups compare under the column's collation. Datetimeoffset groups by instant.
                "word" | "padded" | "moment" => &[],
                _ => &[(column, column)],
            };
            h.drills_exactly(rows_tab, &meta, exact).await;
        }

        let two_keys = format!(
            "WITH k AS (SELECT * FROM dbo.[{table}] WHERE id > 0)
             SELECT COUNT(*) AS n, CAST(k.day AS date) AS [day], YEAR(old) AS y FROM k WITH (NOLOCK)
             WHERE k.flag IS NULL OR k.flag = 1 GROUP BY YEAR(old), CAST(k.day AS DATE) HAVING COUNT(*) > 0
             ORDER BY n OFFSET 0 ROWS FETCH NEXT 10 ROWS ONLY"
        );
        let meta = h.sql(tab, &two_keys).await.remove(0);
        h.drills_exactly(rows_tab, &meta, &[("day", "day")]).await;

        let batches = format!("SELECT 1 AS one\nGO\nSELECT TOP (50) PERCENT WITH TIES flag, COUNT(*) AS n FROM dbo.[{table}] GROUP BY flag ORDER BY flag");
        let results = h.sql(tab, &batches).await;
        assert_eq!(results.len(), 2);
        h.drills_exactly(rows_tab, &results[1], &[("flag", "flag")]).await;
        assert!(matches!(h.engine.result_drill(results[0].r#ref, DrillPoint::Probe), Err(DrillError::Ungrouped)));

        for (sql, reason) in [
            (format!("SELECT variant, COUNT(*) AS n FROM dbo.[{table}] GROUP BY variant"), "A grouped sql_variant column cannot be compared to its text."),
            (format!("SET NOCOUNT ON; SELECT flag, COUNT(*) AS n FROM dbo.[{table}] GROUP BY flag"), "The chart's batch is more than one statement."),
            (format!("SELECT flag, COUNT(*) AS n FROM dbo.[{table}] GROUP BY flag SELECT 2 AS two"), "The chart's batch is more than one statement."),
            (format!("SELECT flag, COUNT(*) AS n FROM dbo.[{table}] GROUP BY flag UNION ALL SELECT NULL, 0"), "The chart's query uses UNION, INTERSECT, or EXCEPT."),
            (format!("SELECT flag, COUNT(*) AS n FROM dbo.[{table}] GROUP BY flag WITH ROLLUP"), "The chart's query uses GROUPING SETS, ROLLUP, or CUBE."),
            (format!("SELECT flag, COUNT(*) AS n FROM dbo.[{table}] WHERE dbo.order_count(id) >= 0 GROUP BY flag"), "The chart's query calls functions that may not be safe to run again."),
            (format!("SELECT flag, COUNT(*) AS n FROM dbo.[{table}] GROUP BY flag OPTION (MAXDOP 1)"), "The chart's query has an OPTION clause."),
            (format!("SELECT UPPER(word) AS word, COUNT(*) AS n FROM dbo.[{table}] GROUP BY word"), "A GROUP BY column has the name of another column's alias. Rename the alias."),
        ] {
            let meta = h.sql(tab, &sql).await.remove(0);
            assert_eq!(h.refusal(&meta), reason, "{sql}");
        }
        let raw = h.sql(tab, &format!("SELECT flag, id FROM dbo.[{table}]")).await.remove(0);
        assert!(matches!(h.engine.result_drill(raw.r#ref, DrillPoint::Probe), Err(DrillError::Ungrouped)));
        let page = RunRequest::TablePage { schema: "dbo".into(), table: table.clone(), page: 0, query: TableQuery::default() };
        let page = h.run(tab, page).await.remove(0);
        assert_eq!(h.refusal(&page), "A table page filters the table instead.");
    })
    .catch_unwind()
    .await;
    h.sql(tab, &format!("DROP TABLE IF EXISTS dbo.[{table}]")).await;
    h.engine.shutdown().await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}
