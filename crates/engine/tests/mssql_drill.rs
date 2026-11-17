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
        let mut rows = Vec::new();
        while (rows.len() as u32) < meta.row_count {
            let start = rows.len() as u32;
            let page = self.engine.result_page(meta.r#ref, start, meta.row_count - start).unwrap();
            rows.extend(page.rows);
            for (row, column) in page.cut {
                rows[row as usize][usize::from(column)] = self.engine.result_cell(meta.r#ref, row, column).unwrap();
            }
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
            (format!("SELECT flag, COUNT(*) AS n FROM dbo.[{table}] TABLESAMPLE (50 PERCENT) GROUP BY flag"), "The chart's query uses TABLESAMPLE, so a new cell would read another sample."),
            (format!("SELECT flag, COUNT(*) AS n FROM dbo.[{table}] WHERE CHECKSUM(NEWID()) % 2 = 0 GROUP BY flag"), "The chart's query calls functions that may not be safe to run again."),
            (format!("SELECT DATEDIFF(day, modern, SYSDATETIME()) AS age, COUNT(*) AS n FROM dbo.[{table}] GROUP BY DATEDIFF(day, modern, SYSDATETIME())"), UNREPRODUCIBLE),
            (format!("SELECT CONCAT(word, CURRENT_USER) AS who, COUNT(*) AS n FROM dbo.[{table}] GROUP BY CONCAT(word, CURRENT_USER)"), UNREPRODUCIBLE),
            (format!("WITH k AS (SELECT GETDATE() AS now FROM dbo.[{table}]) SELECT k.now, COUNT(*) AS n FROM k GROUP BY k.now"), UNREPRODUCIBLE),
            (format!("SELECT CONCAT(word, NEWID()) AS g, COUNT(*) AS n FROM dbo.[{table}] GROUP BY CONCAT(word, NEWID())"), "The chart's query calls functions that may not be safe to run again."),
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

const UNREPRODUCIBLE: &str = "The chart's query reads the time or the session in a GROUP BY key, its WITH, or its FROM, which a new cell would read again with other values.";

const NON_ASCII: &str =
    "A grouped varchar value is not plain ASCII, so its literal could spell another value in the column's code page.";

/// The sorted `id` column of `meta`, joined with commas, as `STRING_AGG(id) WITHIN GROUP (ORDER BY id)` shows it.
fn ids_of(h: &Harness, meta: &ResultMeta) -> String {
    let c = meta.columns.iter().position(|c| c.name == "id").expect("an id column");
    let mut ids: Vec<i64> = h.rows(meta).iter().map(|r| r[c].as_deref().unwrap().parse().unwrap()).collect();
    ids.sort();
    ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",")
}

/// Each grouped query's `ids` column lists the ids of the rows in each group. Every group must drill to
/// exactly those rows, or refuse a non-ASCII varchar key with its reason.
#[tokio::test]
async fn every_group_drills_to_exactly_its_own_rows() {
    let mut h = Harness::open().await;
    let (tab, rows_tab) = (h.tab().await, h.tab().await);
    let t = format!("l1_drill_ids_{}", uuid::Uuid::new_v4().simple());
    let outcome = AssertUnwindSafe(async {
        h.sql(tab, &format!(r#"
CREATE TABLE dbo.[{t}] (
  id int IDENTITY PRIMARY KEY,
  g varchar(20) COLLATE Greek_CI_AS, c varchar(20) COLLATE Cyrillic_General_CI_AS, u8 varchar(20) COLLATE Latin1_General_100_CI_AS_SC_UTF8,
  ai nvarchar(20) COLLATE Latin1_General_CI_AI, sqlv varchar(20), pad char(6), npad nchar(4), supp nvarchar(20),
  cs nvarchar(20) COLLATE Latin1_General_CS_AS, bin2 varchar(20) COLLATE Latin1_General_BIN2,
  f float, r real, d38 decimal(38,10), d38z decimal(38,0), m money, sm smallmoney,
  dt datetime, dt2 datetime2(7), sdt smalldatetime, dto datetimeoffset(7), tm time(7), dd date, u uniqueidentifier,
  b bit, vb varbinary(8), tiny tinyint, big bigint, cc AS (id % 3), [we ird] nvarchar(10)
);
INSERT dbo.[{t}] (g, c, u8, ai, sqlv, pad, npad, supp, cs, bin2, f, r, d38, d38z, m, sm, dt, dt2, sdt, dto, tm, dd, u, b, vb, tiny, big, [we ird]) VALUES
 (N'Ω', N'Ж', N'Ω', N'é', 'Straße', 'ab', N'ab', N'😀', N'Ab', 'a', CAST(7930575147043188 AS float) * 4e0, 0.1, 1234567890123456789012345678.1234567890, 12345678901234567890123456789012345678, -922337203685477.5808, -214748.3648, '2024-01-02T03:04:05.997', '2024-01-02T03:04:05.1234567', '2024-01-02T03:04:00', '2024-01-02T03:04:05.1234567+02:00', '23:59:59.9999999', '0001-01-01', 'ABCD1234-5678-90AB-CDEF-1234567890AB', 1, 0x01, 0, -9223372036854775808, N'x y'),
 (N'α', N'д', N'😀', N'E', 'strasse', 'ab   ', N'ab  ', N'😁', N'ab', 'A', CAST(7930575147043187 AS float) * 4e0, 0.3, 0.0000000001, -1, 0.0001, 0, '2024-01-02T03:04:05.993', '2024-01-02T03:04:05.1234568', '2024-01-02T03:05:00', '2024-01-02T01:04:05.1234567+00:00', '00:00:00', '9999-12-31', NULL, 0, 0x0100, 255, 9223372036854775807, N'x y'),
 (N'a', N'a', N'?', N'e', 'STRASSE', '', N'', N'', N'AB', 'a ', CAST(7930575147043187 AS float) * 4e0, 1e-30, -0.5, 0, 0, 0, NULL, NULL, NULL, NULL, NULL, '2024-02-29', NEWID(), NULL, 0x010000, NULL, NULL, N''),
 (N'?', N'?', N'a', N'ê', '', NULL, NULL, NULL, NULL, NULL, -0e0, -0e0, NULL, NULL, NULL, NULL, '1753-01-01', '0001-01-01', NULL, NULL, NULL, NULL, NULL, NULL, 0x, NULL, NULL, NULL),
 (N'Ψ', N'Я', N'é', NULL, NULL, NULL, NULL, NULL, NULL, NULL, 0e0, 1e30, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL);
"#)).await;
        // The drilled cell's own session settings must not change what its literals mean.
        h.sql(rows_tab, "SET LANGUAGE French; SET DATEFORMAT dmy; SET DATEFIRST 1").await;
        let agg = "COUNT(*) AS n, STRING_AGG(CONVERT(varchar(max), id), ',') WITHIN GROUP (ORDER BY id) AS ids";
        let mut cases: Vec<String> = [
            "g", "c", "u8", "ai", "sqlv", "pad", "npad", "supp", "cs", "bin2", "f", "r", "d38", "d38z", "m", "sm", "dt",
            "dt2", "sdt", "dto", "tm", "dd", "u", "b", "vb", "tiny", "big", "cc", "[we ird]",
        ]
        .iter()
        .map(|col| format!("SELECT {col}, {agg} FROM dbo.[{t}] GROUP BY {col}"))
        .collect();
        cases.extend([
            format!("SELECT YEAR(dt) AS y, {agg} FROM dbo.[{t}] GROUP BY YEAR(dt)"),
            format!("SELECT CAST(dt2 AS date) AS d, {agg} FROM dbo.[{t}] GROUP BY CAST(dt2 AS date)"),
            format!("SELECT LEFT(g, 1) AS l, {agg} FROM dbo.[{t}] GROUP BY LEFT(g, 1)"),
            format!("SELECT CAST(ai AS varchar(20)) AS l, {agg} FROM dbo.[{t}] GROUP BY CAST(ai AS varchar(20))"),
            format!("SELECT g, b, {agg} FROM dbo.[{t}] GROUP BY g, b"),
            format!("SELECT b, {agg} FROM dbo.[{t}] WHERE b = 1 OR b = 0 OR id = 3 GROUP BY b"),
            format!("SELECT sqlv, {agg} FROM dbo.[{t}] WHERE ai <> N'zz' OR g = N'Ω' GROUP BY sqlv"),
            format!("SELECT b, {agg} FROM dbo.[{t}] WHERE id < 3 OR id > 0 -- trailing\nGROUP BY b"),
            format!("SELECT b, {agg} FROM dbo.[{t}] WHERE /* x */ id < 4 /* y GROUP BY id */ GROUP BY b"),
            format!("SELECT b, {agg} FROM dbo.[{t}] WHERE dt2 < DATEADD(year, 1, SYSDATETIME()) OR dt2 IS NULL GROUP BY b"),
            format!("WITH k AS (SELECT * FROM dbo.[{t}] WHERE id > 1) SELECT k.b, {agg} FROM k GROUP BY k.b"),
            format!("SELECT TOP (50) PERCENT WITH TIES b, {agg} FROM dbo.[{t}] GROUP BY b ORDER BY b"),
            format!("SELECT b, {agg} FROM dbo.[{t}] GROUP BY b ORDER BY b OFFSET 1 ROWS FETCH NEXT 5 ROWS ONLY"),
            format!("SELECT b, {agg} FROM dbo.[{t}] GROUP BY b HAVING COUNT(*) < 3"),
            format!("SELECT sqlv COLLATE Latin1_General_BIN AS s, {agg} FROM dbo.[{t}] GROUP BY sqlv COLLATE Latin1_General_BIN"),
            format!("SELECT CASE WHEN b = 1 THEN 'y' ELSE 'n' END AS k, {agg} FROM dbo.[{t}] GROUP BY CASE WHEN b = 1 THEN 'y' ELSE 'n' END"),
            format!("SELECT x.v, {agg} FROM dbo.[{t}] CROSS APPLY (SELECT tiny AS v) x GROUP BY x.v"),
            format!("SELECT x.v, {agg} FROM dbo.[{t}] o OUTER APPLY (SELECT TOP (1) i.b AS v FROM dbo.[{t}] i WHERE i.id = o.id + 1) x GROUP BY x.v"),
            format!("SELECT d.b, COUNT(*) AS n, STRING_AGG(CONVERT(varchar(max), d.id), ',') WITHIN GROUP (ORDER BY d.id) AS ids FROM (SELECT id, b FROM dbo.[{t}] WHERE id < 3 UNION ALL SELECT id, b FROM dbo.[{t}] WHERE id >= 3) d GROUP BY d.b"),
            "SELECT c.region, COUNT(*) AS n, STRING_AGG(CONVERT(varchar(max), o.id), ',') WITHIN GROUP (ORDER BY o.id) AS ids FROM dbo.orders o JOIN dbo.customers c ON c.id = o.customer_id WHERE o.total > 0 OR c.name LIKE N'%' GROUP BY c.region".into(),
        ]);
        let mut refused_non_ascii = 0;
        for sql in &cases {
            let meta = h.sql(tab, sql).await.pop().unwrap();
            h.engine.result_drill(meta.r#ref, DrillPoint::Probe).unwrap_or_else(|e| panic!("{sql}: {e:?}"));
            let ids = meta.columns.iter().position(|c| c.name == "ids").unwrap();
            let rows = h.rows(&meta);
            assert!(rows.len() > 1, "groups to drill into in {sql}");
            for (row, cells) in rows.iter().enumerate() {
                let point = DrillPoint::Row { row: row as u32, title: "Untitled-1.sql".into() };
                match h.engine.result_drill(meta.r#ref, point) {
                    Ok(drilled) => {
                        let behind = h.sql(rows_tab, &drilled.sql).await.remove(0);
                        assert_eq!(Some(ids_of(&h, &behind)), cells[ids].clone(), "{sql}\n{}", drilled.sql);
                    }
                    Err(DrillError::Unsupported { reason }) => {
                        assert_eq!(reason, NON_ASCII, "{sql} {cells:?}");
                        assert!(cells[..ids - 1].iter().flatten().any(|v| !v.is_ascii()), "{sql} {cells:?}");
                        refused_non_ascii += 1;
                    }
                    Err(e) => panic!("{sql} {cells:?}: {e:?}"),
                }
            }
        }
        assert!(refused_non_ascii >= 10, "the Greek, Cyrillic, UTF-8, and default-collation varchar keys refuse");
    })
    .catch_unwind()
    .await;
    h.sql(tab, &format!("DROP TABLE IF EXISTS dbo.[{t}]")).await;
    h.engine.shutdown().await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

/// xorshift64*, so a failure names the seed that reproduces it.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// Doubles across the whole range: random bit patterns, subnormals, values near 2^53 and near the
/// largest double, and integers and decimals between 1e-6 and 1e22, each with either sign.
fn doubles(rng: &mut Rng) -> Vec<f64> {
    let mut out = vec![
        0.0,
        -0.0,
        0.1,
        1e22,
        1e308,
        f64::MAX,
        f64::MIN,
        f64::MIN_POSITIVE,
        f64::from_bits(1),
        f64::from_bits(0x000F_FFFF_FFFF_FFFF),
        f64::from_bits(0x435C_2CD0_EA81_0974),
        9007199254740992.0,
        9007199254740991.0,
        9007199254740994.0,
    ];
    let sign = |rng: &mut Rng| rng.next() & (1 << 63);
    let fraction = |rng: &mut Rng| rng.next() & ((1 << 52) - 1);
    for _ in 0..8000 {
        let bits = rng.next();
        if (bits >> 52) & 0x7ff != 0x7ff {
            out.push(f64::from_bits(bits));
        }
    }
    for _ in 0..3000 {
        out.push(f64::from_bits(sign(rng) | fraction(rng)));
    }
    for _ in 0..3000 {
        out.push(f64::from_bits(sign(rng) | (1074 + rng.below(4)) << 52 | fraction(rng)));
    }
    for _ in 0..1000 {
        out.push(((1i64 << 53) + rng.below(10_000) as i64 - 5_000) as f64);
    }
    for _ in 0..2000 {
        out.push(f64::from_bits(sign(rng) | (0x7fd + rng.below(2)) << 52 | fraction(rng)));
    }
    for _ in 0..6000 {
        let v = 10f64.powf(-6.0 + 28.0 * rng.unit());
        out.push(if rng.below(2) == 0 { v.round() } else { v });
    }
    out
}

fn reals(rng: &mut Rng) -> Vec<f32> {
    let mut out =
        vec![0.0, -0.0, 0.1, f32::MAX, f32::MIN, f32::MIN_POSITIVE, f32::from_bits(1), 16777216.0, 16777215.0];
    for _ in 0..3000 {
        let bits = rng.next() as u32;
        if (bits >> 23) & 0xff != 0xff {
            out.push(f32::from_bits(bits));
        }
    }
    for _ in 0..1000 {
        out.push(f32::from_bits(rng.next() as u32 & 0x807F_FFFF));
    }
    for _ in 0..1000 {
        out.push(10f32.powf(-6.0 + 28.0 * rng.unit() as f32));
    }
    out
}

/// The test's own exact construction of a double, for the INSERT. The stored value is read back and
/// compared bit for bit, so the proof does not lean on it.
fn insert_literal(v: f64) -> String {
    let bits = v.to_bits();
    let exponent = ((bits >> 52) & 0x7ff) as i32;
    let fraction = (bits & ((1 << 52) - 1)) as i64;
    let (mantissa, power) = if exponent == 0 { (fraction, -1074) } else { (fraction | 1 << 52, exponent - 1075) };
    let sign = if v.is_sign_negative() { "-" } else { "" };
    format!("{sign}(CAST({mantissa} AS float) * POWER(CAST(2 AS float), {power}))")
}

/// Every float and real group of a random sample of more than 20,000 doubles drills to exactly its own
/// rows, where `CONVERT(float, N'…')` of the shown text picks a neighbouring double for some.
#[tokio::test]
async fn a_float_or_real_key_drills_to_exactly_its_value() {
    let seed = std::env::var("L1_DATA_TEST_SEED").ok().and_then(|s| s.parse().ok()).unwrap_or(0x5EED_1167);
    eprintln!("seed {seed}");
    let mut rng = Rng(seed);
    let floats = doubles(&mut rng);
    let reals = reals(&mut rng);
    assert!(floats.len() >= 20_000);
    let mut h = Harness::open().await;
    let (tab, rows_tab) = (h.tab().await, h.tab().await);
    let t = format!("l1_drill_float_{}", uuid::Uuid::new_v4().simple());
    let outcome = AssertUnwindSafe(async {
        h.sql(tab, &format!("CREATE TABLE dbo.[{t}] (id int PRIMARY KEY, f float NOT NULL, r real NULL)")).await;
        for (chunk, values) in floats.chunks(1000).enumerate() {
            let rows: Vec<String> = values
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    let id = chunk * 1000 + i;
                    let r = reals
                        .get(id)
                        .map_or("NULL".into(), |r| format!("CAST({} AS real)", insert_literal(f64::from(*r))));
                    format!("({id}, {}, {r})", insert_literal(*v))
                })
                .collect();
            h.sql(tab, &format!("INSERT INTO dbo.[{t}] (id, f, r) VALUES {}", rows.join(", "))).await;
        }
        h.sql(tab, &format!("CREATE INDEX f ON dbo.[{t}] (f); CREATE INDEX r ON dbo.[{t}] (r)")).await;
        let stored = h.sql(tab, &format!("SELECT id, f, r FROM dbo.[{t}] ORDER BY id")).await.remove(0);
        for (id, row) in h.rows(&stored).iter().enumerate() {
            let f: f64 = row[1].as_deref().unwrap().parse().unwrap();
            assert_eq!(f.to_bits(), floats[id].to_bits(), "row {id} stores {f}, not {}", floats[id]);
            if let Some(r) = reals.get(id) {
                let read: f32 = row[2].as_deref().unwrap().parse().unwrap();
                assert_eq!(read.to_bits(), r.to_bits(), "row {id} stores {read}, not {r}");
            }
        }

        for column in ["f", "r"] {
            let grouped = format!("SELECT {column}, COUNT(*) AS n, MIN(id) AS lo FROM dbo.[{t}] GROUP BY {column}");
            let meta = h.sql(tab, &grouped).await.remove(0);
            let rows = h.rows(&meta);
            let mut checks = Vec::new();
            for (row, cells) in rows.iter().enumerate() {
                let point = DrillPoint::Row { row: row as u32, title: "Untitled-1.sql".into() };
                let drilled = h.engine.result_drill(meta.r#ref, point).unwrap_or_else(|e| panic!("{cells:?}: {e:?}"));
                let Some(literal) = drilled.sql.rsplit_once(") = ").map(|(_, l)| l.to_owned()) else {
                    assert_eq!(cells[0], None, "{}", drilled.sql);
                    continue;
                };
                checks.push(format!("({}, {}, {literal})", cells[2].as_deref().unwrap(), cells[1].as_deref().unwrap()));
                // A sample of the drills runs end to end.
                if row % 97 == 0 {
                    let behind = h.sql(rows_tab, &drilled.sql).await.remove(0);
                    assert_eq!(Some(behind.rows_seen.to_string()), cells[1], "{}", drilled.sql);
                }
            }
            assert!(checks.len() + 1 >= rows.len());
            // The drill's comparison must find the group's own first row and exactly as many rows as the group.
            for batch in checks.chunks(500) {
                let check = format!(
                    "SELECT COUNT(*) AS bad FROM (VALUES {}) v(lo, n, x)
                     WHERE (SELECT COUNT(*) FROM dbo.[{t}] t WHERE t.{column} = v.x) <> v.n
                        OR NOT EXISTS (SELECT 1 FROM dbo.[{t}] t WHERE t.id = v.lo AND t.{column} = v.x)",
                    batch.join(", ")
                );
                let bad = h.sql(tab, &check).await.remove(0);
                assert_eq!(h.rows(&bad)[0][0].as_deref(), Some("0"), "{column}: {check}");
            }
            eprintln!("{column}: {} groups drill exactly", checks.len());
        }
    })
    .catch_unwind()
    .await;
    h.sql(tab, &format!("DROP TABLE IF EXISTS dbo.[{t}]")).await;
    h.engine.shutdown().await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}
