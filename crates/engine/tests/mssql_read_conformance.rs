//! A permanent conformance check for the assistant's SQL Server read allowlist (`tsql::read`),
//! against SQL Server's own parser. It compiles each text the allowlist admits under
//! `SET SHOWPLAN_XML ON`, which compiles without executing, on a throwaway scratch database as a
//! `db_owner` login, and asserts the server parses it as exactly one read statement. The corpus is a
//! fixture (`fixtures/mssql_read_conformance.jsonl`): the texts the server split into two statements
//! that an earlier allowlist admitted, every statement keyword after a plain `SELECT`, the PR's valid
//! reads, and a deterministic sample of recombinations.
//!
//! Behind `mssql-tests`, like the other `mssql_*` integration tests. Needs the dev SQL Server
//! (`pnpm db:up`) on `localhost:1434`. It creates and drops its own `l1rc_*` database and login and
//! touches nothing else.
#![cfg(feature = "mssql-tests")]
use std::panic::AssertUnwindSafe;
use std::time::Instant;

use futures_util::{FutureExt, TryStreamExt};
use quick_xml::events::{BytesStart, Event};
use serde_json::Value;
use tiberius::{AuthMethod, Client, ColumnData, Config, EncryptionLevel, QueryItem};
use tokio::net::TcpStream;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

type C = Client<Compat<TcpStream>>;

const PW: &str = "L1rc_Str0ng_Pw";

fn env(k: &str, d: &str) -> String {
    std::env::var(k).unwrap_or_else(|_| d.to_owned())
}
fn sa_pw() -> String {
    env("L1_DATA_TEST_MSSQL_PASSWORD", "L1Data_dev_Str0ng")
}

/// The same driver options the assistant's session uses (`session::mssql::config`).
async fn connect(db: &str, user: &str, pw: &str) -> C {
    let mut c = Config::new();
    c.host(env("L1_DATA_TEST_MSSQL_HOST", "localhost"));
    c.port(env("L1_DATA_TEST_MSSQL_PORT", "1434").parse().unwrap());
    c.database(db);
    c.application_name("L1 Data");
    c.command_timeout(None);
    c.authentication(AuthMethod::sql_server(user, pw));
    c.encryption(EncryptionLevel::Required);
    c.trust_cert();
    let tcp = TcpStream::connect(c.get_addr()).await.unwrap();
    tcp.set_nodelay(true).unwrap();
    Client::connect(c, tcp.compat_write()).await.unwrap()
}

#[derive(Default)]
struct Outcome {
    /// Top-level statement types, e.g. `SELECT`, `FETCH CURSOR`, `UPDATE`.
    stmts: Vec<String>,
    errors: Vec<u32>,
    /// A cell that was not a showplan document, which would mean the batch executed.
    other: Vec<String>,
}

impl Outcome {
    /// Whether the batch compiled to exactly one read statement and nothing executed.
    fn one_read(&self) -> bool {
        const READS: &[&str] = &["SELECT", "SELECT WITHOUT QUERY", "XML SELECT", "JSON SELECT"];
        self.errors.is_empty()
            && self.other.is_empty()
            && self.stmts.len() == 1
            && READS.contains(&self.stmts[0].as_str())
    }
}

fn attr(e: &BytesStart<'_>, name: &str) -> String {
    match e.try_get_attribute(name) {
        Ok(Some(a)) => a.unescape_value().map(|v| v.into_owned()).unwrap_or_default(),
        _ => String::new(),
    }
}

/// Collects the top-level statement types from one `<ShowPlanXML>` document. A `Stmt*` element is
/// top-level when it is a batch's direct statement, not one nested inside another's plan.
fn plan(s: &str, o: &mut Outcome) {
    let mut r = quick_xml::Reader::from_str(s);
    let mut stack: Vec<Vec<u8>> = Vec::new();
    loop {
        let (e, start) = match r.read_event() {
            Ok(Event::Start(e)) => (e, true),
            Ok(Event::Empty(e)) => (e, false),
            Ok(Event::End(_)) => {
                stack.pop();
                continue;
            }
            Ok(Event::Eof) => break,
            Ok(_) => continue,
            Err(err) => {
                o.other.push(format!("bad showplan xml: {err}"));
                break;
            }
        };
        let name = e.local_name().as_ref().to_vec();
        if name.starts_with(b"Stmt") && stack.len() == 4 && stack[2] == b"Batch" && stack[3] == b"Statements" {
            o.stmts.push(attr(&e, "StatementType"));
        }
        if start {
            stack.push(name);
        }
    }
}

/// Runs one batch and records its plans, errors, and any stray cell. `Err` is a lost connection.
async fn run(c: &mut C, sql: &str) -> Result<Outcome, String> {
    let mut o = Outcome::default();
    let stream = match c.simple_query(sql).await {
        Ok(s) => s,
        Err(tiberius::error::Error::Server(e)) => {
            o.errors.push(e.code());
            return Ok(o);
        }
        Err(e) => return Err(e.to_string()),
    };
    let mut stream = std::pin::pin!(stream);
    loop {
        match stream.try_next().await {
            Ok(Some(QueryItem::Row(row))) => {
                for (_, d) in row.cells() {
                    match d {
                        ColumnData::String(Some(s)) if s.starts_with("<ShowPlanXML") => plan(s, &mut o),
                        ColumnData::Xml(Some(x)) if x.as_ref().as_ref().starts_with("<ShowPlanXML") => {
                            plan(x.as_ref().as_ref(), &mut o)
                        }
                        other => o.other.push(format!("{other:?}")),
                    }
                }
            }
            Ok(Some(QueryItem::Error(e))) => o.errors.push(e.code()),
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(tiberius::error::Error::Server(e)) => {
                o.errors.push(e.code());
                break;
            }
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(o)
}

async fn exec(c: &mut C, sql: &str) {
    let o = run(c, sql).await.unwrap();
    assert!(o.errors.is_empty(), "admin batch failed ({:?}): {sql}", o.errors);
}

async fn scalar(c: &mut C, sql: &str) -> String {
    let row = c.simple_query(sql).await.unwrap().into_row().await.unwrap().expect("a row");
    row.cells().map(|(_, d)| format!("{d:?}")).collect::<Vec<_>>().join(" | ")
}

async fn setup(sa: &mut C, db: &str, login: &str) {
    exec(sa, &format!("CREATE DATABASE [{db}]")).await;
    exec(sa, &format!("CREATE LOGIN [{login}] WITH PASSWORD = N'{PW}', CHECK_POLICY = OFF")).await;
    let mut s = connect(db, "sa", &sa_pw()).await;
    exec(
        &mut s,
        "CREATE TABLE dbo.t (id int NOT NULL PRIMARY KEY, x int NULL, y int NULL, a int NULL, b int NULL, \
         name nvarchar(50) NULL, note ntext NULL, total decimal(10, 2) NULL, receive int NULL, [move] int NULL, \
         [get] int NULL, send int NULL, d datetime2 NULL, j nvarchar(max) NULL); \
         INSERT dbo.t (id, x, y, a, b, name, total) SELECT value, value, value % 2 + 1, value, value, CONCAT(N'n', value), value \
         FROM GENERATE_SERIES(1, 60); \
         CREATE TABLE dbo.u (id int NOT NULL, send int NULL, a int NULL); INSERT dbo.u VALUES (1, 1, 1); \
         CREATE TABLE dbo.a (id int NOT NULL); CREATE TABLE dbo.b (id int NOT NULL); \
         CREATE TABLE dbo.orders (id int NOT NULL PRIMARY KEY, total decimal(10, 2) NULL, customer_id int NULL); \
         CREATE TABLE dbo.customers (id int NOT NULL PRIMARY KEY, name nvarchar(50) NULL); \
         CREATE TABLE dbo.t2 (id int IDENTITY NOT NULL, a int NULL, z int NULL, g uniqueidentifier ROWGUIDCOL NOT NULL DEFAULT NEWID()); \
         CREATE TABLE dbo.log (n int IDENTITY NOT NULL, what nvarchar(100) NOT NULL); \
         CREATE SEQUENCE dbo.seq AS bigint START WITH 1; \
         CREATE TABLE dbo.tt (id int NOT NULL PRIMARY KEY, s datetime2 GENERATED ALWAYS AS ROW START NOT NULL, \
         e datetime2 GENERATED ALWAYS AS ROW END NOT NULL, PERIOD FOR SYSTEM_TIME (s, e)) \
         WITH (SYSTEM_VERSIONING = ON (HISTORY_TABLE = dbo.tt_history)); \
         CREATE QUEUE dbo.q;",
    )
    .await;
    exec(
        &mut s,
        "CREATE TRIGGER dbo.tr ON dbo.t2 AFTER INSERT, UPDATE, DELETE AS INSERT dbo.log (what) VALUES (N'trigger')",
    )
    .await;
    exec(&mut s, "CREATE PROCEDURE dbo.p AS INSERT dbo.log (what) VALUES (N'proc')").await;
    exec(&mut s, "CREATE VIEW dbo.v AS SELECT id FROM dbo.t").await;
    exec(&mut s, &format!("CREATE USER [{login}] FOR LOGIN [{login}]; ALTER ROLE db_owner ADD MEMBER [{login}];"))
        .await;
}

/// The counts a write would change: trigger-logged rows, the sequence, and the base table.
async fn state(db: &str) -> String {
    let mut s = connect(db, "sa", &sa_pw()).await;
    scalar(
        &mut s,
        "SELECT (SELECT COUNT(*) FROM dbo.log), (SELECT COUNT(*) FROM dbo.t2), (SELECT COUNT(*) FROM dbo.t), \
         (SELECT CAST(current_value AS bigint) FROM sys.sequences WHERE name = N'seq')",
    )
    .await
}

async fn teardown(sa: &mut C, db: &str, login: &str) {
    let _ = run(
        sa,
        &format!(
            "DECLARE @k nvarchar(max) = N''; SELECT @k += N'KILL ' + CAST(session_id AS nvarchar(10)) + N';' \
             FROM sys.dm_exec_sessions WHERE login_name = N'{login}'; EXEC (@k);"
        ),
    )
    .await;
    let _ = run(
        sa,
        &format!(
            "IF DB_ID(N'{db}') IS NOT NULL BEGIN ALTER DATABASE [{db}] SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE [{db}]; END; \
             IF SUSER_ID(N'{login}') IS NOT NULL DROP LOGIN [{login}];"
        ),
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_read_allowlist_admits_only_a_single_select_the_server_parses() {
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let db = format!("l1rc_{}", &suffix[..12]);
    let login = format!("l1rc_l_{}", &suffix[..12]);
    let corpus: Vec<Value> =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/mssql_read_conformance.jsonl"))
            .unwrap()
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
    let mut sa = connect("master", "sa", &sa_pw()).await;

    let result = AssertUnwindSafe(async {
        setup(&mut sa, &db, &login).await;
        let before = state(&db).await;

        // A `db_owner` login on the scratch database, a global cursor so a `FETCH` would compile, and
        // SHOWPLAN on so every batch compiles without executing.
        let mut c = connect(&db, &login, PW).await;
        exec(&mut c, "DECLARE c CURSOR GLOBAL FOR SELECT id FROM dbo.t; OPEN c;").await;
        exec(&mut c, "SET SHOWPLAN_XML ON").await;

        // SHOWPLAN executes nothing: a trigger-tracked insert, a sequence read, a write, and DDL all
        // return a plan and change no state.
        for sql in [
            "INSERT dbo.t2 (a) VALUES (1)",
            "SELECT NEXT VALUE FOR dbo.seq",
            "SELECT 1 FROM dbo.t UPDATE dbo.t2 SET a = 2",
            "DELETE FROM dbo.t",
            "SELECT 1 FROM dbo.t DROP TABLE dbo.u",
            "EXEC dbo.p",
        ] {
            run(&mut c, sql).await.unwrap();
        }
        assert_eq!(state(&db).await, before, "SHOWPLAN executed something");

        // Every text the allowlist admits must compile to exactly one read. A compile error means the
        // text could not bind on this fixture (an undetermined case), never a second statement, so it
        // is skipped. The admitted texts are pure reads, so the SHOWPLAN session needs no reset.
        let started = Instant::now();
        let (mut admitted, mut compiled, mut bad) = (0u32, 0u32, Vec::new());
        for row in &corpus {
            let sql = row["sql"].as_str().unwrap().replace("__DB__", &db);
            if engine::assistant_tsql_read_refusal(&sql).is_some() {
                continue;
            }
            admitted += 1;
            let o = run(&mut c, &sql).await.expect("SHOWPLAN connection lost");
            if o.errors.is_empty() && o.other.is_empty() {
                compiled += 1;
                if !o.one_read() {
                    bad.push(format!("{:?} -> {:?}", sql, o.stmts));
                }
            }
        }
        drop(c);
        assert_eq!(state(&db).await, before, "the corpus changed the scratch database");
        eprintln!(
            "{} fixture texts, {admitted} admitted, {compiled} compiled, {} not one read, in {:?}",
            corpus.len(),
            bad.len(),
            started.elapsed()
        );
        assert!(
            bad.is_empty(),
            "the allowlist admitted a text the server does not parse as one read:\n{}",
            bad.join("\n")
        );
    })
    .catch_unwind()
    .await;

    teardown(&mut sa, &db, &login).await;
    if let Err(p) = result {
        std::panic::resume_unwind(p);
    }
}
