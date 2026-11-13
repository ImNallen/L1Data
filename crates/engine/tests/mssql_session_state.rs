#![cfg(feature = "mssql-tests")]

use engine::model::*;
use engine::{AgentPath, Engine, EngineConfig, MemoryKeyStore};
use futures_util::{FutureExt, future::BoxFuture};
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

type Client = tiberius::Client<Compat<TcpStream>>;
type Rows = Vec<Vec<Option<String>>>;

fn address() -> (String, u16) {
    (
        std::env::var("L1_DATA_TEST_MSSQL_HOST").unwrap_or_else(|_| "localhost".into()),
        std::env::var("L1_DATA_TEST_MSSQL_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(1434),
    )
}

fn password() -> String {
    std::env::var("L1_DATA_TEST_MSSQL_PASSWORD").unwrap_or_else(|_| "L1Data_dev_Str0ng".into())
}

async fn raw(user: &str, password: &str, database: &str) -> Client {
    let (host, port) = address();
    let mut config = tiberius::Config::new();
    config.host(&host);
    config.port(port);
    config.database(database);
    config.application_name("L1 Data");
    config.authentication(tiberius::AuthMethod::sql_server(user, password));
    config.encryption(tiberius::EncryptionLevel::Required);
    config.trust_cert();
    let tcp = TcpStream::connect((host.as_str(), port)).await.unwrap();
    tiberius::Client::connect(config, tcp.compat_write()).await.unwrap()
}

async fn raw_rows(client: &mut Client, sql: &str) -> Result<Rows, tiberius::error::Error> {
    let sets = client.simple_query(sql).await?.into_results().await?;
    Ok(sets
        .into_iter()
        .flatten()
        .map(|row| (0..row.len()).map(|i| row.get::<&str, _>(i).map(str::to_owned)).collect())
        .collect())
}

struct Harness {
    engine: Arc<Engine>,
    tab: TabId,
    control: Client,
    _dir: tempfile::TempDir,
}

impl Harness {
    async fn open(user: &str, password: &str, database: &str) -> Self {
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
        let (host, port) = address();
        let connection = engine
            .save_connection(ConnectionDraft {
                id: None,
                database_kind: DatabaseKind::SqlServer,
                name: "Native state regression".into(),
                host,
                port,
                database: database.into(),
                user: user.into(),
                password: PasswordInput::Set(password.into()),
                ssl_mode: SslMode::Require,
                ca_cert_path: None,
                color: None,
                ssh: None,
                environment: None,
                sql_server_auth: None,
            })
            .await
            .unwrap();
        let tab = TabId(uuid::Uuid::new_v4());
        engine
            .console_configure(
                tab,
                ConsoleConfig {
                    connection_id: connection.id,
                    database: database.into(),
                    schema: None,
                    mode: TxnMode::AutoCommit,
                },
            )
            .await
            .unwrap();
        Self { engine, tab, control: raw(user, password, database).await, _dir: dir }
    }

    fn state(&self) -> ConsoleState {
        self.engine.boot().consoles.into_iter().find(|state| state.tab_id == self.tab).unwrap()
    }

    async fn finish(&self, id: RunId) -> ConsoleState {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let state = self.state();
                if matches!(state.run, RunState::Finished { run_id, .. } if run_id == id) {
                    return state;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("Native state regression timed out")
    }

    async fn run(&self, sql: &str) -> (ConsoleState, Rows) {
        let accepted = self
            .engine
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
            .unwrap();
        let state = self.finish(accepted.run_id).await;
        let RunState::Finished { statements, .. } = &state.run else { unreachable!() };
        let rows = statements
            .iter()
            .flat_map(|s| &s.outputs)
            .filter_map(|output| match output {
                RunOutput::Rows { result, .. } => Some(self.engine.result_page(result.r#ref, 0, 100).unwrap().rows),
                _ => None,
            })
            .flatten()
            .collect();
        (state, rows)
    }

    async fn same(&mut self, sql: &str) -> ConsoleState {
        let expected = raw_rows(&mut self.control, sql).await.unwrap();
        let (state, actual) = self.run(sql).await;
        assert!(matches!(state.run, RunState::Finished { outcome: RunOutcome::Ok, .. }), "{sql}: {:?}", state.run);
        assert_eq!(actual, expected, "Native effect differs for {sql}");
        state
    }

    async fn cleanup(mut self) {
        let sql = "IF @@TRANCOUNT > 0 ROLLBACK; DROP TABLE IF EXISTS #l1_native_state; SELECT CONVERT(nvarchar(20),OBJECT_ID('tempdb..#l1_native_state'))";
        let expected = vec![vec![None]];
        assert_eq!(raw_rows(&mut self.control, sql).await.unwrap(), expected);
        let (state, rows) = self.run(sql).await;
        assert!(matches!(state.run, RunState::Finished { outcome: RunOutcome::Ok, .. }), "{:?}", state.run);
        assert_eq!(rows, expected);
        let connection = self.state().config.connection_id;
        self.control.close().await.unwrap();
        self.engine.console_close(self.tab, true).await.unwrap();
        self.engine.disconnect(connection, true).await.unwrap();
        self.engine.shutdown().await;
    }
}

async fn check(test: impl for<'a> FnOnce(&'a mut Harness) -> BoxFuture<'a, ()>) {
    let mut h = Harness::open("sa", &password(), "shop").await;
    let result = AssertUnwindSafe(test(&mut h)).catch_unwind().await;
    h.cleanup().await;
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

const SETUP: &str = "DROP TABLE IF EXISTS #l1_native_state; CREATE TABLE #l1_native_state(id int PRIMARY KEY, n int); INSERT #l1_native_state VALUES(1,10),(2,20)";
const EFFECTS: &str = "SELECT CONVERT(nvarchar(20),@@ROWCOUNT), CONVERT(nvarchar(20),ROWCOUNT_BIG()), CONVERT(nvarchar(20),@@ERROR), CONVERT(nvarchar(20),@@OPTIONS), CONVERT(nvarchar(40),@@IDENTITY)";

fn transaction(state: &ConsoleState, open: bool) {
    assert_eq!((state.txn, state.txn_depth), if open { (TxnState::Open, None) } else { (TxnState::Idle, Some(0)) });
}

#[tokio::test]
async fn startup_and_protocol_reads_preserve_native_state() {
    check(|h| {
        async move {
            let login = h.control.session_state().unwrap();
            assert_eq!(login.database, "shop");
            assert!(!login.has_transaction);
            let pid = login.server_process_id;
            let version = login.server_version;
            for _ in 0..3 {
                h.control.session_state().unwrap();
            }
            transaction(&h.same(EFFECTS).await, false);
            let actual = raw_rows(
                &mut h.control,
                "SELECT CONVERT(nvarchar(20),@@SPID), CONVERT(nvarchar(128),SERVERPROPERTY('ProductVersion'))",
            )
            .await
            .unwrap();
            assert_eq!(actual[0][0], Some(pid.to_string()));
            assert!(actual[0][1].as_ref().unwrap().starts_with(&format!("{}.{}.{}.", version.0, version.1, version.2)));
            let (state, rows) = h.run("SELECT CONVERT(nvarchar(20),@@SPID)").await;
            let SessionState::Open { backend_pid } = state.session else { panic!("Expected native session") };
            assert_eq!(rows, vec![vec![Some(backend_pid.to_string())]]);
        }
        .boxed()
    })
    .await;
}

#[tokio::test]
async fn go_and_consecutive_runs_match_raw_guarded_writes() {
    check(|h| async move {
        let cases = [
            ("UPDATE #l1_native_state SET n=99 WHERE id=99", EFFECTS),
            ("UPDATE #l1_native_state SET n=n+1", EFFECTS),
            ("UPDATE #l1_native_state SET n=99 WHERE id=99", "IF @@ROWCOUNT>0 DELETE #l1_native_state WHERE id=1; SELECT CONVERT(nvarchar(20),COUNT(*)) FROM #l1_native_state WHERE id=1"),
            ("UPDATE #l1_native_state SET n=99 WHERE id=99", "IF @@ROWCOUNT=0 INSERT #l1_native_state VALUES(3,30); SELECT CONVERT(nvarchar(20),COUNT(*)) FROM #l1_native_state"),
            ("UPDATE #l1_native_state SET n=n+1", "IF @@ROWCOUNT=2 DELETE #l1_native_state WHERE id=1; SELECT CONVERT(nvarchar(20),COUNT(*)) FROM #l1_native_state WHERE id=1"),
        ];
        for go in [false, true] {
            for (first, second) in cases {
                h.same(SETUP).await;
                if go {
                    raw_rows(&mut h.control, first).await.unwrap();
                    let expected = raw_rows(&mut h.control, second).await.unwrap();
                    let (state, actual) = h.run(&format!("{first}\nGO\n{second}")).await;
                    assert!(matches!(state.run, RunState::Finished { outcome: RunOutcome::Ok, .. }));
                    assert_eq!(actual, expected, "GO changed native branch for {first}; {second}");
                } else {
                    h.same(first).await;
                    h.same(second).await;
                }
                h.same("SELECT CONVERT(nvarchar(20),id), CONVERT(nvarchar(20),n) FROM #l1_native_state ORDER BY id").await;
            }
        }
    }.boxed()).await;
}

#[tokio::test]
async fn server_errors_preserve_error_number_and_recover_without_observation_sql() {
    check(|h| async move {
        for sql in ["SELECT CONVERT(int,N'not an int')", "RAISERROR(N'native error',16,1)"] {
            assert!(raw_rows(&mut h.control, sql).await.is_err());
            let (state, _) = h.run(sql).await;
            assert!(matches!(state.run, RunState::Finished { outcome: RunOutcome::Failed, .. }));
            assert!(matches!(state.session, SessionState::Open { .. }));
            h.same(EFFECTS).await;
            h.same("SELECT N'recovered'").await;
        }
        h.same(SETUP).await;
        transaction(&h.same("BEGIN TRANSACTION; BEGIN TRANSACTION").await, true);
        let sql = "RAISERROR(N'keeps the native transaction',16,1)";
        assert!(raw_rows(&mut h.control, sql).await.is_err());
        let (state, _) = h.run(sql).await;
        transaction(&state, true);
        h.same(EFFECTS).await;
        h.same("ROLLBACK TRANSACTION").await;
        h.same("SET XACT_ABORT ON; BEGIN TRANSACTION; BEGIN TRANSACTION").await;
        let sql = "SELECT CONVERT(int,N'bad')";
        assert!(raw_rows(&mut h.control, sql).await.is_err());
        let (state, _) = h.run(sql).await;
        transaction(&state, false);
        h.same("SELECT CONVERT(nvarchar(20),@@ERROR),CONVERT(nvarchar(20),@@TRANCOUNT),CONVERT(nvarchar(20),XACT_STATE())").await;
    }.boxed()).await;
}

#[tokio::test]
async fn native_buttons_keep_nesting_semantics_and_do_not_run_a_followup_probe() {
    check(|h| {
        async move {
            h.same(SETUP).await;
            transaction(&h.same("BEGIN TRANSACTION; BEGIN TRANSACTION; UPDATE #l1_native_state SET n=99").await, true);
            transaction(&h.same("COMMIT TRANSACTION").await, true);
            raw_rows(&mut h.control, "WHILE @@TRANCOUNT > 0 COMMIT TRANSACTION").await.unwrap();
            h.engine.console_commit(h.tab, None).await.unwrap();
            transaction(&h.state(), false);
            h.same(EFFECTS).await;
            h.same("SELECT CONVERT(nvarchar(20),SUM(n)) FROM #l1_native_state").await;
            transaction(&h.same("BEGIN TRANSACTION; BEGIN TRANSACTION; UPDATE #l1_native_state SET n=7").await, true);
            raw_rows(&mut h.control, "IF @@TRANCOUNT > 0 ROLLBACK TRANSACTION").await.unwrap();
            h.engine.console_rollback(h.tab).await.unwrap();
            transaction(&h.state(), false);
            h.same(EFFECTS).await;
            h.same("SELECT CONVERT(nvarchar(20),SUM(n)) FROM #l1_native_state").await;
            transaction(&h.same("SET IMPLICIT_TRANSACTIONS ON; UPDATE #l1_native_state SET n=8").await, true);
            h.same("ROLLBACK TRANSACTION; SET IMPLICIT_TRANSACTIONS OFF").await;
        }
        .boxed()
    })
    .await;
}

#[tokio::test]
async fn failed_statements_drain_the_original_response_and_scoped_use_stays_scoped() {
    check(|h| async move {
        let (state, _) = h.run("RAISERROR('expected failure',16,1); SELECT REPLICATE(CAST('x' AS varchar(max)),9000); BEGIN TRANSACTION").await;
        assert!(matches!(state.run, RunState::Finished { outcome: RunOutcome::Failed, .. }));
        transaction(&state, true);
        assert!(matches!(state.session, SessionState::Open { .. }));
        h.engine.console_rollback(h.tab).await.unwrap();
        let (state, _) = h.run("EXEC(N'USE reporting; SELECT DB_NAME()')").await;
        assert!(matches!(state.run, RunState::Finished { outcome: RunOutcome::Ok, .. }));
        assert!(matches!(state.session, SessionState::Open { .. }));
        h.same("SELECT DB_NAME()").await;
    }.boxed()).await;
}

#[tokio::test]
async fn a_login_without_server_state_permission_preserves_native_effects() {
    let mut admin = raw("sa", &password(), "master").await;
    let login = format!("l1_state_{}", uuid::Uuid::new_v4().simple());
    let secret = format!("L1_test_{}!", uuid::Uuid::new_v4().simple());
    raw_rows(&mut admin, &format!("CREATE LOGIN [{login}] WITH PASSWORD=N'{secret}', DEFAULT_DATABASE=master"))
        .await
        .unwrap();
    let result = AssertUnwindSafe(async {
        let mut h = Harness::open(&login, &secret, "master").await;
        let test = AssertUnwindSafe(async {
            let pid = h.control.session_state().unwrap().server_process_id;
            let mut observer = raw(&login, &secret, "master").await;
            let observed = raw_rows(&mut observer, &format!("SELECT CONVERT(nvarchar(20),COUNT(*)) FROM sys.dm_exec_sessions WHERE session_id={pid}")).await.unwrap();
            observer.close().await.unwrap();
            assert_eq!(observed, vec![vec![Some("0".into())]], "A same-login observer must not require access to the native session");
            h.same(EFFECTS).await;
            let (state, permissions) = h.run("SELECT CONVERT(nvarchar(20),IS_SRVROLEMEMBER('sysadmin')),CONVERT(nvarchar(20),HAS_PERMS_BY_NAME(NULL,NULL,'VIEW SERVER STATE')),CONVERT(nvarchar(20),HAS_PERMS_BY_NAME(NULL,NULL,'VIEW SERVER PERFORMANCE STATE'))").await;
            assert!(matches!(state.run, RunState::Finished { outcome: RunOutcome::Ok, .. }));
            assert_eq!(permissions, vec![vec![Some("0".into()), Some("0".into()), Some("0".into())]]);
            h.same(SETUP).await;
            raw_rows(&mut h.control, "UPDATE #l1_native_state SET n=99 WHERE id=99").await.unwrap();
            let second = "IF @@ROWCOUNT>0 DELETE #l1_native_state WHERE id=1; SELECT CONVERT(nvarchar(20),COUNT(*)) FROM #l1_native_state WHERE id=1";
            let expected = raw_rows(&mut h.control, second).await.unwrap();
            let (state, actual) = h.run(&format!("UPDATE #l1_native_state SET n=99 WHERE id=99\nGO\n{second}")).await;
            assert!(matches!(state.run, RunState::Finished { outcome: RunOutcome::Ok, .. }));
            assert_eq!(actual, expected);
            transaction(&h.same("BEGIN TRANSACTION; BEGIN TRANSACTION").await, true);
            h.same("ROLLBACK TRANSACTION").await;
        }).catch_unwind().await;
        h.cleanup().await;
        if let Err(panic) = test { std::panic::resume_unwind(panic); }
    }).catch_unwind().await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let sessions = raw_rows(
                &mut admin,
                &format!("SELECT CONVERT(nvarchar(20),COUNT(*)) FROM sys.dm_exec_sessions WHERE login_name=N'{login}'"),
            )
            .await
            .unwrap();
            if sessions == vec![vec![Some("0".into())]] {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("Disposable login sessions did not close");
    raw_rows(&mut admin, &format!("DROP LOGIN [{login}]")).await.unwrap();
    assert_eq!(
        raw_rows(
            &mut admin,
            &format!("SELECT CONVERT(nvarchar(20),COUNT(*)) FROM sys.server_principals WHERE name=N'{login}'")
        )
        .await
        .unwrap(),
        vec![vec![Some("0".into())]]
    );
    admin.close().await.unwrap();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test]
async fn cancel_retires_the_moved_session_and_transaction_buttons_remain_busy() {
    check(|h| {
        async move {
            h.same(SETUP).await;
            h.same("BEGIN TRANSACTION; UPDATE #l1_native_state SET n=9").await;
            let accepted = h
                .engine
                .console_run(
                    h.tab,
                    RunRequest::Script {
                        text: "SELECT N'started'; RAISERROR(N'flush',0,1) WITH NOWAIT; WAITFOR DELAY '00:00:30'".into(),
                        target: RunTarget::All,
                        mode: RunMode::Run,
                        params: Vec::new(),
                        cells: Vec::new(),
                    },
                    None,
                )
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    if let RunState::Running { ref statements, .. } = h.state().run
                        && statements.iter().flat_map(|s| &s.outputs).any(|o| matches!(o, RunOutput::Rows { .. }))
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("First result did not stream before WAITFOR");
            assert!(matches!(h.engine.console_commit(h.tab, None).await, Err(TxnError::Busy)));
            assert!(matches!(h.engine.console_rollback(h.tab).await, Err(TxnError::Busy)));
            assert!(matches!(
                h.engine
                    .console_run(
                        h.tab,
                        RunRequest::Script {
                            text: "SELECT 1".into(),
                            target: RunTarget::All,
                            mode: RunMode::Run,
                            params: Vec::new(),
                            cells: Vec::new()
                        },
                        None
                    )
                    .await,
                Err(RunError::Busy)
            ));
            h.engine.console_cancel(h.tab).await;
            let state = h.finish(accepted.run_id).await;
            assert!(matches!(state.run, RunState::Finished { outcome: RunOutcome::Cancelled, .. }));
            assert!(matches!(state.session, SessionState::Lost { transaction: TransactionLoss::Unknown, .. }));
            let (_, rows) = h.run("SELECT CONVERT(nvarchar(20),OBJECT_ID('tempdb..#l1_native_state'))").await;
            assert_eq!(rows, vec![vec![None]]);
        }
        .boxed()
    })
    .await;
}
