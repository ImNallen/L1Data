//! The assistant's read-only query path on SQL Server, against a real server. It makes a uniquely
//! named database, fills it, marks it `READ_ONLY`, and makes a minimal login, drives `run_query`
//! through the engine's assistant with a scripted provider, and drops everything on the way out, even
//! on failure. Behind `mssql-tests`, like the other `mssql_*` integration tests.
#![cfg(feature = "mssql-tests")]
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use engine::model::*;
use engine::{AgentPath, Engine, EngineConfig, MemoryKeyStore};
use futures_util::FutureExt;
use tokio::sync::broadcast;

const FAKE: &str = env!("CARGO_BIN_EXE_fake-claude");
const WAIT: Duration = Duration::from_secs(30);
const LOGIN_PASSWORD: &str = "L1m_Asst_Str0ng_Pw";

fn host() -> String {
    std::env::var("L1_DATA_TEST_MSSQL_HOST").unwrap_or_else(|_| "localhost".into())
}
fn port() -> u16 {
    std::env::var("L1_DATA_TEST_MSSQL_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(1434)
}
fn sa_password() -> String {
    std::env::var("L1_DATA_TEST_MSSQL_PASSWORD").unwrap_or_else(|_| "L1Data_dev_Str0ng".into())
}

struct Harness {
    engine: Arc<Engine>,
    events: broadcast::Receiver<AppEvent>,
    sa_tab: TabId,
    db: String,
    login: String,
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
        engine.set_settings(Settings { assistant_access: AssistantAccess::Full, ..engine.settings() }).unwrap();
        let mut p = engine.settings().providers;
        p[0].binary_path = FAKE.into();
        engine.set_settings(Settings { providers: p, ..engine.settings() }).unwrap();
        let events = engine.subscribe();
        // The `sa` connection runs the admin DDL from `master`.
        let sa = engine.save_connection(draft("master", "sa", &sa_password(), false, "sa admin")).await.unwrap();
        engine.connect(sa.id).await.unwrap();
        let sa_tab = TabId(uuid::Uuid::new_v4());
        engine
            .console_configure(
                sa_tab,
                ConsoleConfig {
                    connection_id: sa.id,
                    database: "master".into(),
                    schema: None,
                    mode: TxnMode::AutoCommit,
                },
            )
            .await
            .unwrap();
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        Self {
            engine,
            events,
            sa_tab,
            db: format!("l1m_asst_{}", &suffix[..12]),
            login: format!("l1m_asst_login_{}", &suffix[..12]),
            _dir: dir,
        }
    }

    /// Runs one admin batch as `sa` on `master` and asserts it finished without error.
    async fn admin(&mut self, sql: &str) {
        let accepted = self
            .engine
            .console_run(
                self.sa_tab,
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
        let state = self.finish(self.sa_tab, accepted.run_id).await;
        assert!(
            matches!(state.run, RunState::Finished { outcome: RunOutcome::Ok, .. }),
            "admin failed: {sql}\n{state:?}"
        );
    }

    async fn finish(&mut self, tab: TabId, run_id: RunId) -> ConsoleState {
        tokio::time::timeout(WAIT, async {
            loop {
                if let AppEvent::ConsoleChanged(state) = self.events.recv().await.unwrap()
                    && state.tab_id == tab
                    && matches!(state.run, RunState::Finished { run_id: r, .. } if r == run_id)
                {
                    return state;
                }
            }
        })
        .await
        .expect("admin run did not finish")
    }

    /// Creates the scratch database, seeds it past the row cap, makes the minimal login, and marks the
    /// database `READ_ONLY`.
    async fn setup(&mut self) {
        let db = self.db.clone();
        let login = self.login.clone();
        self.admin(&format!("CREATE DATABASE [{db}]")).await;
        self.admin(&format!("CREATE LOGIN [{login}] WITH PASSWORD = N'{LOGIN_PASSWORD}', CHECK_POLICY = OFF")).await;
        let seed = format!(
            "CREATE TABLE dbo.t (id int NOT NULL, name nvarchar(50) NULL); \
             INSERT dbo.t (id, name) SELECT value, CONCAT(N'n', value) FROM GENERATE_SERIES(1, 60); \
             CREATE SEQUENCE dbo.seq AS bigint START WITH 1; \
             CREATE USER [{login}] FOR LOGIN [{login}]; \
             GRANT SELECT ON dbo.t TO [{login}]; \
             GRANT VIEW DEFINITION TO [{login}];"
        );
        self.admin(&format!("EXEC [{db}].sys.sp_executesql N'{}'", seed.replace('\'', "''"))).await;
        self.admin(&format!("ALTER DATABASE [{db}] SET READ_ONLY WITH NO_WAIT")).await;
    }

    async fn teardown(&mut self) {
        let db = self.db.clone();
        let login = self.login.clone();
        // Best effort, in order, so a half-made fixture still clears.
        let _ = self.try_admin(&format!(
            "IF DB_ID(N'{db}') IS NOT NULL BEGIN ALTER DATABASE [{db}] SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE [{db}]; END"
        ))
        .await;
        let _ = self.try_admin(&format!("IF SUSER_ID(N'{login}') IS NOT NULL DROP LOGIN [{login}]")).await;
    }

    async fn try_admin(&mut self, sql: &str) -> Result<(), ()> {
        let accepted = self
            .engine
            .console_run(
                self.sa_tab,
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
            .map_err(|_| ())?;
        self.finish(self.sa_tab, accepted.run_id).await;
        Ok(())
    }

    /// A connection as the minimal login to the scratch database, with the read-only assistant setting
    /// on or off.
    async fn login_connection(&self, read_only: bool, name: &str) -> ConnectionId {
        let config =
            self.engine.save_connection(draft(&self.db, &self.login, LOGIN_PASSWORD, read_only, name)).await.unwrap();
        self.engine.connect(config.id).await.unwrap();
        config.id
    }

    fn chat(&self, permission: Permission) -> ChatId {
        let id = ChatId(uuid::Uuid::new_v4());
        self.engine
            .assistant_configure(
                id,
                ChatOptions { provider: ProviderKind::Claude, model: ModelId("opus".into()), effort: None, permission },
            )
            .unwrap();
        id
    }

    fn fake_sql(&self, sql: &str) {
        let mut settings = self.engine.settings();
        settings.providers[0].launch_args = format!("--fake-scenario query --fake-sql \"{sql}\"");
        self.engine.set_settings(settings).unwrap();
    }

    async fn send(&self, chat: ChatId, connection: ConnectionId) {
        let tab = TabContext {
            tab_id: TabId(uuid::Uuid::new_v4()),
            title: "assistant.sql".into(),
            db: Some(DatabaseRef { connection_id: connection, database: self.db.clone() }),
            text: String::new(),
            target: RunTarget::All,
            attach: None,
        };
        self.engine.assistant_send(chat, SendRequest { text: "Read it".into(), tab: Some(tab) }).await.unwrap();
    }

    async fn wait_entry(&mut self, chat: ChatId, pick: impl Fn(&Entry) -> bool) -> Entry {
        tokio::time::timeout(WAIT, async {
            loop {
                if let AppEvent::ChatEntryChanged(s) = self.events.recv().await.unwrap()
                    && s.chat_id == chat
                    && pick(&s.entry)
                {
                    return s.entry;
                }
            }
        })
        .await
        .expect("chat entry did not arrive")
    }

    async fn wait_approval(&mut self, chat: ChatId) -> ApprovalId {
        let entry = self
            .wait_entry(chat, |e| matches!(e, Entry::Tool { status: ToolStatus::AwaitingApproval { .. }, .. }))
            .await;
        let Entry::Tool { status: ToolStatus::AwaitingApproval { approval, .. }, .. } = entry else { unreachable!() };
        approval
    }

    async fn tool_status(&self, chat: ChatId) -> ToolStatus {
        let entries = self.engine.assistant_open(chat).await.unwrap().entries;
        entries
            .into_iter()
            .find_map(|s| match s.entry {
                Entry::Tool { tool: ToolUse::RunQuery { .. }, status, .. } => Some(status),
                _ => None,
            })
            .expect("a run_query tool entry")
    }

    /// Sends a query the policy refuses before any approval, waits for the turn to end, and returns the
    /// tool's failure message.
    async fn refused(&mut self, connection: ConnectionId, sql: &str) -> String {
        self.fake_sql(sql);
        let chat = self.chat(Permission::AskFirst);
        self.send(chat, connection).await;
        self.wait_entry(chat, |e| matches!(e, Entry::End { .. })).await;
        match self.tool_status(chat).await {
            ToolStatus::Failed { message } => message,
            other => panic!("expected a refusal for {sql}, got {other:?}"),
        }
    }

    /// Sends a query the policy asks about, approves it, waits for the turn to end, and returns the
    /// tool's final status.
    async fn approved(&mut self, connection: ConnectionId, sql: &str) -> ToolStatus {
        self.fake_sql(sql);
        let chat = self.chat(Permission::AskFirst);
        self.send(chat, connection).await;
        let approval = self.wait_approval(chat).await;
        self.engine.assistant_approve(chat, approval, Decision::Run).unwrap();
        self.wait_entry(chat, |e| matches!(e, Entry::End { .. })).await;
        self.tool_status(chat).await
    }
}

fn draft(database: &str, user: &str, password: &str, read_only: bool, name: &str) -> ConnectionDraft {
    ConnectionDraft {
        database_kind: DatabaseKind::SqlServer,
        id: None,
        name: name.into(),
        host: host(),
        port: port(),
        database: database.into(),
        user: user.into(),
        password: PasswordInput::Set(password.into()),
        ssl_mode: SslMode::Require,
        ca_cert_path: None,
        color: None,
        ssh: None,
        environment: None,
        sql_server_auth: Some(SqlServerAuth::SqlLogin),
        assistant_read_only: read_only,
    }
}

#[tokio::test]
async fn the_assistant_reads_only_a_read_only_sql_server_database() {
    let mut h = Harness::open().await;
    h.setup().await;
    let result = AssertUnwindSafe(async {
        let on = h.login_connection(true, "read-only on").await;
        let off = h.login_connection(false, "read-only off").await;

        // The setting is off: every query is refused without opening a session.
        let refused = h.refused(off, "SELECT id FROM dbo.t").await;
        assert!(refused.contains("Running queries on this SQL Server connection is off"), "{refused}");

        // The setting is on, the database is read-only: an approved SELECT returns capped rows.
        match h.approved(on, "SELECT id, name FROM dbo.t ORDER BY id").await {
            ToolStatus::Done { result: ToolResult::Rows { columns, rows, more, .. } } => {
                assert_eq!(columns, vec!["id".to_owned(), "name".to_owned()]);
                assert_eq!(rows.len(), 50, "capped to the 50-row limit");
                assert!(more, "more rows were available");
                assert_eq!(rows[0][0].as_deref(), Some("1"));
            }
            other => panic!("expected rows, got {other:?}"),
        }

        // The review's two escapes, and the rest of the allowlist, refused before any session opens.
        for (sql, needle) in [
            (format!("SELECT NEXT VALUE FOR [{}].dbo.seq", h.db), "sequence"),
            (
                "SELECT full_filesystem_path FROM sys.dm_os_enumerate_filesystem('/etc', '*')".into(),
                "dm_os_enumerate_filesystem",
            ),
            ("SELECT * INTO dbo.copy FROM dbo.t".into(), "INTO"),
            ("SELECT * FROM OPENROWSET('a', 'b', 'c')".into(), "OPENROWSET"),
            ("SELECT * FROM OPENQUERY(srv, 'select 1')".into(), "OPENQUERY"),
            ("SELECT * FROM OPENDATASOURCE('a', 'b')".into(), "OPENDATASOURCE"),
            ("SELECT * FROM srv.db.dbo.tbl".into(), "four-part"),
            ("SELECT * FROM linked_server.remote_db..orders".into(), "four-part"),
            ("SELECT 1; SELECT 2".into(), "one statement"),
            ("SELECT 1 AS x (SELECT 2 AS y)".into(), "one statement"),
            ("SELECT 'FIRST' AS marker SETUSER".into(), "SETUSER is not part of a single SELECT"),
            ("SELECT 'FIRST' AS marker FROM dbo.t READTEXT dbo.t.name 0x00 0 5".into(), "READTEXT"),
            ("USE master".into(), "SELECT"),
            ("SELECT dbo.my_udf(1)".into(), "my_udf"),
        ] {
            let message = h.refused(on, &sql).await;
            assert!(message.contains(needle), "{sql} -> {message}");
        }

        // Column lists are syntax, not calls.
        match h.approved(on, "WITH c(n) AS (SELECT id FROM dbo.t) SELECT TOP 2 a FROM c, (VALUES (1)) v(a)").await {
            ToolStatus::Done { result: ToolResult::Rows { columns, rows, .. } } => {
                assert_eq!(columns, vec!["a".to_owned()]);
                assert_eq!(rows.len(), 2);
            }
            other => panic!("expected rows, got {other:?}"),
        }

        // A parenthesized query the lexer reads as a column list still runs as a second statement on
        // the server, so the session refuses its second result set.
        match h.approved(on, "SELECT 1 AS x, (SELECT 1) d (SELECT 2 AS y)").await {
            ToolStatus::Failed { message } => assert!(message.contains("one statement"), "{message}"),
            other => panic!("a second result set must be refused, got {other:?}"),
        }

        // A flip to READ_WRITE between calls is caught by the next call's guard. The fresh per-call
        // session re-reads Updateability, so the model's statement never runs against a writable base.
        h.engine.disconnect(off, true).await.unwrap();
        h.engine.disconnect(on, true).await.unwrap();
        h.admin(&format!("ALTER DATABASE [{}] SET READ_WRITE WITH ROLLBACK IMMEDIATE", h.db)).await;
        h.engine.connect(on).await.unwrap();
        match h.approved(on, "SELECT id FROM dbo.t ORDER BY id").await {
            ToolStatus::Failed { message } => assert!(message.contains("writable"), "{message}"),
            other => panic!("a writable database must be refused, got {other:?}"),
        }
    })
    .catch_unwind()
    .await;

    h.teardown().await;
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}
