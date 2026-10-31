#![cfg(feature = "pg-tests")]

use std::sync::Arc;
use std::time::{Duration, Instant};

use engine::model::*;
use engine::{AgentPath, Engine, EngineConfig, MemoryKeyStore, ScriptedTransport, UpdateTransport, draft_from_url};
use futures_util::TryStreamExt;
use tokio::sync::broadcast;
use tokio_postgres::NoTls;

const DEFAULT_URL: &str = "postgres://l1data:l1data@localhost:5433/shop";
const MOCKUP_QUERY: &str = "select c.region, count(*) as orders, sum(o.total) as revenue
from orders o join customers c on c.id = o.customer_id
where o.status = 'paid' and o.created_at > now() - interval '90 days'
group by c.region order by revenue desc";

fn log_line(body: &OutputBody) -> String {
    match body {
        OutputBody::Statement { statement, text } => format!("{statement}> {text}"),
        OutputBody::Rows { statement, rows, truncated, .. } => format!("{statement} rows {rows}{}", if *truncated { " truncated" } else { "" }),
        OutputBody::Command { statement, tag, .. } => format!("{statement} {tag}"),
        OutputBody::Notice { severity, message, .. } => format!("{severity}: {message}"),
        OutputBody::Error { statement, error } => format!("{statement:?} ERROR: {}", error.message),
        OutputBody::Info { message } => message.clone(),
    }
}

fn url() -> String {
    std::env::var("L1_DATA_TEST_PG").unwrap_or_else(|_| DEFAULT_URL.into())
}

struct Harness {
    engine: Arc<Engine>,
    events: broadcast::Receiver<AppEvent>,
    seen: Vec<AppEvent>,
    conn: ConnectionId,
    database: String,
    dir: tempfile::TempDir,
}

impl Harness {
    async fn start() -> Self {
        Self::start_in(tempfile::tempdir().unwrap()).await
    }

    async fn start_in(dir: tempfile::TempDir) -> Self {
        Self::start_with(dir, draft_from_url(&url(), "dev").unwrap()).await
    }

    /// A fresh engine whose one connection logs in as `role`, so `pg_stat_activity` can tell
    /// this test's backends from those of the tests running beside it.
    async fn start_as(role: &Role) -> Self {
        let draft = ConnectionDraft { user: role.name.clone(), password: PasswordInput::Set(ROLE_PASSWORD.into()), ..draft_from_url(&url(), "dev").unwrap() };
        Self::start_with(tempfile::tempdir().unwrap(), draft).await
    }

    async fn start_with(dir: tempfile::TempDir, draft: ConnectionDraft) -> Self {
        Self::start_updating(dir, draft, Arc::new(ScriptedTransport::named("off").unwrap())).await
    }

    async fn start_updating(dir: tempfile::TempDir, draft: ConnectionDraft, updates: Arc<dyn UpdateTransport>) -> Self {
        let engine = Engine::open(EngineConfig { data_dir: dir.path().to_owned(), keys: Arc::new(MemoryKeyStore::default()), agent_path: AgentPath::Fixed("".into()), az_binary: None, user_known_hosts: None, updates })
            .await
            .unwrap();
        let events = engine.subscribe();
        let existing = engine.boot().connections.into_iter().next();
        let config = match existing {
            Some(c) => c,
            None => engine.save_connection(draft).await.unwrap(),
        };
        Self { engine, events, seen: Vec::new(), conn: config.id, database: config.database, dir }
    }

    fn instance(&self) -> InstanceState {
        match self.status() {
            ConnStatus::Connected { instance, .. } => instance,
            other => panic!("{other:?}"),
        }
    }

    /// Names in the instance's `pg_database` list as the snapshot reports it.
    fn database_names(&self) -> Vec<String> {
        self.instance().databases.into_iter().map(|d| d.name).collect()
    }

    fn database_state(&self, name: &str) -> DatabaseState {
        self.instance().databases.into_iter().find(|d| d.name == name).unwrap_or_else(|| panic!("{name} not listed"))
    }

    async fn wait_database(&self, name: &str, done: impl Fn(&DatabaseState) -> bool) -> DatabaseState {
        let started = Instant::now();
        loop {
            let state = self.database_state(name);
            if done(&state) {
                return state;
            }
            assert!(started.elapsed() < Duration::from_secs(10), "{name} never reached the expected state: {state:?}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn edit_view(&self, edit: ViewEdit) {
        self.engine.edit_view(self.conn, edit).await.unwrap();
    }

    async fn wait_database_names(&self, done: impl Fn(&[String]) -> bool) -> Vec<String> {
        let started = Instant::now();
        loop {
            let names = self.database_names();
            if done(&names) {
                return names;
            }
            assert!(started.elapsed() < Duration::from_secs(10), "database list never reached the expected state: {names:?}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    fn status(&self) -> ConnStatus {
        self.engine.boot().live.into_iter().find(|c| c.id == self.conn).expect("connection entry").status
    }

    async fn wait_status(&self, done: impl Fn(&ConnStatus) -> bool) -> ConnStatus {
        let started = Instant::now();
        loop {
            let status = self.status();
            if done(&status) {
                return status;
            }
            assert!(started.elapsed() < Duration::from_secs(10), "connection never reached the expected status: {status:?}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// The connection's configured database, the one every tab targets unless told otherwise.
    fn db(&self) -> DatabaseRef {
        self.db_named(&self.database)
    }

    fn db_named(&self, database: &str) -> DatabaseRef {
        DatabaseRef { connection_id: self.conn, database: database.to_owned() }
    }

    async fn sql_tab(&self, mode: TxnMode) -> TabId {
        self.sql_tab_in(mode, None).await
    }

    async fn sql_tab_in(&self, mode: TxnMode, schema: Option<&str>) -> TabId {
        self.configure(mode, &self.database, schema).await
    }

    async fn sql_tab_on(&self, mode: TxnMode, database: &str) -> TabId {
        self.configure(mode, database, None).await
    }

    async fn configure(&self, mode: TxnMode, database: &str, schema: Option<&str>) -> TabId {
        let tab = TabId(uuid::Uuid::new_v4());
        self.engine
            .console_configure(tab, ConsoleConfig { connection_id: self.conn, database: database.to_owned(), schema: schema.map(str::to_owned), mode })
            .await
            .unwrap();
        tab
    }

    /// The home database's catalog status as the connection snapshot reports it.
    fn catalog_status(&self, database: &str) -> CatalogStatus {
        let live = self.engine.boot().live;
        let ConnStatus::Connected { instance, .. } = &live[0].status else { panic!("{:?}", live[0].status) };
        instance.databases.iter().find(|d| d.name == database).unwrap_or_else(|| panic!("{database} not listed")).catalog.clone()
    }

    /// Waits until a connection snapshot has carried `version` for `database`, counting the
    /// snapshots already seen, since a refresh can publish before the run that caused it finishes.
    async fn wait_published(&mut self, database: &str, version: u64) {
        loop {
            self.drain();
            let published = self.seen.iter().any(|e| match e {
                AppEvent::ConnectionChanged(ConnectionState { id, status: ConnStatus::Connected { instance, .. }, .. }) if *id == self.conn => {
                    instance.databases.iter().any(|d| d.name == database && d.catalog == CatalogStatus::Ready { version })
                }
                _ => false,
            });
            if published {
                return;
            }
            self.next_event().await;
        }
    }

    /// Polls the catalog until `done` accepts it.
    async fn catalog_when(&self, db: &DatabaseRef, done: impl Fn(&Catalog) -> bool) -> Arc<Catalog> {
        let started = Instant::now();
        loop {
            if let Ok(catalog) = self.engine.catalog(db.clone()).await
                && done(&catalog)
            {
                return catalog;
            }
            assert!(started.elapsed() < Duration::from_secs(30), "catalog never reached the expected state");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn next_event(&mut self) -> AppEvent {
        let ev = tokio::time::timeout(Duration::from_secs(30), self.events.recv()).await.expect("event within 30s").unwrap();
        self.seen.push(ev.clone());
        ev
    }

    async fn wait_finished(&mut self, tab: TabId, run_id: RunId) -> ConsoleState {
        loop {
            if let AppEvent::ConsoleChanged(state) = self.next_event().await {
                if state.tab_id == tab && matches!(state.run, RunState::Finished { run_id: r, .. } if r == run_id) {
                    return state;
                }
            }
        }
    }

    async fn start_run(&mut self, tab: TabId, sql: &str) -> RunAccepted {
        self.engine
            .console_run(tab, RunRequest::Script { text: sql.into(), target: RunTarget::All, mode: RunMode::Run, params: Vec::new() }, None)
            .await
            .unwrap()
    }

    async fn run(&mut self, tab: TabId, sql: &str) -> ConsoleState {
        let accepted = self.start_run(tab, sql).await;
        self.wait_finished(tab, accepted.run_id).await
    }

    async fn run_ok(&mut self, tab: TabId, sql: &str) -> ConsoleState {
        let state = self.run(tab, sql).await;
        assert!(matches!(state.run, RunState::Finished { outcome: RunOutcome::Ok, .. }), "{sql}: {:?}", state.run);
        state
    }

    fn result(state: &ConsoleState, index: usize) -> ResultMeta {
        match &state.run {
            RunState::Finished { statements, .. } => match statements[index].outputs.first() {
                Some(RunOutput::Rows { result, state: RowsState::Complete }) => result.clone(),
                other => panic!("statement {index} is not a finished result: {other:?}"),
            },
            other => panic!("not finished: {other:?}"),
        }
    }

    fn page(&self, meta: &ResultMeta, start: u32, len: u32) -> Vec<Vec<Option<String>>> {
        self.engine.result_page(meta.r#ref, start, len).unwrap().rows
    }

    async fn scalar(&mut self, tab: TabId, sql: &str) -> String {
        let state = self.run_ok(tab, sql).await;
        let meta = Self::result(&state, 0);
        self.page(&meta, 0, 1)[0][0].clone().expect("non-null scalar")
    }

    fn console(&self, tab: TabId) -> ConsoleState {
        self.engine.boot().consoles.into_iter().find(|c| c.tab_id == tab).expect("console")
    }

    fn entries(&self, tab: TabId) -> Vec<OutputEntry> {
        self.seen
            .iter()
            .filter_map(|e| match e {
                AppEvent::ConsoleOutput { tab_id, entries } if *tab_id == tab => Some(entries.iter().cloned()),
                _ => None,
            })
            .flatten()
            .collect()
    }

    fn outputs(&self, tab: TabId) -> Vec<OutputBody> {
        self.entries(tab).into_iter().map(|e| e.body).collect()
    }

    /// The tab's output log, one short string per entry, for literal assertions.
    fn log(&self, tab: TabId) -> Vec<String> {
        self.outputs(tab).iter().map(log_line).collect()
    }

    /// Collects events already sent, for calls such as `table_apply` that return without a run to wait on.
    fn drain(&mut self) {
        while let Ok(ev) = self.events.try_recv() {
            self.seen.push(ev);
        }
    }

    async fn export(&self, meta: &ResultMeta, format: ExportFormat, scope: ExportScope, name: &str) -> (ExportSummary, String) {
        let path = self.dir.path().join(name);
        let summary = self.engine.result_export(meta.r#ref, format, scope, path.display().to_string()).await.unwrap();
        (summary, std::fs::read_to_string(path).unwrap())
    }

    async fn wait_console(&self, tab: TabId, done: impl Fn(&ConsoleState) -> bool) -> ConsoleState {
        let started = Instant::now();
        loop {
            let state = self.console(tab);
            if done(&state) {
                return state;
            }
            assert!(started.elapsed() < Duration::from_secs(30), "console never reached the expected state: {state:?}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn history_len(&self) -> usize {
        self.engine.history(HistoryQuery { text: None, connection_id: None, before_id: None, limit: 500 }).entries.len()
    }

    async fn oracle(&self) -> tokio_postgres::Client {
        let (client, connection) = tokio_postgres::connect(&url(), NoTls).await.unwrap();
        tokio::spawn(connection);
        let tz = iana_time_zone::get_timezone().unwrap_or_else(|_| "UTC".into());
        client.batch_execute(&format!("SET TimeZone = '{tz}'")).await.unwrap();
        client
    }
}

const ROLE_PASSWORD: &str = "pw";

/// A throwaway login role: every backend it owns on the server belongs to the test that made it.
struct Role {
    name: String,
    oracle: tokio_postgres::Client,
}

impl Role {
    async fn create() -> Self {
        let (oracle, connection) = tokio_postgres::connect(&url(), NoTls).await.unwrap();
        tokio::spawn(connection);
        let name = unique("t_role");
        oracle.batch_execute(&format!("create role {name} login password '{ROLE_PASSWORD}'")).await.unwrap();
        Self { name, oracle }
    }

    /// `(database, pid)` of every client backend logged in as the role, sorted.
    async fn backends(&self) -> Vec<(String, i32)> {
        let rows = self
            .oracle
            .query("select datname, pid from pg_stat_activity where usename = $1 and backend_type = 'client backend' order by datname, pid", &[&self.name])
            .await
            .unwrap();
        rows.iter().map(|r| (r.get::<_, String>(0), r.get::<_, i32>(1))).collect()
    }

    /// The backends once `done` accepts them, or whatever is left after five seconds: a closed
    /// socket reaches `pg_stat_activity` a little after the client drops it.
    async fn backends_when(&self, done: impl Fn(&[(String, i32)]) -> bool) -> Vec<(String, i32)> {
        let started = Instant::now();
        loop {
            let backends = self.backends().await;
            if done(&backends) || started.elapsed() > Duration::from_secs(5) {
                return backends;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn drop(self) {
        self.oracle.batch_execute(&format!("drop role {}", self.name)).await.unwrap();
    }
}

fn statuses(state: &ConsoleState) -> Vec<&StmtStatus> {
    match &state.run {
        RunState::Finished { statements, .. } => statements.iter().map(|s| &s.status).collect(),
        other => panic!("not finished: {other:?}"),
    }
}

fn unique(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::new_v4().simple())
}

#[tokio::test]
async fn connect_reports_server_and_catalog_has_the_mockup_tables() {
    let h = Harness::start().await;
    let server = h.engine.connect(h.conn).await.unwrap();
    assert!(server.version.starts_with("16."), "{}", server.version);
    assert!(server.version_num >= 160000);
    assert_eq!(server.encoding, "UTF8");
    assert_eq!(h.engine.connect(h.conn).await.unwrap().version, server.version, "connect is idempotent");

    let catalog = h.engine.catalog(h.db()).await.unwrap();
    assert_eq!(catalog.db.database, "shop");
    let names: Vec<&str> = catalog.schemas.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["information_schema", "lab", "pg_catalog", "public"]);
    let public = catalog.schemas.iter().find(|s| s.name == "public").unwrap();
    for (table, rows) in [("customers", 18_400), ("orders", 241_000), ("order_items", 612_000), ("payments", 238_000), ("products", 1_200)] {
        let rel = public.relations.iter().find(|r| r.name == table).unwrap_or_else(|| panic!("{table} missing"));
        assert_eq!(rel.kind, RelKind::Table);
        let estimate = rel.row_estimate.unwrap_or_else(|| panic!("{table} has no estimate"));
        assert!((estimate - rows).abs() * 20 < rows, "{table} estimate {estimate} vs {rows}");
        let id = &rel.columns[0];
        assert!(id.name == "id" && id.pk && id.not_null && id.type_display == "bigint", "{table}: {id:?}");
    }
    let views: Vec<&str> = public.relations.iter().filter(|r| r.kind == RelKind::View).map(|r| r.name.as_str()).collect();
    assert_eq!(views, ["low_stock", "paid_orders", "revenue_by_region"]);
    let slow = public.functions.iter().find(|f| f.name == "slow_echo").unwrap();
    assert_eq!((slow.args.as_str(), slow.returns.as_str(), slow.kind), ("secs double precision", "double precision", FnKind::Function));
    let zoo = catalog.relation("lab", "type_zoo").unwrap();
    assert_eq!(zoo.columns.iter().find(|c| c.name == "m").unwrap().type_display, "lab.mood");

    assert_eq!(h.catalog_status("shop"), CatalogStatus::Ready { version: 1 });
}

#[tokio::test]
async fn exact_row_count_is_exact_and_reports_missing_tables() {
    let h = Harness::start().await;
    let tab = TabId(uuid::Uuid::new_v4());
    assert!(matches!(h.engine.exact_row_count(tab, h.db(), "public".into(), "coupons".into(), TableQuery::default()).await, Err(CountError::NotConnected)));
    h.engine.connect(h.conn).await.unwrap();
    assert_eq!(h.engine.exact_row_count(tab, h.db(), "public".into(), "coupons".into(), TableQuery::default()).await.unwrap(), 3);
    assert_eq!(h.engine.exact_row_count(tab, h.db(), "lab".into(), "type_zoo".into(), TableQuery::default()).await.unwrap(), 2);
    let missing = h.engine.exact_row_count(tab, h.db(), "public".into(), "nope".into(), TableQuery::default()).await;
    assert!(matches!(&missing, Err(CountError::Failed { message }) if message.contains("does not exist")), "{missing:?}");
}

#[tokio::test]
async fn mockup_query_returns_six_regions_with_bigint_orders() {
    let mut h = Harness::start().await;
    let tab = h.sql_tab(TxnMode::AutoCommit).await;
    let state = h.run_ok(tab, MOCKUP_QUERY).await;
    let meta = Harness::result(&state, 0);
    assert_eq!(meta.row_count, 6);
    assert_eq!(meta.rows_seen, 6);
    assert!(meta.truncated.is_none());
    let cols: Vec<(&str, &str, TypeCategory)> =
        meta.columns.iter().map(|c| (c.name.as_str(), c.r#type.display.as_str(), c.r#type.category)).collect();
    assert_eq!(
        cols,
        [("region", "text", TypeCategory::Text), ("orders", "bigint", TypeCategory::Integer), ("revenue", "numeric", TypeCategory::Numeric)]
    );
    assert_eq!(meta.columns[0].source.map(|s| s.attnum), Some(4), "region is customers.region");
    let rows = h.page(&meta, 0, 10);
    assert_eq!(rows.len(), 6);
    let regions: Vec<&str> = rows.iter().map(|r| r[0].as_deref().unwrap()).collect();
    assert_eq!(regions.len(), 6);
    assert!(regions.contains(&"EU-West") && regions.contains(&"APAC"));
    assert!(rows[0][2].as_deref().unwrap().contains('.'), "numeric text keeps its scale");
    assert!(matches!(h.engine.result_page(meta.r#ref, 7, 10), Err(ResultError::OutOfRange)));
    assert_eq!(h.log(tab), [format!("0> {MOCKUP_QUERY}"), "0 rows 6".into()]);
    assert!(matches!(state.session, SessionState::Open { backend_pid } if backend_pid > 0));
}

#[tokio::test]
async fn type_zoo_cells_equal_postgres_text_output() {
    let mut h = Harness::start().await;
    let tab = h.sql_tab(TxnMode::AutoCommit).await;
    let state = h.run_ok(tab, "select * from lab.type_zoo order by id").await;
    let meta = Harness::result(&state, 0);
    let rows = h.page(&meta, 0, 10);
    assert_eq!(rows.len(), 2);

    // The simple protocol on an independent session is Postgres's own text output per column
    // (what psql prints). `bool::text` would say `true` where the output function says `t`.
    let oracle = h.oracle().await;
    let expected = oracle.simple_query("select * from lab.type_zoo order by id").await.unwrap();
    let expected_rows: Vec<Vec<Option<String>>> = expected
        .iter()
        .filter_map(|m| match m {
            tokio_postgres::SimpleQueryMessage::Row(r) => Some((0..r.len()).map(|i| r.get(i).map(str::to_owned)).collect()),
            _ => None,
        })
        .collect();
    for (r, row) in rows.iter().enumerate() {
        for (c, col) in meta.columns.iter().enumerate() {
            assert_eq!(row[c], expected_rows[r][c], "row {r} column {} ({})", col.name, col.r#type.display);
        }
    }
    let by_name = |n: &str| meta.columns.iter().find(|c| c.name == n).unwrap().r#type.clone();
    assert_eq!((by_name("m").display.as_str(), by_name("m").category), ("lab.mood", TypeCategory::Enum));
    assert_eq!((by_name("arr_i").display.as_str(), by_name("arr_i").category), ("integer[]", TypeCategory::Array));
    assert_eq!(by_name("rng").category, TypeCategory::Range);
    assert_eq!(by_name("tstz").display, "timestamptz");
    assert_eq!(by_name("jb").category, TypeCategory::Json);
    assert_eq!(rows[0][1].as_deref(), Some("t"), "booleans render like psql");
    assert_eq!(rows[1][1], None);
    assert_eq!(rows[0][3].as_deref(), Some("9007199254740993"), "bigint beyond 2^53 is verbatim");
    assert_eq!(rows[1][7].as_deref(), Some(""), "empty string is not NULL");
}

#[tokio::test]
async fn multi_statement_run_tags_notices_and_history() {
    let mut h = Harness::start().await;
    let tab = h.sql_tab(TxnMode::AutoCommit).await;
    let script = "create temp table t_multi (id int, v text);\n\
                  insert into t_multi select g, 'v' || g from generate_series(1, 5) g;\n\
                  update t_multi set v = 'x' where id <= 3;\n\
                  select slow_echo(0.01);\n\
                  select count(*) from t_multi";
    let accepted = h.start_run(tab, script).await;
    assert_eq!(accepted.statements.len(), 5);
    let state = h.wait_finished(tab, accepted.run_id).await;
    let RunState::Finished { statements, .. } = &state.run else { panic!("not finished") };
    let tags: Vec<String> = statements.iter().flat_map(|s| &s.outputs).map(|o| match o {
        RunOutput::Command { tag, .. } => tag.clone(),
        RunOutput::Rows { result, .. } => format!("rows:{}", result.row_count),
        RunOutput::Plan { .. } => panic!("a script run does not return a plan"),
    }).collect();
    assert_eq!(tags, ["CREATE TABLE", "INSERT 0 5", "UPDATE 3", "rows:1", "rows:1"]);
    assert_eq!(
        h.log(tab),
        [
            "0> create temp table t_multi (id int, v text)",
            "0 CREATE TABLE",
            "1> insert into t_multi select g, 'v' || g from generate_series(1, 5) g",
            "1 INSERT 0 5",
            "2> update t_multi set v = 'x' where id <= 3",
            "2 UPDATE 3",
            "3> select slow_echo(0.01)",
            "NOTICE: slept 0.01 seconds",
            "3 rows 1",
            "4> select count(*) from t_multi",
            "4 rows 1",
        ]
    );
    assert!(h.entries(tab).iter().all(|e| e.run_id == accepted.run_id));
    assert_eq!(h.scalar(tab, "select count(*) from t_multi where v = 'x'").await, "3", "temp table sticks to the tab's session");

    let history = h.engine.history(HistoryQuery { text: None, connection_id: Some(h.conn), before_id: None, limit: 10 });
    assert_eq!(history.entries.len(), 6);
    assert!(matches!(history.entries[0].outcome, HistoryOutcome::Rows { rows: 1 }));
    assert!(matches!(&history.entries[3].outcome, HistoryOutcome::Command { tag } if tag == "UPDATE 3"));
    assert_eq!(history.entries[5].sql, "create temp table t_multi (id int, v text)");
    assert!(h.seen.iter().filter(|e| matches!(e, AppEvent::HistoryAppended(_))).count() >= 6);
}

#[tokio::test]
async fn notices_stream_before_a_waiting_command_finishes() {
    let mut h = Harness::start().await;
    let tab = h.sql_tab(TxnMode::AutoCommit).await;
    let accepted = h.start_run(tab, "DO $$ BEGIN RAISE NOTICE 'before wait'; PERFORM pg_sleep(30); END $$").await;
    let noticed = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let AppEvent::ConsoleOutput { tab_id, entries } = h.next_event().await
                && tab_id == tab
                && entries.iter().any(|e| matches!(&e.body, OutputBody::Notice { message, .. } if message == "before wait"))
            {
                break;
            }
        }
    }).await;
    h.engine.console_cancel(tab).await;
    let state = h.wait_finished(tab, accepted.run_id).await;
    assert!(noticed.is_ok(), "notice waited for command completion");
    assert!(matches!(state.run, RunState::Finished { outcome: RunOutcome::Cancelled, .. }));
    assert_eq!(h.scalar(tab, "SELECT 1").await, "1");
}

#[tokio::test]
async fn error_position_maps_to_utf16_past_an_emoji() {
    let mut h = Harness::start().await;
    let tab = h.sql_tab(TxnMode::AutoCommit).await;
    let text = "select 1;\nselect '😀 ok' as e, nope_column from customers limit 1";
    let cursor = text.encode_utf16().count() as u32 - 3;
    let accepted = h
        .engine
        .console_run(tab, RunRequest::Script { text: text.into(), target: RunTarget::Cursor { offset: DocOffset(cursor) }, mode: RunMode::Run, params: Vec::new() }, None)
        .await
        .unwrap();
    assert_eq!(accepted.statements.len(), 1, "cursor picks only the second statement");
    let state = h.wait_finished(tab, accepted.run_id).await;
    let StmtStatus::Failed { error } = &statuses(&state)[0] else { panic!("{:?}", state.run) };
    assert_eq!(error.sqlstate.as_deref(), Some("42703"));
    let expected = text[..text.find("nope_column").unwrap()].encode_utf16().count() as u32;
    assert_eq!(error.position, Some(DocOffset(expected)));
    assert!(matches!(state.run, RunState::Finished { outcome: RunOutcome::Failed, .. }));
    assert_eq!(state.txn, TxnState::Idle);
    assert_eq!(h.log(tab), ["0> select '😀 ok' as e, nope_column from customers limit 1", "Some(0) ERROR: column \"nope_column\" does not exist"]);
}

#[tokio::test]
async fn cancel_finishes_cancelled_quickly() {
    let mut h = Harness::start().await;
    let tab = h.sql_tab(TxnMode::AutoCommit).await;
    let accepted = h.start_run(tab, "select slow_echo(30); select 2").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(matches!(h.engine.console_run(tab, RunRequest::Script { text: "select 1".into(), target: RunTarget::All, mode: RunMode::Run, params: Vec::new() }, None).await, Err(RunError::Busy)));
    assert!(matches!(h.engine.console_commit(tab, None).await, Err(TxnError::Busy)));
    let started = Instant::now();
    h.engine.console_cancel(tab).await;
    let state = h.wait_finished(tab, accepted.run_id).await;
    assert!(started.elapsed() < Duration::from_secs(2), "cancel took {:?}", started.elapsed());
    assert!(matches!(state.run, RunState::Finished { outcome: RunOutcome::Cancelled, .. }));
    assert!(matches!(statuses(&state)[..], [StmtStatus::Cancelled, StmtStatus::Skipped]));
    assert_eq!(h.log(tab), ["0> select slow_echo(30)", "Some(0) ERROR: canceling statement due to user request"]);
    assert_eq!(h.scalar(tab, "select 7").await, "7", "the session is usable afterwards");
    h.engine.console_cancel(tab).await;
}

#[tokio::test]
async fn manual_tab_isolated_until_commit_and_abort_rolls_back() {
    let mut h = Harness::start().await;
    let a = h.sql_tab(TxnMode::Manual).await;
    let b = h.sql_tab(TxnMode::AutoCommit).await;
    let table = unique("public.t_iso");
    h.run_ok(b, &format!("create table {table} (id int)")).await;

    let state = h.run_ok(a, &format!("insert into {table} values (1)")).await;
    assert_eq!(state.txn, TxnState::Active);
    assert_eq!(h.log(a), ["BEGIN (implicit)".into(), format!("0> insert into {table} values (1)"), "0 INSERT 0 1".into()]);
    assert_eq!(h.scalar(b, &format!("select count(*) from {table}")).await, "0");
    let err = h.engine.disconnect(h.conn, false).await.unwrap_err();
    assert!(matches!(err, DisconnectError::OpenTransactions { ref tabs } if tabs.len() == 1 && tabs[0].tab_id == a && tabs[0].has_writes));
    let reconfigure = |database: &str, mode| ConsoleConfig { connection_id: h.conn, database: database.into(), schema: None, mode };
    assert!(matches!(h.engine.console_configure(a, reconfigure("shop", TxnMode::AutoCommit)).await, Err(ConfigureError::TransactionOpen)));
    assert!(matches!(h.engine.console_configure(a, reconfigure("reporting", TxnMode::Manual)).await, Err(ConfigureError::TransactionOpen)));
    h.engine.console_commit(a, None).await.unwrap();
    assert_eq!(h.console(a).txn, TxnState::Idle);
    assert_eq!(h.scalar(b, &format!("select count(*) from {table}")).await, "1");
    assert!(matches!(h.engine.console_commit(a, None).await, Err(TxnError::NoTransaction)));

    let state = h.run(a, "select 1/0").await;
    assert!(matches!(state.run, RunState::Finished { outcome: RunOutcome::Failed, .. }));
    assert_eq!(state.txn, TxnState::Aborted);
    let state = h.run(a, "select 1").await;
    assert_eq!(state.txn, TxnState::Aborted, "an aborted block rejects everything but ROLLBACK");
    assert!(matches!(statuses(&state)[0], StmtStatus::Failed { error } if error.sqlstate.as_deref() == Some("25P02")));
    assert!(matches!(h.engine.console_commit(a, None).await, Err(TxnError::RolledBack)));
    assert_eq!(h.console(a).txn, TxnState::Idle);

    h.run_ok(a, &format!("insert into {table} values (2)")).await;
    h.engine.console_rollback(a).await.unwrap();
    assert_eq!(h.console(a).txn, TxnState::Idle);
    assert_eq!(h.scalar(b, &format!("select count(*) from {table}")).await, "1");

    h.run_ok(a, "begin; savepoint s; select 1; rollback to s; commit").await;
    assert_eq!(h.console(a).txn, TxnState::Idle);
    h.run_ok(b, &format!("drop table {table}")).await;
}

#[tokio::test]
async fn a_notice_raised_at_commit_carries_the_commit_run_id() {
    let mut h = Harness::start().await;
    let tab = h.sql_tab(TxnMode::Manual).await;
    let script = "create temp table t_deferred (id int);\n\
                  create function pg_temp.notice_deferred() returns trigger language plpgsql as $$ begin raise notice 'deferred'; return null; end $$;\n\
                  create constraint trigger t_deferred_notice after insert on t_deferred deferrable initially deferred for each row execute function pg_temp.notice_deferred();\n\
                  insert into t_deferred values (1)";
    let accepted = h.start_run(tab, script).await;
    let state = h.wait_finished(tab, accepted.run_id).await;
    assert!(matches!(state.run, RunState::Finished { outcome: RunOutcome::Ok, .. }), "{:?}", state.run);

    assert!(!h.log(tab).contains(&"NOTICE: deferred".to_owned()), "the trigger is deferred to COMMIT: {:?}", h.log(tab));

    let before = h.entries(tab).len();
    h.engine.console_commit(tab, None).await.unwrap();
    h.drain();
    let commit = &h.entries(tab)[before..];
    assert_eq!(commit.iter().map(|e| log_line(&e.body)).collect::<Vec<_>>(), ["0> COMMIT", "NOTICE: deferred", "0 COMMIT"]);
    assert!(commit.iter().all(|e| e.run_id == commit[0].run_id));
    assert!(commit[0].run_id > accepted.run_id, "commit run {:?} is not after run {:?}", commit[0].run_id, accepted.run_id);
}

#[tokio::test]
async fn a_long_statement_is_cut_to_two_thousand_chars_in_the_log() {
    let mut h = Harness::start().await;
    let tab = h.sql_tab(TxnMode::AutoCommit).await;
    let sql = format!("select '{}' as long", "é".repeat(2_500));
    h.run_ok(tab, &sql).await;
    let Some(OutputBody::Statement { statement: 0, text }) = h.outputs(tab).into_iter().next() else { panic!("{:?}", h.log(tab)) };
    let expected: String = sql.chars().take(2_000).chain(['…']).collect();
    assert_eq!(text, expected);
    assert_eq!(text.chars().count(), 2_001);
}

#[tokio::test]
async fn terminated_backend_becomes_lost_and_the_next_run_reopens() {
    let mut h = Harness::start().await;
    let tab = h.sql_tab(TxnMode::Manual).await;
    let state = h.run_ok(tab, "create temp table t_lost (id int)").await;
    let SessionState::Open { backend_pid } = state.session else { panic!("{:?}", state.session) };
    assert_eq!(state.txn, TxnState::Active);

    let oracle = h.oracle().await;
    oracle.simple_query(&format!("select pg_terminate_backend({backend_pid})")).await.unwrap();
    let state = h.run(tab, "select 1").await;
    assert!(matches!(state.session, SessionState::Lost { transaction: TransactionLoss::RolledBack, .. }), "{:?}", state.session);
    assert_eq!(state.txn, TxnState::Idle);
    assert!(h.outputs(tab).iter().any(|o| matches!(o, OutputBody::Info { message } if message.starts_with("Transaction rolled back"))));

    let state = h.run_ok(tab, "select 2").await;
    assert!(matches!(state.session, SessionState::Open { backend_pid: pid } if pid != backend_pid));
    let pid = match state.session {
        SessionState::Open { backend_pid } => backend_pid,
        _ => unreachable!(),
    };
    h.engine.console_rollback(tab).await.unwrap();

    oracle.simple_query(&format!("select pg_terminate_backend({pid})")).await.unwrap();
    let auto = h.sql_tab(TxnMode::AutoCommit).await;
    let state = h.run_ok(auto, "select 1").await;
    let SessionState::Open { backend_pid: auto_pid } = state.session else { panic!() };
    oracle.simple_query(&format!("select pg_terminate_backend({auto_pid})")).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let state = h.run(auto, "select 3").await;
    assert!(matches!(state.session, SessionState::Lost { transaction: TransactionLoss::None, .. }), "{:?}", state.session);
    assert_eq!(h.scalar(auto, "select 4").await, "4", "an idle lost session is replaced transparently");
}

#[tokio::test]
async fn forced_disconnect_during_a_manual_run_leaves_the_tab_idle() {
    let mut h = Harness::start().await;
    let tab = h.sql_tab(TxnMode::Manual).await;
    let accepted = h.start_run(tab, "select pg_sleep(3)").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    h.engine.disconnect(h.conn, true).await.unwrap();
    let state = h.wait_finished(tab, accepted.run_id).await;
    assert!(matches!(state.run, RunState::Finished { outcome: RunOutcome::Cancelled, .. }), "{:?}", state.run);
    assert!(matches!(statuses(&state)[..], [StmtStatus::Cancelled]));
    assert!(matches!(state.session, SessionState::None), "{:?}", state.session);

    // The orphaned task sees its sleep cancelled now; it must not write that failure into the tab.
    tokio::time::sleep(Duration::from_millis(700)).await;
    let state = h.console(tab);
    assert_eq!(state.txn, TxnState::Idle, "the orphaned run wrote its transaction state");
    assert!(matches!(state.session, SessionState::None), "{:?}", state.session);

    let first = h.scalar(tab, "select pg_current_xact_id()::text").await;
    let second = h.scalar(tab, "select pg_current_xact_id()::text").await;
    assert_eq!(h.console(tab).txn, TxnState::Active, "manual mode opened a block again");
    assert_eq!(first, second, "both statements ran inside one transaction");
    assert!(h.log(tab).iter().any(|l| l == "BEGIN (implicit)"), "{:?}", h.log(tab));
    h.engine.console_rollback(tab).await.unwrap();
}

#[tokio::test]
async fn a_run_orphaned_by_disconnect_never_writes_into_the_next_run() {
    let mut h = Harness::start().await;
    let tab = h.sql_tab(TxnMode::AutoCommit).await;
    let survives_cancel = "select 1; select 2; do $$ begin perform pg_sleep(2); exception when query_canceled then perform pg_sleep(2); end $$";
    let old = h.start_run(tab, survives_cancel).await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    h.engine.disconnect(h.conn, true).await.unwrap();
    let new = h.start_run(tab, "select pg_sleep(3); select 'b1'; select 'b2'; select 'b3'").await;
    let state = h.wait_finished(tab, new.run_id).await;

    // The DO block outlives the cancel by two seconds, so the orphaned task ends during the new run.
    let runs: Vec<(RunId, bool)> = h
        .seen
        .iter()
        .filter_map(|e| match e {
            AppEvent::ConsoleChanged(s) if s.tab_id == tab => match &s.run {
                RunState::Running { run_id, .. } => Some((*run_id, false)),
                RunState::Finished { run_id, outcome, statements, .. } => {
                    Some((*run_id, *outcome == RunOutcome::Cancelled && statements.get(2).is_some_and(|s| matches!(s.status, StmtStatus::Cancelled))))
                }
                RunState::Idle => None,
            },
            _ => None,
        })
        .collect();
    let first_new = runs.iter().position(|(r, _)| *r == new.run_id).unwrap();
    let stale: Vec<RunId> = runs[first_new..].iter().map(|(r, _)| *r).filter(|r| *r != new.run_id).collect();
    assert!(stale.is_empty(), "stale run {stale:?} published after run {:?} was accepted", new.run_id);
    assert!(runs[..first_new].iter().any(|(r, cancelled)| *r == old.run_id && *cancelled), "disconnect finished the old run as Cancelled: {runs:?}");
    assert!(matches!(state.run, RunState::Finished { outcome: RunOutcome::Ok, .. }), "{:?}", state.run);
    assert_eq!(statuses(&state).len(), 4);
    assert!(statuses(&state).iter().all(|s| matches!(s, StmtStatus::Finished)), "{:?}", state.run);
    assert_eq!(h.page(&Harness::result(&state, 3), 0, 1)[0][0].as_deref(), Some("b3"));
}

#[tokio::test]
async fn big_result_truncates_at_max_rows_and_keeps_counting() {
    let mut h = Harness::start().await;
    let tab = h.sql_tab(TxnMode::AutoCommit).await;
    let state = h.run_ok(tab, "select g, 'row ' || g from generate_series(1, 300000) g").await;
    let meta = Harness::result(&state, 0);
    assert_eq!(meta.row_count, 200_000);
    assert_eq!(meta.rows_seen, 300_000);
    assert_eq!(meta.truncated, Some(TruncateReason::RowLimit));
    assert_eq!(h.page(&meta, 199_999, 5), vec![vec![Some("200000".to_owned()), Some("row 200000".to_owned())]]);
    let streaming = h
        .seen
        .iter()
        .filter(|e| matches!(e, AppEvent::ConsoleChanged(s) if s.tab_id == tab && matches!(s.run, RunState::Running { .. })))
        .count();
    assert!(streaming >= 2, "progress snapshots while streaming: {streaming}");
    assert_eq!(h.log(tab).last().map(String::as_str), Some("0 rows 300000 truncated"));
    assert_eq!(h.scalar(tab, "select 1").await, "1");
    assert!(matches!(h.engine.result_page(meta.r#ref, 0, 1), Err(ResultError::Gone)), "results die on re-run");
}

#[tokio::test]
async fn aggregate_orders_total_matches_sql_sum_only_when_every_row_is_stored() {
    let mut h = Harness::start().await;
    let tab = h.sql_tab(TxnMode::AutoCommit).await;
    h.engine.set_settings(Settings { max_rows: 241_000, ..h.engine.settings() });
    let state = h.run_ok(tab, "select total from public.orders").await;
    let meta = Harness::result(&state, 0);
    assert_eq!(meta.truncated, None, "the cap has to hold every orders row");
    assert!(meta.row_count >= 241_000, "stored {}", meta.row_count);
    let full = h
        .engine
        .result_aggregate(meta.r#ref, RowSpan { start: 0, end: meta.row_count - 1 }, ColSpan { start: 0, end: 0 })
        .await
        .unwrap();
    let CellAggregate::Numeric { sum, .. } = full else { panic!("expected a numeric total, got {full:?}") };
    let sql_sum = h.scalar(tab, "select sum(total) from public.orders").await;
    assert_eq!(sum, sql_sum);
    h.engine.set_settings(Settings { max_rows: 200_000, ..h.engine.settings() });
    let state = h.run_ok(tab, "select total from public.orders").await;
    let meta = Harness::result(&state, 0);
    assert_eq!(meta.row_count, 200_000);
    let capped = h
        .engine
        .result_aggregate(meta.r#ref, RowSpan { start: 0, end: meta.row_count - 1 }, ColSpan { start: 0, end: 0 })
        .await
        .unwrap();
    let CellAggregate::Numeric { sum: capped_sum, .. } = capped else { panic!("expected a numeric total, got {capped:?}") };
    assert_ne!(capped_sum, sql_sum);
}

#[tokio::test]
async fn ddl_refreshes_the_catalog() {
    let mut h = Harness::start().await;
    let tab = h.sql_tab(TxnMode::AutoCommit).await;
    h.engine.connect(h.conn).await.unwrap();
    let table = unique("t_ddl");
    let initial = h.engine.catalog(h.db()).await.unwrap().version;
    h.run_ok(tab, &format!("create table public.{table} (id serial primary key, note text)")).await;
    let catalog = h.catalog_when(&h.db(), |c| c.version > initial).await;
    let version = catalog.version;
    assert_eq!(version, initial + 1, "one DDL statement causes one refresh");
    assert_eq!(catalog.relation("public", &table).unwrap().pk_columns(), ["id"]);
    h.wait_published("shop", version).await;
    assert_eq!(h.catalog_status("shop"), CatalogStatus::Ready { version });

    let page = h.engine.console_run(tab, RunRequest::TablePage { schema: "public".into(), table: "orders".into(), page: 2, query: TableQuery::default() }, None).await.unwrap();
    let state = h.wait_finished(tab, page.run_id).await;
    let meta = Harness::result(&state, 0);
    assert_eq!(meta.row_count, 500);
    let logged: Vec<String> = h.entries(tab).iter().filter(|e| e.run_id == page.run_id).map(|e| log_line(&e.body)).collect();
    assert_eq!(logged, ["0> SELECT * FROM \"public\".\"orders\" ORDER BY \"id\" LIMIT 500 OFFSET 1000", "0 rows 500"]);
    assert_eq!(h.page(&meta, 0, 1)[0][0].as_deref(), Some("1001"), "page 2 of 500 ordered by pk");

    h.run_ok(tab, &format!("drop table public.{table}")).await;
    let dropped = h.engine.refresh_catalog(h.db()).await.unwrap();
    assert!(dropped > version);
    assert!(h.engine.catalog(h.db()).await.unwrap().relation("public", &table).is_none());
}

#[tokio::test]
async fn csv_export_matches_copy_csv() {
    let mut h = Harness::start().await;
    let tab = h.sql_tab(TxnMode::AutoCommit).await;
    let sql = "select id, b, n, t, vc, jb, arr_t, m, tstz from lab.type_zoo order by id";
    let state = h.run_ok(tab, sql).await;
    let meta = Harness::result(&state, 0);
    let path = h.dir.path().join("zoo.csv");
    let summary = h.engine.result_export(meta.r#ref, ExportFormat::Csv, ExportScope::Loaded, path.display().to_string()).await.unwrap();
    let ours = std::fs::read_to_string(&path).unwrap();
    assert_eq!(summary.rows, 2);
    assert_eq!(summary.bytes, ours.len() as u64);
    assert!(!summary.truncated);

    let oracle = h.oracle().await;
    let stream = oracle.copy_out(&format!("copy ({sql}) to stdout with (format csv, header)")).await.unwrap();
    let chunks: Vec<bytes::Bytes> = stream.try_collect().await.unwrap();
    let theirs = String::from_utf8(chunks.concat()).unwrap();
    assert_eq!(ours.replace("\r\n", "\n"), theirs);
    assert!(matches!(h.engine.result_export(ResultRef { index: 9, ..meta.r#ref }, ExportFormat::Csv, ExportScope::Loaded, path.display().to_string()).await, Err(ExportError::Gone)));
}

#[tokio::test]
async fn loaded_exports_type_json_draw_markdown_and_round_trip_inserts() {
    let mut h = Harness::start().await;
    let tab = h.sql_tab(TxnMode::AutoCommit).await;
    let state = h.run_ok(tab, "select * from lab.type_zoo order by id").await;
    let meta = Harness::result(&state, 0);

    let (summary, json) = h.export(&meta, ExportFormat::Json, ExportScope::Loaded, "zoo.json").await;
    assert_eq!((summary.rows, summary.bytes, summary.truncated), (2, json.len() as u64, false));
    for exact in [r#""i8":9007199254740993"#, r#""n":12345678901234567890.123456789"#, r#""f8":"-Infinity""#, r#""f4":"NaN""#] {
        assert!(json.contains(exact), "{exact} in {json}");
    }
    let rows: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap();
    assert_eq!((&rows[0]["b"], &rows[0]["jb"], &rows[0]["arr_t"]), (&serde_json::json!(true), &serde_json::json!({ "b": [1, 2, 3] }), &serde_json::json!(r#"{a,"b c"}"#)));
    assert_eq!((&rows[1]["b"], &rows[1]["j"], &rows[1]["t"]), (&serde_json::Value::Null, &serde_json::Value::Null, &serde_json::json!("")));

    let (_, md) = h.export(&meta, ExportFormat::Markdown, ExportScope::Loaded, "zoo.md").await;
    let lines: Vec<&str> = md.lines().collect();
    assert_eq!(lines.len(), 4);
    assert!(lines[0].starts_with("| id | b | i2 | i8 | f4 |"), "{}", lines[0]);
    assert!(lines[1].starts_with("| ---: | --- | ---: | ---: | ---: |"), "{}", lines[1]);
    assert!(lines[3].starts_with("| 2 | NULL | NULL | NULL | NaN | -Infinity | NULL |  |"), "{}", lines[3]);

    let (_, script) = h.export(&meta, ExportFormat::SqlInsert, ExportScope::Loaded, "zoo.sql").await;
    assert!(script.lines().all(|l| l.starts_with(r#"INSERT INTO "lab"."type_zoo" ("id", "b", "i2", "#)), "{script}");
    let copy = unique("zoo_copy");
    let oracle = h.oracle().await;
    oracle.batch_execute(&format!("create table lab.{copy} (like lab.type_zoo)")).await.unwrap();
    let loaded = oracle.batch_execute(&script.replace(r#""lab"."type_zoo""#, &format!("lab.{copy}"))).await;
    let text = async |table: &str| -> Vec<String> {
        oracle.query(&format!("select t::text from lab.{table} t order by id"), &[]).await.unwrap().iter().map(|r| r.get(0)).collect()
    };
    let (copied, original) = (text(&copy).await, text("type_zoo").await);
    oracle.batch_execute(&format!("drop table lab.{copy}")).await.unwrap();
    loaded.unwrap();
    assert_eq!(copied, original, "the INSERT script rebuilds every awkward value");

    let state = h.run_ok(tab, "select o.id as order_id, 'x' as label from public.orders o order by o.id limit 1").await;
    let (_, placeholder) = h.export(&Harness::result(&state, 0), ExportFormat::SqlInsert, ExportScope::Loaded, "computed.sql").await;
    assert_eq!(placeholder, "INSERT INTO target_table (\"order_id\", \"label\") VALUES (1, 'x');\n", "a computed column has no single source table");
    let state = h.run_ok(tab, "select id as order_id from public.orders order by id limit 1").await;
    let (_, aliased) = h.export(&Harness::result(&state, 0), ExportFormat::SqlInsert, ExportScope::Loaded, "aliased.sql").await;
    assert_eq!(aliased, "INSERT INTO \"public\".\"orders\" (\"id\") OVERRIDING SYSTEM VALUE VALUES (1);\n", "the table's own column name, not the alias");
}

#[tokio::test]
async fn all_rows_streams_past_the_cap_inside_an_open_transaction_and_leaves_it_writable() {
    let mut h = Harness::start().await;
    h.engine.set_settings(Settings { max_rows: 1_000, ..h.engine.settings() });
    let probe = unique("t_writable");
    let oracle = h.oracle().await;
    oracle.batch_execute(&format!("create table public.{probe} (id int)")).await.unwrap();
    let tab = h.sql_tab(TxnMode::Manual).await;
    h.run_ok(tab, "create temp table t as select g, 'row ' || g as label from generate_series(1, 300000) g").await;
    let state = h.run_ok(tab, "select g, label, g % 2 = 0 as even from t order by g").await;
    let meta = Harness::result(&state, 0);
    assert_eq!((meta.truncated, &meta.all_rows, state.txn), (Some(TruncateReason::RowLimit), &AllRows::Statement, TxnState::Active));
    let history = h.history_len();

    let (summary, json) = h.export(&meta, ExportFormat::Json, ExportScope::All, "all.json").await;

    assert_eq!((summary.rows, summary.bytes, summary.truncated), (300_000, json.len() as u64, false));
    let rows: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap();
    assert_eq!(rows.len(), 300_000);
    assert_eq!(rows[299_999], serde_json::json!({ "g": 300000, "label": "row 300000", "even": true }));
    assert_eq!(h.page(&meta, 0, 1)[0][0].as_deref(), Some("1"), "the grid's result survives");
    assert_eq!(h.history_len(), history, "an export records no history");
    let after = h.console(tab);
    assert_eq!((after.txn, after.work, after.export), (TxnState::Active, None, None));
    // A read-only transaction may still write temp tables, so only a permanent table shows the
    // read-only savepoint was undone.
    h.run_ok(tab, &format!("insert into public.{probe} values (1)")).await;
    h.engine.console_rollback(tab).await.unwrap();
    oracle.batch_execute(&format!("drop table public.{probe}")).await.unwrap();
    h.drain();
    let exported: Vec<String> = h.log(tab).into_iter().filter(|l| l.starts_with("Exported ")).collect();
    assert_eq!(exported.len(), 1, "one Output line records the export");
    assert!(exported[0].starts_with("Exported 300,000 rows as JSON to "), "{}", exported[0]);
}

#[tokio::test]
async fn esc_during_all_rows_leaves_the_target_untouched_and_the_session_alive() {
    let mut h = Harness::start().await;
    h.engine.set_settings(Settings { max_rows: 100, ..h.engine.settings() });
    let tab = h.sql_tab(TxnMode::AutoCommit).await;
    let state = h.run_ok(tab, "select g, pg_sleep(0.001) from generate_series(1, 2000) g").await;
    let meta = Harness::result(&state, 0);
    assert_eq!(meta.all_rows, AllRows::Confirm { calls: vec!["pg_sleep".into()] }, "pg_sleep is not on the vetted list");
    let out = h.dir.path().join("out");
    std::fs::create_dir(&out).unwrap();
    let path = out.join("big.csv");
    std::fs::write(&path, "old").unwrap();
    let export = {
        let (engine, path) = (h.engine.clone(), path.display().to_string());
        tokio::spawn(async move { engine.result_export(meta.r#ref, ExportFormat::Csv, ExportScope::All, path).await })
    };
    let during = h.wait_console(tab, |s| s.export.as_ref().is_some_and(|p| p.rows > 0)).await;
    assert_eq!((during.work, during.export.as_ref().map(|p| (p.result, p.format))), (Some(ConsoleWork::Export), Some((meta.r#ref, ExportFormat::Csv))));
    assert!(matches!(h.engine.console_run(tab, RunRequest::Script { text: "select 1".into(), target: RunTarget::All, mode: RunMode::Run, params: Vec::new() }, None).await, Err(RunError::Busy)), "the export holds the session");
    h.engine.console_cancel(tab).await;

    assert!(matches!(export.await.unwrap(), Err(ExportError::Cancelled)));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "old");
    assert_eq!(std::fs::read_dir(&out).unwrap().count(), 1, "no temp file left beside it");
    let after = h.console(tab);
    assert_eq!((after.work, after.export, after.txn), (None, None, TxnState::Idle));
    assert!(matches!(after.session, SessionState::Open { .. }));
    assert_eq!(h.scalar(tab, "select 1").await, "1", "the session survived the cancel");
}

#[tokio::test]
async fn all_rows_refuses_a_write_and_a_nextval_rerun_fails_without_advancing() {
    let mut h = Harness::start().await;
    let tab = h.sql_tab(TxnMode::AutoCommit).await;
    h.run_ok(tab, "create temp table w (id int)").await;
    let state = h.run_ok(tab, "insert into w values (1), (2) returning id").await;
    let meta = Harness::result(&state, 0);
    assert_eq!(meta.all_rows, AllRows::Refused { reason: "The statement may write, so it does not run again.".into() });
    let path = h.dir.path().join("never.csv");
    assert!(matches!(h.engine.result_export(meta.r#ref, ExportFormat::Csv, ExportScope::All, path.display().to_string()).await, Err(ExportError::NotRerunnable)));
    assert!(!path.exists());
    assert_eq!(h.scalar(tab, "select count(*) from w").await, "2", "the insert did not run again");

    let seq = unique("s_export");
    let oracle = h.oracle().await;
    oracle.batch_execute(&format!("create sequence public.{seq}")).await.unwrap();
    let state = h.run_ok(tab, &format!("select nextval('public.{seq}') as n from generate_series(1, 3)")).await;
    let meta = Harness::result(&state, 0);
    assert_eq!(meta.all_rows, AllRows::Confirm { calls: vec!["nextval".into()] });
    let failed = h.engine.result_export(meta.r#ref, ExportFormat::Json, ExportScope::All, path.display().to_string()).await;
    let next = h.scalar(tab, &format!("select nextval('public.{seq}')")).await;
    oracle.batch_execute(&format!("drop sequence public.{seq}")).await.unwrap();
    assert!(matches!(&failed, Err(ExportError::Query { error }) if error.sqlstate.as_deref() == Some("25006")), "{failed:?}");
    assert_eq!(next, "4", "the read-only re-run did not advance the sequence");
    assert!(!path.exists());
    assert_eq!(h.console(tab).txn, TxnState::Idle);
}

#[tokio::test]
async fn a_table_tabs_all_rows_is_the_whole_filtered_table_in_page_order() {
    let mut h = Harness::start().await;
    h.engine.set_settings(Settings { table_page_size: 10, ..h.engine.settings() });
    let tab = h.sql_tab(TxnMode::AutoCommit).await;
    let query = TableQuery { r#where: "status <> 'cancelled'".into(), order_by: "total DESC".into(), ..Default::default() };
    let accepted = h.engine.console_run(tab, RunRequest::TablePage { schema: "public".into(), table: "orders".into(), page: 1, query }, None).await.unwrap();
    let state = h.wait_finished(tab, accepted.run_id).await;
    let meta = Harness::result(&state, 0);
    assert_eq!((meta.row_count, meta.truncated, &meta.all_rows), (10, None, &AllRows::Table));

    let (summary, csv) = h.export(&meta, ExportFormat::Csv, ExportScope::All, "orders.csv").await;
    let expected: Vec<String> = h
        .oracle()
        .await
        .query("select o.id from public.orders o where o.status <> 'cancelled' order by o.total desc, o.id", &[])
        .await
        .unwrap()
        .iter()
        .map(|r| r.get::<_, i64>(0).to_string())
        .collect();
    assert!(expected.len() > 10);
    let ids: Vec<&str> = csv.lines().skip(1).map(|l| l.split(',').next().unwrap()).collect();
    assert_eq!(summary.rows as usize, expected.len());
    assert_eq!(ids, expected, "every filtered row, in the page's order with its key tiebreak");

    let (_, script) = h.export(&meta, ExportFormat::SqlInsert, ExportScope::All, "orders.sql").await;
    assert_eq!(script.lines().count(), expected.len());
    assert!(script.lines().all(|l| l.starts_with(r#"INSERT INTO "public"."orders" ("id", "#)), "{}", script.lines().next().unwrap());
    h.drain();
    let log = h.log(tab);
    assert_eq!(log.iter().filter(|l| l.starts_with("0> ")).count(), 1, "only the page run logs a statement: {log:?}");
    assert_eq!(log.iter().filter(|l| l.starts_with("Exported ")).count(), 2, "{log:?}");
}

#[tokio::test]
async fn insert_exports_replay_identity_and_generated_columns_into_an_empty_copy() {
    let mut h = Harness::start().await;
    let tab = h.sql_tab(TxnMode::AutoCommit).await;
    let (table, copy) = (unique("t_ident"), unique("t_ident_copy"));
    let oracle = h.oracle().await;
    oracle
        .batch_execute(&format!(
            "create table public.{table} (id bigint generated always as identity primary key, label text not null, \
             width int generated always as (length(label)) stored, seq int generated by default as identity); \
             insert into public.{table} (label) values ('a'), ('bb'), ('ccc'); \
             create table public.{copy} (like public.{table} including identity including generated)"
        ))
        .await
        .unwrap();
    let state = h.run_ok(tab, &format!("select * from public.{table} order by id")).await;
    let meta = Harness::result(&state, 0);
    h.engine.refresh_catalog(h.db()).await.unwrap();

    let (_, script) = h.export(&meta, ExportFormat::SqlInsert, ExportScope::All, "ident.sql").await;
    let loaded = oracle.batch_execute(&script.replace(&format!(r#""public"."{table}""#), &format!("public.{copy}"))).await;
    let text = async |t: &str| -> Vec<String> { oracle.query(&format!("select t::text from public.{t} t order by id"), &[]).await.unwrap().iter().map(|r| r.get(0)).collect() };
    let (copied, original) = (text(&copy).await, text(&table).await);
    oracle.batch_execute(&format!("drop table public.{table}; drop table public.{copy}")).await.unwrap();
    assert_eq!(script.lines().next(), Some(format!(r#"INSERT INTO "public"."{table}" ("id", "label", "seq") OVERRIDING SYSTEM VALUE VALUES (1, 'a', 1);"#).as_str()), "{script}");
    loaded.unwrap();
    assert_eq!(copied, original, "the script rebuilds every row, keeping the identity values");
}

#[tokio::test]
async fn all_rows_refuses_a_rerun_whose_column_types_changed() {
    let mut h = Harness::start().await;
    let tab = h.sql_tab(TxnMode::AutoCommit).await;
    let table = unique("t_retype");
    let oracle = h.oracle().await;
    oracle.batch_execute(&format!("create table public.{table} (id int, n int); insert into public.{table} values (1, 123)")).await.unwrap();
    let state = h.run_ok(tab, &format!("select id, n from public.{table}")).await;
    let meta = Harness::result(&state, 0);
    oracle.batch_execute(&format!("alter table public.{table} alter column n type text")).await.unwrap();

    let path = h.dir.path().join("retyped.json");
    let exported = h.engine.result_export(meta.r#ref, ExportFormat::Json, ExportScope::All, path.display().to_string()).await;
    oracle.batch_execute(&format!("drop table public.{table}")).await.unwrap();
    assert!(matches!(exported, Err(ExportError::ShapeChanged)), "{exported:?}");
    assert!(!path.exists(), "no file when the column types changed");
}

#[tokio::test]
async fn history_survives_reopen_on_the_same_dir() {
    let dir = tempfile::tempdir().unwrap();
    let marker = unique("select 'hist");
    let sql = format!("{marker}'");
    {
        let mut h = Harness::start_in(dir).await;
        let tab = h.sql_tab(TxnMode::AutoCommit).await;
        h.run_ok(tab, &sql).await;
        let h2 = Harness::start_in(h.dir).await;
        let page = h2.engine.history(HistoryQuery { text: Some(marker.clone()), connection_id: None, before_id: None, limit: 5 });
        assert_eq!(page.entries.len(), 1);
        assert_eq!(page.entries[0].sql, sql);
        assert_eq!(page.entries[0].connection_name, "dev");
        assert_eq!(page.entries[0].database, "shop");
        assert!(h2.engine.boot().consoles.is_empty(), "consoles are not persisted");
        h2.engine.clear_history();
        assert!(h2.engine.history(HistoryQuery { text: None, connection_id: None, before_id: None, limit: 5 }).entries.is_empty());
    }
}

#[tokio::test]
async fn schema_and_search_path_follow_the_console_config() {
    let mut h = Harness::start().await;
    let tab = h.sql_tab_in(TxnMode::AutoCommit, Some("lab")).await;
    assert_eq!(h.scalar(tab, "select count(*) from type_zoo").await, "2");
    assert_eq!(h.scalar(tab, "show search_path").await, "lab");
    h.engine.console_configure(tab, ConsoleConfig { connection_id: h.conn, database: "shop".into(), schema: Some("public".into()), mode: TxnMode::AutoCommit }).await.unwrap();
    assert_eq!(h.scalar(tab, "show search_path").await, "public");
    assert_eq!(h.scalar(tab, "show application_name").await, "L1 Data");
    for copy in ["copy customers from stdin", "copy public.customers (id) to stdout", "copy pg_temp.nope from stdin with (format csv)"] {
        let err = h.engine.console_run(tab, RunRequest::Script { text: copy.into(), target: RunTarget::All, mode: RunMode::Run, params: Vec::new() }, None).await.err();
        assert!(matches!(err, Some(RunError::Unsupported { .. })), "{copy}: {err:?}");
    }
    assert_eq!(h.scalar(tab, "select 5").await, "5", "the session survived the rejected COPYs");
    assert!(matches!(h.engine.console_run(tab, RunRequest::Script { text: " -- nothing\n".into(), target: RunTarget::All, mode: RunMode::Run, params: Vec::new() }, None).await, Err(RunError::NoStatement)));
    h.engine.console_close(tab, false).await.unwrap();
    assert!(h.engine.boot().consoles.is_empty());
    h.engine.disconnect(h.conn, false).await.unwrap();
    assert!(matches!(h.engine.boot().live[0].status, ConnStatus::Disconnected));
    assert!(matches!(h.engine.catalog(h.db()).await, Err(CatalogError::NotConnected)));
}

#[tokio::test]
async fn bad_credentials_fail_with_auth_and_test_connection_works() {
    let h = Harness::start().await;
    let mut draft = draft_from_url(&url(), "x").unwrap();
    assert!(h.engine.test_connection(draft.clone()).await.unwrap().version.starts_with("16."));
    draft.password = PasswordInput::Set("wrong".into());
    assert!(matches!(h.engine.test_connection(draft.clone()).await, Err(ConnectError::Auth { .. })));
    draft.port = 1;
    assert!(matches!(h.engine.test_connection(draft).await, Err(ConnectError::Network { .. })));
}

fn cell(column: &str, value: Option<&str>) -> CellWrite {
    CellWrite { column: column.into(), value: value.map(Into::into) }
}

fn changes(table: &str) -> TableChanges {
    TableChanges { schema: "public".into(), table: table.into(), inserts: vec![], updates: vec![], deletes: vec![] }
}

async fn edit_table(h: &mut Harness, tab: TabId) -> String {
    let table = unique("t_edit");
    h.run_ok(tab, &format!("create table public.{table} (id serial primary key, name text not null, note text); insert into public.{table} (name) values ('a'), ('b'), ('c')")).await;
    h.engine.refresh_catalog(h.db()).await.unwrap();
    table
}

async fn rows_of(h: &mut Harness, tab: TabId, table: &str) -> String {
    h.scalar(tab, &format!("select coalesce(string_agg(id || ':' || name || ':' || coalesce(note, 'NULL'), ' ' order by id), '') from public.{table}")).await
}

#[tokio::test]
async fn table_apply_commits_every_change_or_none() {
    let mut h = Harness::start().await;
    h.engine.connect(h.conn).await.unwrap();
    let tab = h.sql_tab(TxnMode::AutoCommit).await;
    let table = edit_table(&mut h, tab).await;

    let mut ok = changes(&table);
    ok.deletes.push(vec![cell("id", Some("1"))]);
    ok.updates.push(RowUpdate { key: vec![cell("id", Some("2"))], set: vec![cell("name", Some("b'2")), cell("note", Some("x"))] });
    ok.inserts.push(vec![cell("name", Some("d"))]);
    let before = h.log(tab).len();
    let summary = h.engine.table_apply(tab, ok, None).await.unwrap();
    assert!(summary.committed);
    h.drain();
    assert_eq!(
        h.log(tab)[before..],
        [
            format!("0> DELETE FROM \"public\".\"{table}\" WHERE \"id\" = '1'"),
            "0 DELETE 1".into(),
            format!("1> UPDATE \"public\".\"{table}\" SET \"name\" = 'b''2', \"note\" = 'x' WHERE \"id\" = '2'"),
            "1 UPDATE 1".into(),
            format!("2> INSERT INTO \"public\".\"{table}\" (\"name\") VALUES ('d')"),
            "2 INSERT 0 1".into(),
        ]
    );
    assert_eq!(rows_of(&mut h, tab, &table).await, "2:b'2:x 3:c:NULL 4:d:NULL");
    assert_eq!(h.console(tab).txn, TxnState::Idle);

    let mut missing = changes(&table);
    missing.inserts.push(vec![cell("name", Some("e"))]);
    missing.updates.push(RowUpdate { key: vec![cell("id", Some("999"))], set: vec![cell("name", Some("ghost"))] });
    let err = h.engine.table_apply(tab, missing, None).await.unwrap_err();
    assert!(matches!(err, ApplyError::RowMismatch { change: ChangeRef { kind: ChangeKind::Update, index: 0 }, rows: 0 }), "{err:?}");

    let mut violating = changes(&table);
    violating.deletes.push(vec![cell("id", Some("3"))]);
    violating.inserts.push(vec![cell("note", Some("no name"))]);
    h.drain();
    let before = h.log(tab).len();
    let err = h.engine.table_apply(tab, violating, None).await.unwrap_err();
    assert!(matches!(&err, ApplyError::Failed { change: ChangeRef { kind: ChangeKind::Insert, index: 0 }, error } if error.sqlstate.as_deref() == Some("23502")), "{err:?}");
    h.drain();
    let failed = &h.log(tab)[before..];
    assert_eq!(failed.len(), 4, "{failed:?}");
    assert_eq!(failed[1], "0 DELETE 1");
    assert!(failed[3].starts_with("Some(1) ERROR: null value in column \"name\""), "{failed:?}");

    assert_eq!(rows_of(&mut h, tab, &table).await, "2:b'2:x 3:c:NULL 4:d:NULL", "failed applies left nothing behind");
    assert_eq!(h.console(tab).txn, TxnState::Idle);

    let mut keyless = changes(&table);
    keyless.deletes.push(vec![cell("name", Some("c"))]);
    assert!(matches!(h.engine.table_apply(tab, keyless, None).await, Err(ApplyError::NoPrimaryKey)));
    h.run_ok(tab, &format!("drop table public.{table}")).await;
}

#[tokio::test]
async fn table_apply_in_manual_mode_waits_for_commit_and_a_failure_keeps_the_open_transaction() {
    let mut h = Harness::start().await;
    h.engine.connect(h.conn).await.unwrap();
    let auto = h.sql_tab(TxnMode::AutoCommit).await;
    let table = edit_table(&mut h, auto).await;
    let manual = h.sql_tab(TxnMode::Manual).await;

    let mut insert = changes(&table);
    insert.inserts.push(vec![cell("name", Some("d"))]);
    let summary = h.engine.table_apply(manual, insert, None).await.unwrap();
    assert!(!summary.committed);
    assert_eq!(h.console(manual).txn, TxnState::Active);
    assert_eq!(rows_of(&mut h, auto, &table).await, "1:a:NULL 2:b:NULL 3:c:NULL", "other sessions don't see it before Commit");

    let mut failing = changes(&table);
    failing.updates.push(RowUpdate { key: vec![cell("id", Some("1"))], set: vec![cell("name", None)] });
    assert!(matches!(h.engine.table_apply(manual, failing, None).await, Err(ApplyError::Failed { .. })));
    assert_eq!(h.console(manual).txn, TxnState::Active, "the savepoint kept the transaction usable");
    assert_eq!(rows_of(&mut h, manual, &table).await, "1:a:NULL 2:b:NULL 3:c:NULL 4:d:NULL", "earlier work in the transaction survives");

    h.engine.console_commit(manual, None).await.unwrap();
    assert_eq!(rows_of(&mut h, auto, &table).await, "1:a:NULL 2:b:NULL 3:c:NULL 4:d:NULL");
    h.run_ok(auto, &format!("drop table public.{table}")).await;
}

#[tokio::test]
async fn table_pages_sort_and_filter_in_the_engine() {
    let mut h = Harness::start().await;
    h.engine.connect(h.conn).await.unwrap();
    let tab = h.sql_tab(TxnMode::AutoCommit).await;
    let query = TableQuery {
        order_by: r#""created_at" DESC"#.into(),
        filters: vec![ColumnFilter { column: "email".into(), op: FilterOp::StartsWith, value: "user1".into() }],
        r#where: "region = 'APAC'".into(),
    };
    let page = h
        .engine
        .console_run(tab, RunRequest::TablePage { schema: "public".into(), table: "customers".into(), page: 0, query: query.clone() }, None)
        .await
        .unwrap();
    let state = h.wait_finished(tab, page.run_id).await;
    let meta = Harness::result(&state, 0);
    let rows = h.page(&meta, 0, meta.row_count);
    let expected = h
        .scalar(tab, "select count(*) from customers where region = 'APAC' and email ilike 'user1%'")
        .await
        .parse::<u32>()
        .unwrap();
    assert_eq!(meta.row_count, expected.min(500));
    assert!(rows.iter().all(|r| r[3].as_deref() == Some("APAC") && r[1].as_deref().unwrap().starts_with("user1")));
    let created: Vec<&str> = rows.iter().map(|r| r[4].as_deref().unwrap()).collect();
    assert!(created.windows(2).all(|w| w[0] >= w[1]), "newest first");
    assert_eq!(h.engine.exact_row_count(tab, h.db(), "public".into(), "customers".into(), query).await.unwrap(), u64::from(expected));

    for smuggled in [
        TableQuery { r#where: "true; drop table customers".into(), ..Default::default() },
        TableQuery { order_by: "id; drop table customers".into(), ..Default::default() },
    ] {
        let refused = h.engine.console_run(tab, RunRequest::TablePage { schema: "public".into(), table: "customers".into(), page: 0, query: smuggled }, None).await;
        assert!(matches!(refused, Err(RunError::BadFilter)), "{refused:?}");
    }
}

#[tokio::test]
async fn sql_comments_in_where_and_order_by_keep_the_page_limit() {
    let mut h = Harness::start().await;
    h.engine.connect(h.conn).await.unwrap();
    let tab = h.sql_tab(TxnMode::AutoCommit).await;
    let query = TableQuery { order_by: "id DESC -- newest first".into(), filters: vec![], r#where: "region = 'APAC' -- only asia".into() };
    let page = h
        .engine
        .console_run(tab, RunRequest::TablePage { schema: "public".into(), table: "customers".into(), page: 0, query: query.clone() }, None)
        .await
        .unwrap();
    let state = h.wait_finished(tab, page.run_id).await;
    let meta = Harness::result(&state, 0);
    assert_eq!(meta.row_count, 500, "the comment must not swallow LIMIT");
    let apac = h.scalar(tab, "select count(*) from customers where region = 'APAC'").await.parse::<u64>().unwrap();
    assert_eq!(h.engine.exact_row_count(tab, h.db(), "public".into(), "customers".into(), query).await.unwrap(), apac);
}

#[tokio::test]
async fn table_page_preserves_nested_domain_native_metadata_and_primary_key() {
    let mut h = Harness::start().await;
    h.engine.connect(h.conn).await.unwrap();
    let tab = h.sql_tab(TxnMode::AutoCommit).await;
    let oracle = h.oracle().await;
    let schema = unique("nested_domains");
    oracle
        .batch_execute(&format!(
            "CREATE SCHEMA {schema};
             CREATE DOMAIN {schema}.first_level AS int;
             CREATE DOMAIN {schema}.second_level AS {schema}.first_level;
             CREATE TABLE {schema}.items (id int PRIMARY KEY, value {schema}.second_level);
             INSERT INTO {schema}.items VALUES (1, 42)"
        ))
        .await
        .unwrap();
    let check_schema = schema.clone();
    let engine = h.engine.clone();
    let checked = tokio::spawn(async move {
        let schema = check_schema;
        h.engine.refresh_catalog(h.db()).await.unwrap();
        let catalog = h.engine.catalog(h.db()).await.unwrap();
        let relation = catalog.relation(&schema, "items").unwrap();
        let direct = h.run_ok(tab, &format!("SELECT * FROM {schema}.items")).await;
        let native = Harness::result(&direct, 0);
        assert_eq!(native.columns[1].r#type.category, TypeCategory::Integer);

        let accepted = h.engine.console_run(tab, RunRequest::TablePage {
            schema: schema.clone(), table: "items".into(), page: 0, query: TableQuery::default(),
        }, None).await.unwrap();
        let state = h.wait_finished(tab, accepted.run_id).await;
        assert!(matches!(state.run, RunState::Finished { outcome: RunOutcome::Ok, .. }), "{:?}", state.run);
        let meta = Harness::result(&state, 0);
        assert_eq!(serde_json::to_value(&meta.columns).unwrap(), serde_json::to_value(&native.columns).unwrap());
        let rows = h.page(&meta, 0, 1);
        assert_eq!(rows, vec![vec![Some("1".into()), Some("42".into())]]);
        let key = meta.columns.iter().zip(&rows[0]).filter_map(|(column, value)| {
            let source = column.source?;
            (source.table_oid == relation.oid && relation.columns.iter().any(|known| known.attnum == source.attnum && known.pk))
                .then(|| cell(&column.name, value.as_deref()))
        }).collect::<Vec<_>>();
        assert_eq!(key.len(), 1, "native provenance identifies the complete primary key");
        assert_eq!(key[0].column, "id");
        let changes = TableChanges {
            schema: schema.clone(), table: "items".into(), inserts: vec![], deletes: vec![],
            updates: vec![RowUpdate { key, set: vec![cell("value", Some("43"))] }],
        };
        assert!(h.engine.table_apply(tab, changes, None).await.unwrap().committed);
        assert_eq!(h.scalar(tab, &format!("SELECT value FROM {schema}.items WHERE id = 1")).await, "43");
    }).await;
    oracle.batch_execute(&format!("DROP SCHEMA {schema} CASCADE")).await.unwrap();
    let remaining: i64 = oracle.query_one("SELECT count(*) FROM pg_namespace WHERE nspname = $1", &[&schema]).await.unwrap().get(0);
    assert_eq!(remaining, 0);
    engine.shutdown().await;
    checked.unwrap();
}

#[tokio::test]
async fn table_page_on_second_database_uses_its_pk() {
    let mut h = Harness::start().await;
    let tab = h.sql_tab_on(TxnMode::AutoCommit, "reporting").await;
    let page = h
        .engine
        .console_run(tab, RunRequest::TablePage { schema: "public".into(), table: "daily".into(), page: 0, query: TableQuery::default() }, None)
        .await
        .unwrap();
    let state = h.wait_finished(tab, page.run_id).await;
    assert_eq!(Harness::result(&state, 0).row_count, 90);
    let logged: Vec<String> = h.entries(tab).iter().filter(|e| e.run_id == page.run_id).map(|e| log_line(&e.body)).collect();
    assert_eq!(logged, ["0> SELECT * FROM \"public\".\"daily\" ORDER BY \"day\" LIMIT 500 OFFSET 0", "0 rows 90"]);
    assert_eq!(h.scalar(tab, "select current_database()").await, "reporting");

    let reporting = h.engine.catalog(h.db_named("reporting")).await.unwrap();
    assert_eq!(reporting.db, h.db_named("reporting"));
    assert_eq!(reporting.schemas.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["audit", "information_schema", "pg_catalog", "public"]);
    assert_eq!(reporting.relation("public", "daily").unwrap().pk_columns(), ["day"]);
    assert!(h.engine.catalog(h.db()).await.unwrap().relation("public", "daily").is_none(), "shop's catalog is a different database");

    let mut changes = TableChanges { schema: "public".into(), table: "daily".into(), inserts: vec![], updates: vec![], deletes: vec![] };
    changes.updates.push(RowUpdate { key: vec![cell("day", Some("2026-01-02"))], set: vec![cell("orders", Some("101"))] });
    assert!(h.engine.table_apply(tab, changes, None).await.unwrap().committed, "the full key of daily is its own primary key");
    h.run_ok(tab, "update daily set orders = 101 where day = '2026-01-02'").await;
}

#[tokio::test]
async fn catalog_versions_rise_across_reconnect() {
    let h = Harness::start().await;
    h.engine.connect(h.conn).await.unwrap();
    let started = Instant::now();
    while h.catalog_status("shop") != (CatalogStatus::Ready { version: 1 }) {
        assert!(started.elapsed() < Duration::from_secs(10), "connect loads the home catalog by itself: {:?}", h.catalog_status("shop"));
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(h.engine.catalog(h.db()).await.unwrap().version, 1);
    assert_eq!(h.engine.refresh_catalog(h.db()).await.unwrap(), 2);
    h.engine.disconnect(h.conn, false).await.unwrap();
    assert!(matches!(h.engine.catalog(h.db()).await, Err(CatalogError::NotConnected)));
    h.engine.connect(h.conn).await.unwrap();
    assert_eq!(h.engine.catalog(h.db()).await.unwrap().version, 3, "a reconnect never reissues a version the UI has seen");
    assert_eq!(h.catalog_status("shop"), CatalogStatus::Ready { version: 3 });
}

#[tokio::test]
async fn ddl_burst_coalesces_refreshes() {
    let mut h = Harness::start().await;
    h.engine.connect(h.conn).await.unwrap();
    let tab = h.sql_tab(TxnMode::AutoCommit).await;
    let gate = unique("t_gate");
    h.run_ok(tab, &format!("create table public.{gate} (id int); insert into public.{gate} values (1)")).await;
    let before = h.catalog_when(&h.db(), |c| c.relation("public", &gate).is_some()).await.version;

    // A count holds the metadata session, so every refresh the burst schedules queues behind it.
    let (engine, db, held) = (h.engine.clone(), h.db(), gate.clone());
    let slow = tokio::spawn(async move {
        let query = TableQuery { r#where: "pg_sleep(3)::text = ''".into(), ..Default::default() };
        engine.exact_row_count(tab, db, "public".into(), held, query).await
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    let tables: Vec<String> = (0..30).map(|i| format!("{}_{i}", unique("t_burst"))).collect();
    let script = tables.iter().map(|t| format!("create table public.{t} (id int)")).collect::<Vec<_>>().join(";\n");
    h.run_ok(tab, &script).await;
    assert_eq!(slow.await.unwrap().unwrap(), 1);

    h.catalog_when(&h.db(), |c| tables.iter().all(|t| c.relation("public", t).is_some())).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let settled = h.engine.catalog(h.db()).await.unwrap().version;
    assert_eq!(settled - before, 2, "30 DDL statements caused exactly two loads");
    let drops = tables.iter().chain([&gate]).map(|t| format!("drop table public.{t}")).collect::<Vec<_>>().join(";\n");
    h.run_ok(tab, &drops).await;
}

#[tokio::test]
async fn history_records_console_database() {
    let mut h = Harness::start().await;
    let shop = h.sql_tab(TxnMode::AutoCommit).await;
    let reporting = h.sql_tab_on(TxnMode::AutoCommit, "reporting").await;
    let marker = unique("select 'db");
    h.run_ok(shop, &format!("{marker}'")).await;
    h.run_ok(reporting, &format!("{marker}'")).await;
    let page = h.engine.history(HistoryQuery { text: Some(marker), connection_id: Some(h.conn), before_id: None, limit: 10 });
    let databases: Vec<&str> = page.entries.iter().map(|e| e.database.as_str()).collect();
    assert_eq!(databases, ["reporting", "shop"], "newest first, each with the database its console ran in");
}

#[tokio::test]
async fn a_database_no_tab_or_tree_uses_gets_no_catalog() {
    let h = Harness::start().await;
    h.engine.connect(h.conn).await.unwrap();
    let postgres = h.db_named("postgres");
    assert!(matches!(h.engine.catalog(postgres.clone()).await, Err(CatalogError::NotShown { ref database }) if database == "postgres"));
    assert!(matches!(h.engine.catalog(h.db_named("nope")).await, Err(CatalogError::UnknownDatabase { ref database }) if database == "nope"));
    assert_eq!(h.catalog_status("postgres"), CatalogStatus::Idle);

    let tab = h.sql_tab_on(TxnMode::AutoCommit, "postgres").await;
    let catalog = h.engine.catalog(postgres.clone()).await.unwrap();
    assert_eq!(catalog.schemas.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["information_schema", "pg_catalog", "public"]);
    assert!(matches!(h.catalog_status("postgres"), CatalogStatus::Ready { .. }));

    h.engine.console_close(tab, false).await.unwrap();
    assert!(matches!(h.engine.catalog(postgres).await, Err(CatalogError::NotShown { .. })), "closing the console closes its slot");
    assert_eq!(h.catalog_status("postgres"), CatalogStatus::Idle);

    let live = h.engine.boot().live;
    let ConnStatus::Connected { instance, .. } = &live[0].status else { panic!("{:?}", live[0].status) };
    assert_eq!(instance.home, "shop");
    // Databases other tests create and drop while this one runs are not part of the seed.
    let rows: Vec<(&str, bool, DbAccess, bool)> =
        instance.databases.iter().filter(|d| !d.name.starts_with("t_db_")).map(|d| (d.name.as_str(), d.system, d.access, d.shown)).collect();
    assert_eq!(
        rows,
        [
            ("postgres", false, DbAccess::Ok, false),
            ("reporting", false, DbAccess::Ok, false),
            ("shop", false, DbAccess::Ok, true),
            ("template0", true, DbAccess::NoConnections, false),
            ("template1", true, DbAccess::Ok, false),
        ]
    );
}

/// Connect, use a second database from a console, then disconnect: home metadata, `reporting`
/// metadata, and the console session must all be gone from the server.
#[tokio::test]
async fn disconnect_closes_every_metadata_session() {
    let role = Role::create().await;
    let mut h = Harness::start_as(&role).await;
    let tab = h.sql_tab_on(TxnMode::AutoCommit, "reporting").await;
    h.run_ok(tab, "select 1").await;
    h.catalog_when(&h.db_named("reporting"), |_| true).await;
    let open: Vec<String> = role.backends().await.into_iter().map(|(db, _)| db).collect();
    assert_eq!(open, ["reporting", "reporting", "shop"], "metadata on shop and reporting, plus the console on reporting");

    h.engine.disconnect(h.conn, false).await.unwrap();
    let left = role.backends_when(|b| b.is_empty()).await;
    assert_eq!(left, [], "a disconnect closes every session the instance opened");
    role.drop().await;
}

/// Losing the home session loses the instance, and the other databases' metadata sessions go
/// with it. The console's own session is not the instance's and stays.
#[tokio::test]
async fn a_lost_home_session_closes_the_other_metadata_sessions() {
    let role = Role::create().await;
    let mut h = Harness::start_as(&role).await;
    let tab = h.sql_tab_on(TxnMode::AutoCommit, "reporting").await;
    let state = h.run_ok(tab, "select 1").await;
    let SessionState::Open { backend_pid: console_pid } = state.session else { panic!("{:?}", state.session) };
    h.catalog_when(&h.db_named("reporting"), |_| true).await;
    let home: Vec<i32> = role.backends().await.into_iter().filter(|(db, _)| db == "shop").map(|(_, pid)| pid).collect();
    let [home_pid] = home[..] else { panic!("one home metadata session, found {home:?}") };

    role.oracle.simple_query(&format!("select pg_terminate_backend({home_pid})")).await.unwrap();
    let status = h.wait_status(|s| !matches!(s, ConnStatus::Connected { .. })).await;
    assert!(matches!(status, ConnStatus::Lost { .. }), "{status:?}");
    let left = role.backends_when(|b| b.len() == 1).await;
    assert_eq!(left, [("reporting".to_owned(), console_pid)], "only the console's session survives a loss");
    role.drop().await;
}

/// `ALTER DATABASE` inside a Manual-mode transaction is invisible to the home session until it
/// commits, so the list is re-read at commit, as the catalog is after transactional DDL.
#[tokio::test]
async fn alter_database_in_a_manual_transaction_refreshes_the_list_on_commit() {
    let mut h = Harness::start().await;
    let auto = h.sql_tab(TxnMode::AutoCommit).await;
    let (old, new) = (unique("t_db"), unique("t_db"));
    h.run_ok(auto, &format!("create database {old}")).await;
    h.wait_database_names(|names| names.contains(&old)).await;

    let manual = h.sql_tab(TxnMode::Manual).await;
    let state = h.run_ok(manual, &format!("alter database {old} rename to {new}")).await;
    assert_eq!(state.txn, TxnState::Active);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let names = h.database_names();
    assert!(names.contains(&old) && !names.contains(&new), "an uncommitted rename changes nothing: {names:?}");

    h.engine.console_commit(manual, None).await.unwrap();
    let names = h.wait_database_names(|names| names.contains(&new)).await;
    assert!(!names.contains(&old), "the committed rename replaced the old name: {names:?}");
    h.run_ok(auto, &format!("drop database {new}")).await;
    h.wait_database_names(|names| !names.contains(&new)).await;
}

/// A console-only database whose metadata session dies keeps its last catalog on the wire, and
/// the next `catalog()` reopens the session and reloads.
#[tokio::test]
async fn a_dead_metadata_session_keeps_the_last_catalog_until_the_next_load() {
    let role = Role::create().await;
    let mut h = Harness::start_as(&role).await;
    let tab = h.sql_tab_on(TxnMode::AutoCommit, "reporting").await;
    let state = h.run_ok(tab, "select 1").await;
    let SessionState::Open { backend_pid: console_pid } = state.session else { panic!("{:?}", state.session) };
    let reporting = h.db_named("reporting");
    let before_catalog = h.catalog_when(&reporting, |_| true).await;
    let before = before_catalog.version;
    let daily = before_catalog.relation("public", "daily").unwrap().oid;
    let meta: Vec<i32> = role.backends().await.into_iter().filter(|(db, pid)| db == "reporting" && *pid != console_pid).map(|(_, pid)| pid).collect();
    let [meta_pid] = meta[..] else { panic!("one reporting metadata session, found {meta:?}") };

    role.oracle.simple_query(&format!("select pg_terminate_backend({meta_pid})")).await.unwrap();
    let started = Instant::now();
    while !matches!(h.catalog_status("reporting"), CatalogStatus::Failed { .. }) {
        assert!(started.elapsed() < Duration::from_secs(10), "{:?}", h.catalog_status("reporting"));
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let CatalogStatus::Failed { version, message } = h.catalog_status("reporting") else { unreachable!() };
    assert_eq!(version, Some(before), "the last good version stays on the wire: {message}");
    assert!(matches!(h.status(), ConnStatus::Connected { .. }), "another database's session is not the home session");

    let detail = h.engine.relation_detail(reporting.clone(), daily).await.unwrap();
    assert_eq!(detail.catalog_version, before, "a detail read is not a catalog load");
    assert_eq!(h.catalog_status("reporting"), CatalogStatus::Failed { message, version: Some(before) });

    let reloaded = h.engine.catalog(reporting.clone()).await.unwrap();
    assert!(reloaded.version > before, "the next load reopened the session: {} > {before}", reloaded.version);
    assert_eq!(h.catalog_status("reporting"), CatalogStatus::Ready { version: reloaded.version });
    h.engine.disconnect(h.conn, false).await.unwrap();
    role.drop().await;
}

#[tokio::test]
async fn relation_detail_reads_orders() {
    let h = Harness::start().await;
    h.engine.connect(h.conn).await.unwrap();
    let catalog = h.engine.catalog(h.db()).await.unwrap();
    let orders = catalog.relation("public", "orders").unwrap();
    // By name: a dev database migrated in place has the same columns in another attnum order.
    let mut columns: Vec<(&str, TypeCategory, bool, bool)> = orders.columns.iter().map(|c| (c.name.as_str(), c.category, c.pk, c.fk)).collect();
    columns.sort_by_key(|c| c.0);
    assert_eq!(
        columns,
        [
            ("created_at", TypeCategory::Temporal, false, false),
            ("customer_id", TypeCategory::Integer, false, true),
            ("id", TypeCategory::Integer, true, false),
            ("metadata", TypeCategory::Json, false, false),
            ("number", TypeCategory::Text, false, false),
            ("ship_address_id", TypeCategory::Integer, false, true),
            ("status", TypeCategory::Text, false, false),
            ("total", TypeCategory::Numeric, false, false),
            ("updated_at", TypeCategory::Temporal, false, false),
        ]
    );
    let zoo = catalog.relation("lab", "type_zoo").unwrap();
    let category = |name: &str| zoo.columns.iter().find(|c| c.name == name).unwrap().category;
    assert_eq!(
        [category("m"), category("arr_i"), category("rng"), category("b")],
        [TypeCategory::Enum, TypeCategory::Array, TypeCategory::Range, TypeCategory::Bool]
    );

    let detail = h.engine.relation_detail(h.db(), orders.oid).await.unwrap();
    assert_eq!((detail.oid, detail.catalog_version), (orders.oid, catalog.version));
    let expr = |sql: &str| DefaultValue::Expr { sql: sql.into() };
    let mut defaults: Vec<(&str, DefaultValue)> = detail.defaults.iter().map(|d| (d.column.as_str(), d.value.clone())).collect();
    defaults.sort_by_key(|d| d.0);
    assert_eq!(
        defaults,
        [
            ("created_at", expr("now()")),
            ("id", DefaultValue::Identity { always: true }),
            ("metadata", expr("'{}'")),
            ("status", expr("'pending'")),
            ("updated_at", expr("now()")),
        ]
    );

    let names = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let foreign = |table: &str, cols: &[&str]| KeyKind::Foreign {
        ref_schema: "public".into(),
        ref_table: table.into(),
        ref_columns: names(cols),
        on_delete: FkAction::NoAction,
        on_update: FkAction::NoAction,
    };
    assert_eq!(
        detail.keys,
        [
            KeyInfo { name: "orders_pkey".into(), columns: names(&["id"]), kind: KeyKind::Primary },
            KeyInfo { name: "orders_customer_id_fkey".into(), columns: names(&["customer_id"]), kind: foreign("customers", &["id"]) },
            KeyInfo {
                name: "orders_customer_id_ship_address_id_fkey".into(),
                columns: names(&["customer_id", "ship_address_id"]),
                kind: foreign("addresses", &["customer_id", "id"]),
            },
            KeyInfo { name: "orders_number_key".into(), columns: names(&["number"]), kind: KeyKind::Unique },
        ]
    );

    let indexes: Vec<(&str, bool, bool, &str, Option<&str>)> =
        detail.indexes.iter().map(|i| (i.name.as_str(), i.unique, i.primary, i.keys.as_str(), i.predicate.as_deref())).collect();
    assert_eq!(
        indexes,
        [
            ("orders_pkey", true, true, "(id)", None),
            ("orders_number_key", true, false, "(number)", None),
            ("orders_created_at_idx", false, false, "(created_at DESC)", None),
            ("orders_customer_id_idx", false, false, "(customer_id)", None),
            ("orders_open_idx", false, false, "(status)", Some("status <> 'paid'::text")),
        ]
    );
    assert!(detail.indexes.iter().all(|i| i.method == "btree" && i.valid));

    let trigger = |name: &str, timing, events: &[TriggerEvent], function: &str| TriggerInfo {
        name: name.into(),
        timing,
        events: events.to_vec(),
        per_row: true,
        enabled: true,
        implementation: TriggerImplementation::Function { name: function.into() },
    };
    assert_eq!(
        detail.triggers,
        [
            trigger("orders_audit", TriggerTiming::After, &[TriggerEvent::Insert, TriggerEvent::Update, TriggerEvent::Delete], "audit_order"),
            trigger("orders_set_updated_at", TriggerTiming::Before, &[TriggerEvent::Update], "set_updated_at"),
        ]
    );

    assert!(matches!(h.engine.relation_detail(h.db(), 0).await, Err(DetailError::Gone)));
    assert!(matches!(
        h.engine.relation_detail(h.db_named("postgres"), orders.oid).await,
        Err(DetailError::Catalog { error: CatalogError::NotShown { .. } })
    ));
}

#[tokio::test]
async fn relation_detail_hides_partition_clones_and_shows_inherited_triggers() {
    let h = Harness::start().await;
    h.engine.connect(h.conn).await.unwrap();
    let (oracle, connection) = tokio_postgres::connect(&url(), NoTls).await.unwrap();
    tokio::spawn(connection);
    // A scratch schema would race the tests that count shop's schemas, so the tables share public.
    let s = unique("t_detail");
    oracle
        .batch_execute(&format!(
            "create table public.{s}_ptarget (id int, region text, primary key (id, region)) partition by list (region);
             create table public.{s}_ptarget_eu partition of public.{s}_ptarget for values in ('eu');
             create table public.{s}_ptarget_us partition of public.{s}_ptarget for values in ('us');
             create table public.{s}_t (a int, region text, constraint fk_part foreign key (a, region) references public.{s}_ptarget);
             create function public.{s}_noop() returns trigger language plpgsql as $$ begin return new; end $$;
             create trigger t_replica after insert on public.{s}_t for each row execute function public.{s}_noop();
             alter table public.{s}_t enable replica trigger t_replica;
             create table public.{s}_parted (id int) partition by range (id);
             create table public.{s}_parted_1 partition of public.{s}_parted for values from (0) to (10);
             create trigger t_parent after insert on public.{s}_parted for each row execute function public.{s}_noop();"
        ))
        .await
        .unwrap();
    let oid = async |table: &str| oracle.query_one(&format!("select 'public.{s}_{table}'::regclass::oid"), &[]).await.unwrap().get::<_, u32>(0);

    let t = h.engine.relation_detail(h.db(), oid("t").await).await.unwrap();
    let names = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(
        t.keys,
        [KeyInfo {
            name: "fk_part".into(),
            columns: names(&["a", "region"]),
            kind: KeyKind::Foreign {
                ref_schema: "public".into(),
                ref_table: format!("{s}_ptarget"),
                ref_columns: names(&["id", "region"]),
                on_delete: FkAction::NoAction,
                on_update: FkAction::NoAction,
            },
        }]
    );
    let trigger = |name: &str, enabled| TriggerInfo {
        name: name.into(),
        timing: TriggerTiming::After,
        events: vec![TriggerEvent::Insert],
        per_row: true,
        enabled,
        implementation: TriggerImplementation::Function { name: format!("{s}_noop") },
    };
    assert_eq!(t.triggers, [trigger("t_replica", false)], "a replica trigger does not fire in a normal session, and RI triggers stay hidden");

    let version = h.engine.catalog(h.db()).await.unwrap().version;
    let map = h.engine.database_map_neighborhood(h.db(),oid("t").await,version,None).await.unwrap();
    assert_eq!(map.foreign_keys.len(),1,"referenced partitions do not duplicate declared keys");
    assert_eq!(map.foreign_keys[0].target,oid("ptarget").await);
    let incoming = h.engine.database_map_neighborhood(h.db(),oid("ptarget").await,version,None).await.unwrap();
    assert_eq!(incoming.foreign_keys.len(),1);

    let parted_1 = h.engine.relation_detail(h.db(), oid("parted_1").await).await.unwrap();
    assert_eq!(parted_1.triggers, [trigger("t_parent", true)], "a partition shows the row trigger it inherits");

    oracle.batch_execute(&format!("drop table public.{s}_t, public.{s}_ptarget, public.{s}_parted; drop function public.{s}_noop()")).await.unwrap();
}

/// What a connection with no stored view shows: the home database with every user schema, and
/// nothing else.
async fn assert_default_view(h: &Harness) {
    let shop = h.wait_database("shop", |d| matches!(d.catalog, CatalogStatus::Ready { .. })).await;
    assert!(shop.shown && !shop.show_system_schemas);
    assert_eq!(shop.shown_schemas, ["lab", "public"]);
    assert_eq!(shop.schema_count, SchemaCount::Known { count: 2 });
    assert_eq!(h.engine.catalog(h.db()).await.unwrap().default_schema.as_deref(), Some("public"));
    for name in ["postgres", "reporting"] {
        let d = h.database_state(name);
        assert!(!d.shown, "{name} is shown");
        assert_eq!((d.shown_schemas.len(), d.schema_count, d.catalog), (0, SchemaCount::Unknown, CatalogStatus::Idle), "{name}");
    }
    assert!(!h.instance().show_system_databases);
}

#[tokio::test]
async fn default_view_shows_home_with_all_its_schemas() {
    let h = Harness::start().await;
    h.engine.connect(h.conn).await.unwrap();
    assert_default_view(&h).await;
    let catalog = h.engine.catalog(h.db()).await.unwrap();
    let lab = catalog.schemas.iter().find(|s| s.name == "lab").unwrap();
    assert_eq!((lab.system, lab.table_count, lab.relations.len()), (false, 1, 1));
    let public = catalog.schemas.iter().find(|s| s.name == "public").unwrap();
    let tables = public
        .relations
        .iter()
        .filter(|r| !matches!(r.kind, RelKind::View | RelKind::MaterializedView))
        .count();
    assert_eq!(
        public.table_count as usize, tables,
        "views are relations but not tables"
    );
    assert!(
        public.relations.len() >= tables + 3,
        "the seed's three views are listed"
    );
    let mood = lab
        .types
        .iter()
        .find(|t| t.name == "mood")
        .expect("lab.mood");
    assert_eq!(
        mood.kind,
        TypeKind::Enum {
            labels: vec!["sad".into(), "ok".into(), "happy".into()]
        }
    );
    assert!(
        catalog.extensions.iter().any(|e| e.name == "plpgsql"),
        "a normal shop load lists extensions"
    );
    let system: Vec<(&str, bool, usize, usize, usize, usize, usize)> = catalog
        .schemas
        .iter()
        .filter(|s| s.system)
        .map(|s| {
            (
                s.name.as_str(),
                s.table_count > 0,
                s.relations.len(),
                s.functions.len(),
                s.sequences.len(),
                s.types.len(),
                s.synonyms.len(),
            )
        })
        .collect();
    assert_eq!(
        system,
        [
            ("information_schema", true, 0, 0, 0, 0, 0),
            ("pg_catalog", true, 0, 0, 0, 0, 0)
        ],
        "system schemas are listed with a count but not loaded"
    );
}

#[tokio::test]
async fn showing_a_database_opens_its_slot_and_shows_only_its_default_schema() {
    let h = Harness::start().await;
    h.engine.connect(h.conn).await.unwrap();
    h.edit_view(ViewEdit::Database { name: "reporting".into(), shown: true }).await;
    let reporting = h.wait_database("reporting", |d| matches!(d.catalog, CatalogStatus::Ready { .. })).await;
    assert!(reporting.shown);
    assert_eq!(reporting.shown_schemas, ["public"]);
    assert_eq!(reporting.schema_count, SchemaCount::Known { count: 2 });
    assert!(h.database_state("shop").shown, "the home database stays shown");

    let catalog = h.engine.catalog(h.db_named("reporting")).await.unwrap();
    assert_eq!(catalog.default_schema.as_deref(), Some("public"));
    let schemas: Vec<(&str, bool, u32, usize)> = catalog.schemas.iter().map(|s| (s.name.as_str(), s.system, s.table_count, s.relations.len())).collect();
    assert_eq!(schemas, [("audit", false, 1, 1), ("information_schema", true, 4, 0), ("pg_catalog", true, 64, 0), ("public", false, 1, 1)]);

    h.edit_view(ViewEdit::Database { name: "reporting".into(), shown: true }).await;
    assert_eq!(h.database_state("reporting").shown_schemas, ["public"], "the same edit twice changes nothing");
}

#[tokio::test]
async fn hiding_a_database_closes_its_slot_unless_a_tab_uses_it() {
    let h = Harness::start().await;
    h.engine.connect(h.conn).await.unwrap();
    let reporting = h.db_named("reporting");
    h.edit_view(ViewEdit::Database { name: "reporting".into(), shown: true }).await;
    h.wait_database("reporting", |d| matches!(d.catalog, CatalogStatus::Ready { .. })).await;

    h.edit_view(ViewEdit::Database { name: "reporting".into(), shown: false }).await;
    let hidden = h.wait_database("reporting", |d| d.catalog == CatalogStatus::Idle).await;
    assert!(!hidden.shown && hidden.shown_schemas.is_empty());
    assert!(matches!(h.engine.catalog(reporting.clone()).await, Err(CatalogError::NotShown { .. })));

    let tab = h.sql_tab_on(TxnMode::AutoCommit, "reporting").await;
    assert_eq!(h.engine.catalog(reporting.clone()).await.unwrap().db, reporting);
    h.edit_view(ViewEdit::Database { name: "reporting".into(), shown: false }).await;
    let kept = h.database_state("reporting");
    assert!(matches!(kept.catalog, CatalogStatus::Ready { .. }), "{:?}", kept.catalog);
    assert!(!kept.shown);
    assert_eq!(h.engine.catalog(reporting).await.unwrap().relation("public", "daily").unwrap().pk_columns(), ["day"]);
    h.engine.console_close(tab, false).await.unwrap();
    h.wait_database("reporting", |d| d.catalog == CatalogStatus::Idle).await;
}

#[tokio::test]
async fn probe_counts_schemas_of_a_database_without_a_slot() {
    let role = Role::create().await;
    let h = Harness::start_as(&role).await;
    h.engine.connect(h.conn).await.unwrap();
    h.engine.probe_databases(h.conn, vec!["postgres".into(), "template0".into(), "nope".into()]);
    let probing = |d: &DatabaseState| d.schema_count != SchemaCount::Unknown && d.schema_count != SchemaCount::Probing;
    let postgres = h.wait_database("postgres", probing).await;
    assert_eq!((postgres.schema_count, postgres.catalog), (SchemaCount::Known { count: 1 }, CatalogStatus::Idle));
    let template0 = h.wait_database("template0", probing).await;
    assert!(
        matches!(&template0.schema_count, SchemaCount::Unavailable { message } if message.contains("is not currently accepting connections")),
        "{:?}",
        template0.schema_count
    );
    let left = role.backends_when(|b| b.iter().all(|(db, _)| db == "shop")).await;
    assert_eq!(left.iter().map(|(db, _)| db.as_str()).collect::<Vec<_>>(), ["shop"], "the probe session is gone; the home metadata session stays");
    h.engine.disconnect(h.conn, false).await.unwrap();
    role.drop().await;
}

#[tokio::test]
async fn showing_pg_catalog_loads_its_relations_and_no_functions() {
    let h = Harness::start().await;
    h.engine.connect(h.conn).await.unwrap();
    let initial_catalog = h.catalog_when(&h.db(), |_| true).await;
    let initial = initial_catalog.version;
    assert!(
        initial_catalog
            .schemas
            .iter()
            .find(|s| s.name == "pg_catalog")
            .unwrap()
            .types
            .is_empty(),
        "a normal shop load does not read built-in types"
    );
    h.edit_view(ViewEdit::ShowSystemSchemas {
        database: "shop".into(),
        show: true,
    })
    .await;
    h.edit_view(ViewEdit::Schema {
        database: "shop".into(),
        name: "pg_catalog".into(),
        shown: true,
    })
    .await;
    let loaded = h
        .catalog_when(&h.db(), |c| {
            c.version > initial
                && c.schemas
                    .iter()
                    .any(|s| s.name == "pg_catalog" && !s.relations.is_empty())
        })
        .await;
    let pg_catalog = loaded
        .schemas
        .iter()
        .find(|s| s.name == "pg_catalog")
        .unwrap();
    assert!(
        pg_catalog.system
            && pg_catalog.relations.len() > 100
            && pg_catalog.functions.is_empty()
            && pg_catalog.sequences.is_empty()
            && pg_catalog.synonyms.is_empty(),
        "{} relations",
        pg_catalog.relations.len()
    );
    let int4 = pg_catalog
        .types
        .iter()
        .find(|t| t.name == "int4")
        .expect("pg_catalog.int4");
    assert_eq!(int4.kind, TypeKind::Builtin);
    let refused = h
        .engine
        .object_script(h.db(), ScriptTarget::Type { oid: int4.oid })
        .await
        .unwrap_err();
    assert!(
        matches!(refused, DetailError::Failed { message } if message == "Built-in types cannot be scripted.")
    );
    let tables = pg_catalog
        .relations
        .iter()
        .filter(|r| !matches!(r.kind, RelKind::View | RelKind::MaterializedView))
        .count();
    assert_eq!((pg_catalog.table_count, tables), (64, 64));
    let shop = h.wait_database("shop", |d| d.shown_schemas.iter().any(|s| s == "pg_catalog")).await;
    assert_eq!(shop.shown_schemas, ["lab", "pg_catalog", "public"]);
    assert!(shop.show_system_schemas);
    assert_eq!(shop.schema_count, SchemaCount::Known { count: 2 }, "the count stays non-system");

    h.edit_view(ViewEdit::ShowSystemSchemas { database: "shop".into(), show: false }).await;
    let off = h.wait_database("shop", |d| !d.show_system_schemas).await;
    assert_eq!(off.shown_schemas, ["lab", "public"]);
    h.catalog_when(&h.db(), |c| {
        c.schemas
            .iter()
            .any(|s| s.name == "pg_catalog" && s.relations.is_empty() && s.types.is_empty())
    })
    .await;

    h.edit_view(ViewEdit::ShowSystemSchemas { database: "shop".into(), show: true }).await;
    let on = h.wait_database("shop", |d| d.shown_schemas.iter().any(|s| s == "pg_catalog")).await;
    assert_eq!(on.shown_schemas, ["lab", "pg_catalog", "public"], "the checked schema was remembered while the toggle was off");
}

#[tokio::test]
async fn a_view_row_that_fails_to_parse_falls_back_to_the_initial_view() {
    let (dir, conn) = {
        let h = Harness::start_in(tempfile::tempdir().unwrap()).await;
        h.engine.connect(h.conn).await.unwrap();
        h.edit_view(ViewEdit::Database { name: "reporting".into(), shown: true }).await;
        h.wait_database("reporting", |d| d.shown).await;
        h.engine.disconnect(h.conn, false).await.unwrap();
        let Harness { dir, conn, .. } = h;
        (dir, conn)
    };
    let sqlite = rusqlite::Connection::open(dir.path().join("l1-data.sqlite3")).unwrap();
    let changed = sqlite.execute("UPDATE connection_views SET json = '{\"databases\": [' WHERE connection_id = ?1", [conn.0.to_string()]).unwrap();
    assert_eq!(changed, 1, "the edit above stored a row");

    let h = Harness::start_in(dir).await;
    assert_eq!(h.conn, conn);
    // A fresh engine has a fresh in-memory vault, so the saved password has to be sealed again.
    h.engine.save_connection(ConnectionDraft { id: Some(conn), ..draft_from_url(&url(), "dev").unwrap() }).await.unwrap();
    h.engine.connect(h.conn).await.unwrap();
    assert_default_view(&h).await;
}

#[tokio::test]
async fn sequences_types_and_extensions_load_and_script() {
    let mut h = Harness::start().await;
    h.engine.connect(h.conn).await.unwrap();
    let tab = h.sql_tab(TxnMode::AutoCommit).await;
    let stem = unique("p10");
    let seq = format!("{stem}_seq");
    let domain = format!("{stem}_pos");
    let composite = format!("{stem}_pet");
    let range = format!("{stem}_span");
    let en = format!("{stem}_mood");
    h.run_ok(
        tab,
        &format!(
            "CREATE SEQUENCE public.{seq} AS bigint INCREMENT BY 2;
             CREATE DOMAIN public.{domain} AS integer CONSTRAINT {domain}_positive CHECK (VALUE > 0);
             CREATE TYPE public.{composite} AS (id integer, name text);
             CREATE TYPE public.{range} AS RANGE (SUBTYPE = integer);
             CREATE TYPE public.{en} AS ENUM ('sad', 'ok', 'happy')"
        ),
    )
    .await;
    h.engine.refresh_catalog(h.db()).await.unwrap();
    let catalog = h.engine.catalog(h.db()).await.unwrap();
    let public = catalog.schemas.iter().find(|s| s.name == "public").unwrap();
    let sequence = public
        .sequences
        .iter()
        .find(|s| s.name == seq)
        .expect("sequence");
    assert_eq!(sequence.type_name, "bigint");
    let kind = |name: &str| {
        public
            .types
            .iter()
            .find(|t| t.name == name)
            .unwrap_or_else(|| panic!("{name} missing"))
            .kind
            .clone()
    };
    assert_eq!(
        kind(&domain),
        TypeKind::Domain {
            base: "integer".into()
        }
    );
    assert_eq!(
        kind(&composite),
        TypeKind::Composite {
            fields: vec![
                TypeField {
                    name: "id".into(),
                    type_name: "integer".into()
                },
                TypeField {
                    name: "name".into(),
                    type_name: "text".into()
                }
            ]
        }
    );
    assert_eq!(
        kind(&range),
        TypeKind::Range {
            subtype: "integer".into()
        }
    );
    assert_eq!(
        kind(&en),
        TypeKind::Enum {
            labels: vec!["sad".into(), "ok".into(), "happy".into()]
        }
    );
    assert!(
        catalog
            .schemas
            .iter()
            .filter(|s| s.system)
            .all(|s| s.types.is_empty() && s.sequences.is_empty() && s.synonyms.is_empty())
    );
    let seq_sql = h
        .engine
        .object_script(h.db(), ScriptTarget::Sequence { oid: sequence.oid })
        .await
        .unwrap()
        .text;
    assert!(
        seq_sql.contains(&format!("CREATE SEQUENCE public.{seq} AS bigint"))
            && seq_sql.contains("INCREMENT BY 2"),
        "{seq_sql}"
    );
    let enum_oid = public.types.iter().find(|t| t.name == en).unwrap().oid;
    let enum_sql = h
        .engine
        .object_script(h.db(), ScriptTarget::Type { oid: enum_oid })
        .await
        .unwrap()
        .text;
    assert!(
        enum_sql.contains("CREATE TYPE")
            && enum_sql.contains("'sad'")
            && enum_sql.contains("'ok'")
            && enum_sql.contains("'happy'"),
        "{enum_sql}"
    );
    let plpgsql = catalog
        .extensions
        .iter()
        .find(|e| e.name == "plpgsql")
        .expect("plpgsql");
    let ext_sql = h
        .engine
        .object_script(h.db(), ScriptTarget::Extension { oid: plpgsql.oid })
        .await
        .unwrap()
        .text;
    assert!(
        ext_sql.contains("CREATE EXTENSION plpgsql") && ext_sql.contains("VERSION"),
        "{ext_sql}"
    );
    let refused = h
        .engine
        .object_script(h.db(), ScriptTarget::Synonym { oid: 1 })
        .await
        .unwrap_err();
    assert!(
        matches!(refused, DetailError::Failed { message } if message == "PostgreSQL has no synonyms.")
    );
    h.run_ok(
        tab,
        &format!("DROP SEQUENCE public.{seq}; DROP DOMAIN public.{domain}; DROP TYPE public.{composite}; DROP TYPE public.{range}; DROP TYPE public.{en}"),
    )
    .await;
}

/// A role whose search path starts with `pg_catalog` still gets a user schema as its default:
/// "Default only" never shows a schema the picker hides.
#[tokio::test]
async fn a_search_path_starting_with_pg_catalog_defaults_to_a_user_schema() {
    let role = Role::create().await;
    role.oracle.batch_execute(&format!("alter role {} set search_path = pg_catalog, public", role.name)).await.unwrap();
    let mut h = Harness::start_as(&role).await;
    h.engine.connect(h.conn).await.unwrap();
    let tab = h.sql_tab(TxnMode::AutoCommit).await;
    assert_eq!(h.scalar(tab, "show search_path").await, "pg_catalog, public");
    let catalog = h.catalog_when(&h.db(), |_| true).await;
    assert_eq!(catalog.default_schema.as_deref(), Some("public"));
    h.engine.disconnect(h.conn, false).await.unwrap();
    role.drop().await;
}

/// The probe cache is a cache of the database list: a DDL re-read of `pg_database` drops it, so
/// a database dropped and recreated under its old name is counted again.
#[tokio::test]
async fn a_database_list_reread_drops_the_probe_cache() {
    let mut h = Harness::start().await;
    h.engine.connect(h.conn).await.unwrap();
    h.engine.probe_databases(h.conn, vec!["postgres".into()]);
    h.wait_database("postgres", |d| matches!(d.schema_count, SchemaCount::Known { .. })).await;
    let tab = h.sql_tab(TxnMode::AutoCommit).await;
    let name = unique("t_db");
    h.run_ok(tab, &format!("create database {name}")).await;
    h.wait_database_names(|names| names.contains(&name)).await;
    assert_eq!(h.database_state("postgres").schema_count, SchemaCount::Unknown, "the count went with the list it counted");
    h.run_ok(tab, &format!("drop database {name}")).await;
    h.wait_database_names(|names| !names.contains(&name)).await;
}

/// Showing a database whose stored selection is empty resets it to its default schema once the
/// catalog is known, unless the user chose the schemas in the meantime.
#[tokio::test]
async fn a_schema_choice_made_while_the_catalog_loads_is_not_reset() {
    let h = Harness::start().await;
    h.engine.connect(h.conn).await.unwrap();
    h.edit_view(ViewEdit::Schemas { database: "reporting".into(), preset: Preset::None }).await;
    h.edit_view(ViewEdit::Database { name: "reporting".into(), shown: true }).await;
    h.edit_view(ViewEdit::Schemas { database: "reporting".into(), preset: Preset::None }).await;
    let reporting = h.wait_database("reporting", |d| matches!(d.catalog, CatalogStatus::Ready { .. })).await;
    assert!(reporting.shown_schemas.is_empty(), "None was chosen on purpose: {:?}", reporting.shown_schemas);

    h.edit_view(ViewEdit::Database { name: "reporting".into(), shown: false }).await;
    h.wait_database("reporting", |d| d.catalog == CatalogStatus::Idle).await;
    h.edit_view(ViewEdit::Database { name: "reporting".into(), shown: true }).await;
    let reporting = h.wait_database("reporting", |d| matches!(d.catalog, CatalogStatus::Ready { .. }) && !d.shown_schemas.is_empty()).await;
    assert_eq!(reporting.shown_schemas, ["public"], "shown again with nothing else said, the empty selection resets to the default");
}

/// Literals whose value depends on the lexer agreeing with Postgres's `scan.l`. A tag, a continued
/// string, or a number the lexer misreads lets the formatter change the value or break the query.
const AWKWARD_LITERALS: &[&str] = &[
    "$é$select from t where x$é$",
    "$ü$line one\n    indented  line$ü$",
    "$a$ $b$ $b$ $a$",
    "'a;b' || $é$ ; $é$",
    "E'it\\'s'\n'\\'  two  spaces'",
    "E'a\\\\'\n'b\\'c'",
    "'one'\n'  two  '",
    "'one' -- between the parts\n  'two'",
    "'one'\r'two'",
    "X'1F'\n'F'",
    "U&'d!0061t' UESCAPE '!'",
    "U&'d!0061t'\n'  !0061' UESCAPE '!'",
    "1_000.5",
    "1_000.25e-3",
    "0x1F + 0b1010 + 0o17",
    "1. + .5",
];

const BACKSLASH_LITERALS: &[&str] = &["'a\\'b'", "n'a\\'b'", "'C:\\path'", "'tab\\there'", "'x' -- c\n'y\\'z'"];

async fn text_rows(client: &tokio_postgres::Client, sql: &str) -> Vec<Vec<Option<String>>> {
    let messages = client.simple_query(sql).await.unwrap_or_else(|e| panic!("{sql:?}: {}", e.as_db_error().map_or(e.to_string(), |d| d.message().to_owned())));
    messages
        .iter()
        .filter_map(|m| match m {
            tokio_postgres::SimpleQueryMessage::Row(r) => Some((0..r.len()).map(|i| r.get(i).map(str::to_owned)).collect()),
            _ => None,
        })
        .collect()
}

fn apply(text: &str, edit: &FormatEdit) -> String {
    let units: Vec<u16> = text.encode_utf16().collect();
    let (from, to) = (edit.from.0 as usize, edit.to.0 as usize);
    String::from_utf16(&units[..from]).unwrap() + &edit.insert + &String::from_utf16(&units[to..]).unwrap()
}

#[tokio::test]
async fn formatting_keeps_the_value_of_every_awkward_literal() {
    let h = Harness::start().await;
    let oracle = h.oracle().await;
    for literal in AWKWARD_LITERALS {
        let sql = format!("select {literal} as v");
        let formatted = match h.engine.editor_format(&sql, RunTarget::All, DatabaseKind::Postgres) {
            Ok(edit) => apply(&sql, &edit),
            Err(FormatError::Unsafe) => continue,
            Err(e) => panic!("{sql:?}: {e:?}"),
        };
        assert_eq!(text_rows(&oracle, &formatted).await, text_rows(&oracle, &sql).await, "{sql:?} formatted as {formatted:?}");
    }
}

#[tokio::test]
async fn formatting_keeps_the_value_of_a_backslash_literal_with_standard_strings_off() {
    let h = Harness::start().await;
    let oracle = h.oracle().await;
    oracle.batch_execute("set standard_conforming_strings = off; set escape_string_warning = off").await.unwrap();
    for literal in BACKSLASH_LITERALS {
        let sql = format!("select {literal} as v");
        let formatted = match h.engine.editor_format(&sql, RunTarget::All, DatabaseKind::Postgres) {
            Ok(edit) => apply(&sql, &edit),
            Err(FormatError::Unsafe) => continue,
            Err(e) => panic!("{sql:?}: {e:?}"),
        };
        assert_eq!(text_rows(&oracle, &formatted).await, text_rows(&oracle, &sql).await, "{sql:?} formatted as {formatted:?}");
    }
}

async fn next_update(h: &mut Harness, done: impl Fn(&UpdateStatus) -> bool) {
    loop {
        if let AppEvent::UpdateChanged(state) = h.next_event().await
            && done(&state.status)
        {
            return;
        }
    }
}

async fn downloaded_update() -> (Harness, Arc<ScriptedTransport>) {
    let transport = Arc::new(ScriptedTransport::named("available").unwrap());
    let mut h = Harness::start_updating(tempfile::tempdir().unwrap(), draft_from_url(&url(), "dev").unwrap(), transport.clone()).await;
    h.engine.update_check();
    next_update(&mut h, |s| matches!(s, UpdateStatus::Available { .. })).await;
    h.engine.update_download("0.2.0".into()).unwrap();
    next_update(&mut h, |s| matches!(s, UpdateStatus::Downloaded { .. })).await;
    (h, transport)
}

async fn confirm(h: &Harness, accepted: Option<RestartImpact>) -> RestartImpact {
    match h.engine.update_install("0.2.0".into(), accepted).await {
        Ok(InstallReply::Confirm { impact }) => impact,
        other => panic!("expected a confirm, got {other:?}"),
    }
}

#[tokio::test]
async fn an_install_asks_again_when_a_transaction_opens_or_gains_writes() {
    let (mut h, transport) = downloaded_update().await;
    let (a, b) = (h.sql_tab(TxnMode::Manual).await, h.sql_tab(TxnMode::Manual).await);
    h.run_ok(a, "select 1").await;
    let shown = confirm(&h, None).await;
    assert_eq!(shown, RestartImpact { running: vec![], transactions: vec![OpenTxn { tab_id: a, has_writes: false }], chats: vec![] });

    h.run_ok(b, "create temp table restart_b (id int)").await;
    let shown = confirm(&h, Some(shown)).await;
    let mut both = vec![OpenTxn { tab_id: a, has_writes: false }, OpenTxn { tab_id: b, has_writes: true }];
    both.sort_by_key(|t| t.tab_id.0);
    assert_eq!(shown.transactions, both, "a transaction opened since the confirm");

    h.run_ok(a, "create temp table restart_a (id int)").await;
    let shown = confirm(&h, Some(shown)).await;
    assert!(shown.transactions.iter().all(|t| t.has_writes), "a transaction gained writes since the confirm: {shown:?}");

    assert!(matches!(h.engine.update_install("0.2.0".into(), Some(shown)).await, Ok(InstallReply::Restarting)));
    let started = Instant::now();
    while !transport.relaunched() {
        assert!(started.elapsed() < Duration::from_secs(10), "no relaunch");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn a_running_query_is_in_the_impact_and_a_has_writes_probe_is_not() {
    let (mut h, _transport) = downloaded_update().await;
    let busy = h.sql_tab(TxnMode::AutoCommit).await;
    let idle = h.sql_tab(TxnMode::Manual).await;
    h.run_ok(idle, "select 1").await;
    let accepted = h.start_run(busy, "select pg_sleep(30)").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let (first, second) = tokio::join!(confirm(&h, None), confirm(&h, None));
    for impact in [&first, &second] {
        assert_eq!(impact.running, vec![busy], "{impact:?}");
        assert_eq!(impact.transactions.iter().map(|t| t.tab_id).collect::<Vec<_>>(), vec![idle], "{impact:?}");
    }
    h.engine.console_cancel(busy).await;
    h.wait_finished(busy, accepted.run_id).await;
}

/// Forwards to the test server until `freeze`, then holds every byte, as a stalled network does.
async fn freezable_relay(upstream: (String, u16)) -> (u16, Arc<std::sync::atomic::AtomicBool>) {
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    async fn pump(mut from: tokio::net::tcp::OwnedReadHalf, mut to: tokio::net::tcp::OwnedWriteHalf, frozen: Arc<AtomicBool>) {
        let mut buf = vec![0u8; 16 * 1024];
        while let Ok(n @ 1..) = from.read(&mut buf).await {
            while frozen.load(Ordering::Relaxed) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            if to.write_all(&buf[..n]).await.is_err() {
                return;
            }
        }
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let frozen = Arc::new(AtomicBool::new(false));
    let relay_frozen = frozen.clone();
    tokio::spawn(async move {
        while let Ok((client, _)) = listener.accept().await {
            let server = tokio::net::TcpStream::connect((upstream.0.as_str(), upstream.1)).await.unwrap();
            let ((client_read, client_write), (server_read, server_write)) = (client.into_split(), server.into_split());
            tokio::spawn(pump(client_read, server_write, relay_frozen.clone()));
            tokio::spawn(pump(server_read, client_write, relay_frozen.clone()));
        }
    });
    (port, frozen)
}

#[tokio::test]
async fn a_stalled_session_does_not_hang_the_restart_or_disconnect_check() {
    let real = draft_from_url(&url(), "dev").unwrap();
    let (port, frozen) = freezable_relay((real.host.clone(), real.port)).await;
    let draft = ConnectionDraft { host: "127.0.0.1".into(), port, ..real };
    let transport = Arc::new(ScriptedTransport::named("available").unwrap());
    let mut h = Harness::start_updating(tempfile::tempdir().unwrap(), draft, transport).await;
    h.engine.update_check();
    next_update(&mut h, |s| matches!(s, UpdateStatus::Available { .. })).await;
    h.engine.update_download("0.2.0".into()).unwrap();
    next_update(&mut h, |s| matches!(s, UpdateStatus::Downloaded { .. })).await;
    let (a, b) = (h.sql_tab(TxnMode::Manual).await, h.sql_tab(TxnMode::Manual).await);
    h.run_ok(a, "select 1").await;
    h.run_ok(b, "select 1").await;
    let mut both = vec![OpenTxn { tab_id: a, has_writes: true }, OpenTxn { tab_id: b, has_writes: true }];
    both.sort_by_key(|t| t.tab_id.0);
    frozen.store(true, std::sync::atomic::Ordering::Relaxed);

    let started = Instant::now();
    let impact = tokio::time::timeout(Duration::from_secs(10), confirm(&h, None)).await.expect("the restart check waited on a stalled session");
    assert!(started.elapsed() < Duration::from_secs(5), "restart check took {:?}", started.elapsed());
    assert_eq!(impact.transactions, both, "an unanswered probe counts as writes");

    let started = Instant::now();
    let err = tokio::time::timeout(Duration::from_secs(10), h.engine.disconnect(h.conn, false)).await.expect("disconnect waited on a stalled session").unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(5), "disconnect check took {:?}", started.elapsed());
    let DisconnectError::OpenTransactions { mut tabs } = err else { panic!("{err:?}") };
    tabs.sort_by_key(|t| t.tab_id.0);
    assert_eq!(tabs, both);
}

async fn set_environment(h: &Harness, environment: Option<Environment>) {
    let draft = ConnectionDraft { id: Some(h.conn), password: PasswordInput::Keep, environment, ..draft_from_url(&url(), "dev").unwrap() };
    h.engine.save_connection(draft).await.unwrap();
}

fn script(text: &str) -> RunRequest {
    RunRequest::Script { text: text.into(), target: RunTarget::All, mode: RunMode::Run, params: Vec::new() }
}

async fn asked_to_run(h: &Harness, tab: TabId, request: RunRequest, confirmed: Option<&WriteDigest>) -> WriteConfirmation {
    match h.engine.console_run(tab, request, confirmed.cloned()).await {
        Err(RunError::ConfirmWrites { confirmation }) => confirmation,
        other => panic!("expected ConfirmWrites, got {other:?}"),
    }
}

async fn count(oracle: &tokio_postgres::Client, table: &str) -> i64 {
    oracle.query_one(&format!("select count(*) from public.{table}"), &[]).await.unwrap().get(0)
}

#[tokio::test]
async fn unicode_escaped_cte_write_requires_production_approval() {
    let mut h = Harness::start().await;
    let oracle = h.oracle().await;
    let table = unique("t_unicode_guard");
    oracle.batch_execute(&format!("create table public.{table} (n int); insert into public.{table} values (7)")).await.unwrap();
    set_environment(&h, Some(Environment::Production)).await;
    let queries = [
        format!(r#"WITH U&"changed" AS (DELETE FROM public.{table} RETURNING *) SELECT * FROM changed"#),
        format!(r#"WITH U&"ch\0061nged" (n) AS (DELETE FROM public.{table} RETURNING *) SELECT * FROM changed"#),
        format!(r#"WITH U&"ch!0061nged" UESCAPE '!' AS (DELETE FROM public.{table} RETURNING *) SELECT * FROM changed"#),
        format!("WITH U&\"changed\" UESCAPE '!'\n'' AS (DELETE FROM public.{table} RETURNING *) SELECT * FROM changed"),
        format!(r#"WITH RECURSIVE seed AS (SELECT 1), U&"ch!0061nged" UESCAPE E'!' (n) AS (DELETE FROM public.{table} RETURNING *) SELECT * FROM changed"#),
        format!(r#"WITH U&"source" AS (SELECT 1), later AS (SELECT * FROM source) DELETE FROM public.{table}"#),
        format!("WITH changed AS (DELETE FROM public.{table} RETURNING *) SELECT * FROM changed"),
        format!(r#"COPY (WITH U&"source" UESCAPE '!' (n) AS (SELECT 7) DELETE FROM public.{table} RETURNING *) TO PROGRAM 'cat > /dev/null'"#),
        format!("COPY (WITH source AS (SELECT 7) DELETE FROM public.{table} RETURNING *) TO PROGRAM 'cat > /dev/null'"),
    ];
    for sql in queries {
        let tab = h.sql_tab(TxnMode::AutoCommit).await;
        let history = h.history_len();
        let asked = asked_to_run(&h, tab, script(&sql), None).await;
        assert_eq!((asked.sends.count, asked.sends.statements.as_slice()), (1, [sql.clone()].as_slice()));
        assert_eq!(count(&oracle, &table).await, 1, "unconfirmed SQL must not delete rows");
        assert_eq!(h.history_len(), history, "unconfirmed SQL records no history");
        assert!(matches!(h.console(tab).run, RunState::Idle), "unconfirmed SQL starts no run");

        let changed = sql.replace("DELETE FROM", "DELETE /* changed */ FROM");
        let stale = asked_to_run(&h, tab, script(&changed), Some(&asked.digest)).await;
        assert_ne!(stale.digest, asked.digest);
        assert_eq!(count(&oracle, &table).await, 1, "stale approval must not delete rows");
        assert_eq!(h.history_len(), history);
        assert!(matches!(h.console(tab).run, RunState::Idle));

        let accepted = h.engine.console_run(tab, script(&sql), Some(asked.digest)).await.unwrap();
        let state = h.wait_finished(tab, accepted.run_id).await;
        assert!(matches!(state.run, RunState::Finished { outcome: RunOutcome::Ok, .. }), "{sql}: {:?}", state.run);
        assert_eq!(count(&oracle, &table).await, 0, "exact confirmed SQL deletes the row");
        assert_eq!(h.history_len(), history + 1);
        oracle.batch_execute(&format!("insert into public.{table} values (7)")).await.unwrap();
    }
    let tab = h.sql_tab(TxnMode::AutoCommit).await;
    for sql in [
        r#"WITH U&"DELETE" AS (SELECT 'UPDATE') SELECT * FROM "DELETE""#,
        r#"WITH U&"UPDATE" UESCAPE '!' ("DELETE") AS (SELECT $$INSERT INTO orders$$) SELECT * FROM "UPDATE""#,
    ] {
        h.run_ok(tab, sql).await;
        h.run_ok(tab, &format!("COPY ({sql}) TO PROGRAM 'cat > /dev/null'")).await;
    }
    oracle.batch_execute(&format!("drop table public.{table}")).await.unwrap();
}

#[tokio::test]
async fn copy_from_on_production_requires_the_exact_statement_digest() {
    let mut h = Harness::start().await;
    let oracle = h.oracle().await;
    let table = unique("t_copy_guard");
    oracle.batch_execute(&format!("create table public.{table} (n int)")).await.unwrap();
    set_environment(&h, Some(Environment::Production)).await;
    let tab = h.sql_tab(TxnMode::AutoCommit).await;
    let history = h.history_len();
    let copy = format!("COPY public.{table} (n) FROM PROGRAM 'printf 7'");

    let asked = match h.engine.console_run(tab, script(&copy), None).await {
        Err(RunError::ConfirmWrites { confirmation }) => confirmation,
        other => {
            if let Ok(accepted) = &other {
                h.wait_finished(tab, accepted.run_id).await;
            }
            let rows = count(&oracle, &table).await;
            oracle.batch_execute(&format!("drop table public.{table}")).await.unwrap();
            panic!("expected ConfirmWrites before COPY, got {other:?}; stored rows: {rows}");
        }
    };
    assert_eq!((asked.sends.count, asked.sends.statements.as_slice()), (1, [copy.clone()].as_slice()));
    assert_eq!(count(&oracle, &table).await, 0, "declining the question sends no COPY");
    assert_eq!(h.history_len(), history, "declining the question records no run");
    assert!(matches!(h.console(tab).run, RunState::Idle));

    let changed = format!("COPY public.{table} (n) FROM PROGRAM 'printf 8'");
    let stale = asked_to_run(&h, tab, script(&changed), Some(&asked.digest)).await;
    assert_ne!(stale.digest, asked.digest);
    assert_eq!(count(&oracle, &table).await, 0, "approval for other SQL sends no COPY");

    let accepted = h.engine.console_run(tab, script(&copy), Some(asked.digest)).await.unwrap();
    let state = h.wait_finished(tab, accepted.run_id).await;
    assert!(matches!(state.run, RunState::Finished { outcome: RunOutcome::Ok, .. }), "{:?}", state.run);
    assert_eq!(h.scalar(tab, &format!("select n from public.{table}")).await, "7");
    assert_eq!(count(&oracle, &table).await, 1);

    for sql in [format!("copy public.{table} from stdin"), format!("copy public.{table} to stdout")] {
        assert!(matches!(h.engine.console_run(tab, script(&sql), None).await, Err(RunError::Unsupported { .. })), "{sql}");
    }
    assert_eq!(h.scalar(tab, "select 1").await, "1");
    oracle.batch_execute(&format!("drop table public.{table}")).await.unwrap();
}

#[tokio::test]
async fn a_write_on_production_runs_only_with_the_digest_of_the_same_work_and_target() {
    let mut h = Harness::start().await;
    let oracle = h.oracle().await;
    let table = unique("t_guard");
    oracle.batch_execute(&format!("create table public.{table} (n int)")).await.unwrap();
    set_environment(&h, Some(Environment::Production)).await;
    let tab = h.sql_tab(TxnMode::AutoCommit).await;
    let history = h.history_len();

    let one = format!("insert into public.{table} values (1)");
    let asked = asked_to_run(&h, tab, script(&format!("select 1;\n{one};")), None).await;
    assert_eq!((asked.connection.as_str(), asked.database.as_str(), asked.sends.count), ("dev", "shop", 1));
    assert_eq!(asked.sends.statements, [one.as_str()]);
    assert_eq!(count(&oracle, &table).await, 0, "a refused run sends nothing");
    assert!(matches!(h.console(tab).run, RunState::Idle), "{:?}", h.console(tab).run);

    let stale = asked_to_run(&h, tab, script(&format!("select 1;\ninsert into public.{table} values (2);")), Some(&asked.digest)).await;
    assert_ne!(stale.digest, asked.digest);
    assert_eq!(count(&oracle, &table).await, 0, "a digest for other text sends nothing");

    let manual = h.sql_tab(TxnMode::Manual).await;
    asked_to_run(&h, manual, script(&format!("select 1;\n{one};")), Some(&asked.digest)).await;

    let accepted = h.engine.console_run(tab, script(&format!("select 1;\n{one};")), Some(asked.digest.clone())).await.unwrap();
    let state = h.wait_finished(tab, accepted.run_id).await;
    assert!(matches!(state.run, RunState::Finished { outcome: RunOutcome::Ok, .. }), "{:?}", state.run);
    assert_eq!(count(&oracle, &table).await, 1);
    assert_eq!(h.history_len(), history + 2, "only the run that ran is in history");

    h.run_ok(tab, &format!("select count(*) from public.{table}")).await;

    let selected = format!("select 1;\n{one};\ninsert into public.{table} values (3);");
    let from = selected.find("insert").unwrap() as u32;
    let to = from + one.len() as u32 + 1;
    let selection = RunRequest::Script { text: selected.clone(), target: RunTarget::Selection { from: DocOffset(from), to: DocOffset(to) }, mode: RunMode::Run, params: Vec::new() };
    let asked = asked_to_run(&h, tab, selection, None).await;
    assert_eq!((asked.sends.count, asked.sends.statements.as_slice()), (1, [one.clone()].as_slice()));

    asked_to_run(&h, tab, script(&one), None).await;
    set_environment(&h, Some(Environment::Staging)).await;
    asked_to_run(&h, tab, script(&one), None).await;
    let staging = h.sql_tab(TxnMode::AutoCommit).await;
    h.run_ok(staging, &one).await;
    set_environment(&h, None).await;
    let unset = h.sql_tab(TxnMode::AutoCommit).await;
    h.run_ok(unset, &one).await;
    assert_eq!(count(&oracle, &table).await, 3, "a session opened on Staging or unset writes without a digest");

    oracle.batch_execute(&format!("drop table public.{table}")).await.unwrap();
}

#[tokio::test]
async fn a_digest_holds_only_while_the_connection_stays_as_asked() {
    let h = Harness::start().await;
    let oracle = h.oracle().await;
    let table = unique("t_guard");
    oracle.batch_execute(&format!("create table public.{table} (n int)")).await.unwrap();
    set_environment(&h, Some(Environment::Production)).await;
    let tab = h.sql_tab(TxnMode::AutoCommit).await;
    let one = format!("insert into public.{table} values (1)");
    let asked = asked_to_run(&h, tab, script(&one), None).await;

    let renamed = ConnectionDraft {
        id: Some(h.conn),
        name: "dev renamed".into(),
        password: PasswordInput::Keep,
        environment: Some(Environment::Production),
        ..draft_from_url(&url(), "dev").unwrap()
    };
    h.engine.save_connection(renamed).await.unwrap();
    let again = asked_to_run(&h, tab, script(&one), Some(&asked.digest)).await;
    assert_eq!(again.connection, "dev renamed");
    assert_eq!(count(&oracle, &table).await, 0);
    oracle.batch_execute(&format!("drop table public.{table}")).await.unwrap();
}

#[tokio::test]
async fn staged_edits_on_production_apply_only_with_the_digest_of_the_same_edits() {
    let mut h = Harness::start().await;
    h.engine.connect(h.conn).await.unwrap();
    let tab = h.sql_tab(TxnMode::AutoCommit).await;
    let table = edit_table(&mut h, tab).await;
    set_environment(&h, Some(Environment::Production)).await;
    let reader = h.sql_tab(TxnMode::AutoCommit).await;

    let mut edits = changes(&table);
    edits.updates.push(RowUpdate { key: vec![cell("id", Some("1"))], set: vec![cell("note", Some("x"))] });
    edits.inserts.push(vec![cell("name", Some("d"))]);
    let asked = match h.engine.table_apply(tab, edits.clone(), None).await {
        Err(ApplyError::ConfirmWrites { confirmation }) => confirmation,
        other => panic!("expected ConfirmWrites, got {other:?}"),
    };
    assert_eq!((asked.sends.count, asked.holds.count), (2, 2), "the setup's auto-commit writes stay held, because no COMMIT ended them");
    assert_eq!(
        asked.sends.statements,
        [format!("UPDATE \"public\".\"{table}\" SET \"note\" = 'x' WHERE \"id\" = '1'"), format!("INSERT INTO \"public\".\"{table}\" (\"name\") VALUES ('d')")]
    );
    assert_eq!(rows_of(&mut h, reader, &table).await, "1:a:NULL 2:b:NULL 3:c:NULL");

    let mut other = edits.clone();
    other.inserts[0] = vec![cell("name", Some("e"))];
    assert!(matches!(h.engine.table_apply(tab, other, Some(asked.digest.clone())).await, Err(ApplyError::ConfirmWrites { .. })));
    assert_eq!(rows_of(&mut h, reader, &table).await, "1:a:NULL 2:b:NULL 3:c:NULL", "a digest for other edits applies nothing");

    assert!(h.engine.table_apply(tab, edits, Some(asked.digest)).await.unwrap().committed);
    assert_eq!(rows_of(&mut h, tab, &table).await, "1:a:x 2:b:NULL 3:c:NULL 4:d:NULL");
    set_environment(&h, None).await;
    h.run_ok(tab, &format!("drop table public.{table}")).await;
}

#[tokio::test]
async fn a_session_opened_on_production_still_asks_after_the_connection_moves_elsewhere() {
    let mut h = Harness::start().await;
    let oracle = h.oracle().await;
    let table = unique("t_guard");
    oracle.batch_execute(&format!("create table public.{table} (n int primary key)")).await.unwrap();
    set_environment(&h, Some(Environment::Production)).await;
    let tab = h.sql_tab(TxnMode::AutoCommit).await;
    h.run_ok(tab, "select 1").await;
    h.engine.refresh_catalog(h.db()).await.unwrap();

    let page = RunRequest::TablePage { schema: "public".into(), table: table.clone(), page: 0, query: TableQuery::default() };
    let accepted = h.engine.console_run(tab, page, None).await.expect("a table page reads, so it never asks");
    h.wait_finished(tab, accepted.run_id).await;

    let moved = ConnectionDraft {
        id: Some(h.conn),
        host: "moved.invalid".into(),
        password: PasswordInput::Keep,
        environment: Some(Environment::Local),
        ..draft_from_url(&url(), "dev").unwrap()
    };
    h.engine.save_connection(moved).await.unwrap();
    let asked = asked_to_run(&h, tab, script(&format!("insert into public.{table} values (1)")), None).await;
    assert_eq!(asked.sends.count, 1);
    assert_eq!(count(&oracle, &table).await, 0, "the live session still points where it was opened as Production");
    oracle.batch_execute(&format!("drop table public.{table}")).await.unwrap();
}

async fn resave(h: &Harness, host: &str, environment: Option<Environment>) {
    let saved = h.engine.boot().connections.into_iter().find(|c| c.id == h.conn).unwrap();
    let draft = ConnectionDraft {
        database_kind: saved.database_kind,
        id: Some(saved.id),
        name: saved.name,
        host: host.into(),
        port: saved.port,
        database: saved.database,
        user: saved.user,
        ssl_mode: saved.ssl_mode,
        ca_cert_path: saved.ca_cert_path,
        password: PasswordInput::Keep,
        color: saved.color,
        ssh: None,
        environment,
        sql_server_auth: saved.sql_server_auth,
    };
    h.engine.save_connection(draft).await.unwrap();
}

fn saved_host(h: &Harness) -> String {
    h.engine.boot().connections.into_iter().find(|c| c.id == h.conn).unwrap().host
}

#[tokio::test]
async fn a_session_opened_on_production_asks_after_the_connection_moves_away_and_back_as_local() {
    let mut h = Harness::start().await;
    let oracle = h.oracle().await;
    let table = unique("t_guard");
    oracle.batch_execute(&format!("create table public.{table} (n int)")).await.unwrap();
    let home = saved_host(&h);
    resave(&h, &home, Some(Environment::Production)).await;
    let tab = h.sql_tab(TxnMode::AutoCommit).await;
    h.run_ok(tab, "select 1").await;

    resave(&h, "moved.invalid", Some(Environment::Local)).await;
    h.run_ok(tab, "select 1").await;
    resave(&h, &home, Some(Environment::Local)).await;

    let asked = asked_to_run(&h, tab, script(&format!("insert into public.{table} values (1)")), None).await;
    assert_eq!(asked.sends.count, 1);
    assert_eq!(count(&oracle, &table).await, 0, "the tab's session was opened as Production");
    oracle.batch_execute(&format!("drop table public.{table}")).await.unwrap();
}

#[tokio::test]
async fn a_session_opened_on_production_asks_after_the_instance_reconnects_as_local() {
    let role = Role::create().await;
    let mut h = Harness::start_as(&role).await;
    let table = unique("t_guard");
    role.oracle.batch_execute(&format!("create table public.{table} (n int); grant select, insert on public.{table} to {}", role.name)).await.unwrap();
    let home = saved_host(&h);
    resave(&h, &home, Some(Environment::Production)).await;
    let tab = h.sql_tab(TxnMode::AutoCommit).await;
    let state = h.run_ok(tab, "select 1").await;
    let SessionState::Open { backend_pid: console_pid } = state.session else { panic!("{:?}", state.session) };

    resave(&h, &home, Some(Environment::Local)).await;
    let metadata: Vec<i32> = role.backends().await.into_iter().filter(|(_, pid)| *pid != console_pid).map(|(_, pid)| pid).collect();
    let [home_pid] = metadata[..] else { panic!("one home metadata session, found {metadata:?}") };
    role.oracle.simple_query(&format!("select pg_terminate_backend({home_pid})")).await.unwrap();
    h.wait_status(|s| !matches!(s, ConnStatus::Connected { .. })).await;

    let asked = asked_to_run(&h, tab, script(&format!("insert into public.{table} values (1)")), None).await;
    assert_eq!(asked.sends.count, 1);
    assert!(matches!(h.status(), ConnStatus::Connected { .. }), "the run reconnected the instance from the Local config");
    assert!(matches!(h.console(tab).session, SessionState::Open { backend_pid } if backend_pid == console_pid), "the tab kept the session it opened as Production");
    assert_eq!(count(&role.oracle, &table).await, 0);
    role.oracle.batch_execute(&format!("drop table public.{table}")).await.unwrap();
    role.drop().await;
}

#[tokio::test]
async fn commit_on_production_asks_for_writes_that_ran_before_the_connection_was_production() {
    let mut h = Harness::start().await;
    let oracle = h.oracle().await;
    let table = unique("t_guard");
    oracle.batch_execute(&format!("create table public.{table} (n int)")).await.unwrap();
    set_environment(&h, Some(Environment::Staging)).await;
    let tab = h.sql_tab(TxnMode::Manual).await;
    let one = format!("insert into public.{table} values (1)");
    assert_eq!(h.run_ok(tab, &one).await.txn, TxnState::Active);
    set_environment(&h, Some(Environment::Production)).await;

    let asked = match h.engine.console_commit(tab, None).await {
        Err(TxnError::ConfirmWrites { confirmation }) => confirmation,
        other => panic!("expected ConfirmWrites, got {other:?}"),
    };
    assert_eq!((asked.sends.count, asked.holds.count, asked.holds.statements.as_slice()), (0, 1, [one.clone()].as_slice()));
    let typed = asked_to_run(&h, tab, script("commit"), None).await;
    assert_eq!((typed.sends.count, typed.holds.statements.as_slice()), (0, [one.clone()].as_slice()), "a typed COMMIT names the writes it keeps");
    assert_eq!(h.console(tab).txn, TxnState::Active, "a refused commit sends nothing");
    assert_eq!(count(&oracle, &table).await, 0);

    h.engine.console_commit(tab, Some(asked.digest)).await.unwrap();
    assert_eq!(count(&oracle, &table).await, 1);

    let confirmed = asked_to_run(&h, tab, script(&one), None).await;
    let accepted = h.engine.console_run(tab, script(&one), Some(confirmed.digest)).await.unwrap();
    h.wait_finished(tab, accepted.run_id).await;
    h.engine.console_commit(tab, None).await.expect("a write asked as Production commits without a second question");
    assert_eq!(count(&oracle, &table).await, 2);
    oracle.batch_execute(&format!("drop table public.{table}")).await.unwrap();
}

async fn staging_writes_held_then_production(h: &mut Harness, table: &str) -> TabId {
    set_environment(h, Some(Environment::Staging)).await;
    let tab = h.sql_tab(TxnMode::Manual).await;
    assert_eq!(h.run_ok(tab, &format!("insert into public.{table} values (1)")).await.txn, TxnState::Active);
    set_environment(h, Some(Environment::Production)).await;
    tab
}

#[tokio::test]
async fn every_run_on_production_asks_while_the_transaction_holds_writes_that_ran_before() {
    let mut h = Harness::start().await;
    let oracle = h.oracle().await;
    let table = unique("t_guard");
    oracle.batch_execute(&format!("create table public.{table} (n int)")).await.unwrap();
    let tab = staging_writes_held_then_production(&mut h, &table).await;

    let held = format!("insert into public.{table} values (1)");
    for (text, sends) in [("select 1", 0), ("select 1;\ncommit", 0), ("end", 0), ("prepare transaction 'l1_held'", 1)] {
        let asked = asked_to_run(&h, tab, script(text), None).await;
        assert_eq!((asked.sends.count, asked.holds.count, asked.holds.statements.as_slice()), (sends, 1, [held.clone()].as_slice()), "{text}");
        assert_eq!(h.console(tab).txn, TxnState::Active, "{text}: a refused run sends nothing");
    }
    assert_eq!(count(&oracle, &table).await, 0);

    let asked = asked_to_run(&h, tab, script("select 1"), None).await;
    let accepted = h.engine.console_run(tab, script("select 1"), Some(asked.digest)).await.unwrap();
    h.wait_finished(tab, accepted.run_id).await;
    h.run_ok(tab, "commit").await;
    assert_eq!(count(&oracle, &table).await, 1, "the confirmed run marked the held write asked");
    oracle.batch_execute(&format!("drop table public.{table}")).await.unwrap();
}

#[tokio::test]
async fn a_begin_behind_any_comment_holds_its_writes_until_production_asks() {
    let mut h = Harness::start().await;
    let oracle = h.oracle().await;
    let table = unique("t_guard");
    oracle.batch_execute(&format!("create table public.{table} (n int)")).await.unwrap();
    for header in ["/*/ header */", "/* a /* b */ c */", "-- c\r"] {
        set_environment(&h, Some(Environment::Staging)).await;
        let tab = h.sql_tab(TxnMode::AutoCommit).await;
        let held = format!("insert into public.{table} values (1)");
        assert_eq!(h.run_ok(tab, &format!("{header} begin;\n{held};")).await.txn, TxnState::Active, "{header:?}");
        set_environment(&h, Some(Environment::Production)).await;
        for text in ["select now()", "commit"] {
            let asked = asked_to_run(&h, tab, script(text), None).await;
            assert_eq!(asked.holds.statements, [held.clone()], "{header:?} then {text}");
        }
        assert!(matches!(h.engine.console_commit(tab, None).await, Err(TxnError::ConfirmWrites { .. })), "{header:?}");
        assert_eq!(count(&oracle, &table).await, 0, "{header:?}: nothing committed unasked");
        h.engine.console_rollback(tab).await.unwrap();
    }
    oracle.batch_execute(&format!("drop table public.{table}")).await.unwrap();
}

#[tokio::test]
async fn a_rollback_to_a_savepoint_keeps_the_writes_before_it_held() {
    let mut h = Harness::start().await;
    let oracle = h.oracle().await;
    let table = unique("t_guard");
    oracle.batch_execute(&format!("create table public.{table} (n int)")).await.unwrap();
    let held = format!("insert into public.{table} values (1)");
    for rollback_to in ["rollback to s", "rollback work to savepoint s", "ROLLBACK TRANSACTION TO s"] {
        set_environment(&h, Some(Environment::Staging)).await;
        let tab = h.sql_tab(TxnMode::AutoCommit).await;
        let state = h.run_ok(tab, &format!("begin;\n{held};\nsavepoint s;\n{rollback_to}")).await;
        assert_eq!(state.txn, TxnState::Active, "{rollback_to}");
        set_environment(&h, Some(Environment::Production)).await;
        let asked = asked_to_run(&h, tab, script("commit"), None).await;
        assert_eq!(asked.holds.statements, [held.clone()], "{rollback_to}");
        assert_eq!(count(&oracle, &table).await, 0, "{rollback_to}: nothing committed unasked");
        h.engine.console_rollback(tab).await.unwrap();
    }
    oracle.batch_execute(&format!("drop table public.{table}")).await.unwrap();
}

#[tokio::test]
async fn pointing_a_tab_at_another_connection_drops_the_writes_its_session_held() {
    let mut h = Harness::start().await;
    let oracle = h.oracle().await;
    let table = unique("t_guard");
    oracle.batch_execute(&format!("create table public.{table} (n int)")).await.unwrap();
    set_environment(&h, Some(Environment::Staging)).await;
    let tab = h.sql_tab(TxnMode::AutoCommit).await;
    h.run_ok(tab, &format!("insert into public.{table} values (1)")).await;
    let production = ConnectionDraft { environment: Some(Environment::Production), ..draft_from_url(&url(), "prod").unwrap() };
    let production = h.engine.save_connection(production).await.unwrap().id;
    let config = ConsoleConfig { connection_id: production, database: h.database.clone(), schema: None, mode: TxnMode::AutoCommit };
    h.engine.console_configure(tab, config).await.unwrap();
    h.run_ok(tab, "select 1").await;
    asked_to_run(&h, tab, script(&format!("insert into public.{table} values (2)")), None).await;
    oracle.batch_execute(&format!("drop table public.{table}")).await.unwrap();
}

#[tokio::test]
async fn a_staged_apply_off_production_stays_held_until_commit_asks() {
    let mut h = Harness::start().await;
    h.engine.connect(h.conn).await.unwrap();
    set_environment(&h, Some(Environment::Staging)).await;
    let setup = h.sql_tab(TxnMode::AutoCommit).await;
    let table = edit_table(&mut h, setup).await;
    let tab = h.sql_tab(TxnMode::Manual).await;
    let mut edits = changes(&table);
    edits.inserts.push(vec![cell("name", Some("d"))]);
    assert!(!h.engine.table_apply(tab, edits, None).await.unwrap().committed);
    set_environment(&h, Some(Environment::Production)).await;

    let asked = match h.engine.console_commit(tab, None).await {
        Err(TxnError::ConfirmWrites { confirmation }) => confirmation,
        other => panic!("expected ConfirmWrites, got {other:?}"),
    };
    assert_eq!(asked.holds.statements, [format!("INSERT INTO \"public\".\"{table}\" (\"name\") VALUES ('d')")]);
    h.engine.console_rollback(tab).await.unwrap();
    set_environment(&h, None).await;
    h.run_ok(setup, &format!("drop table public.{table}")).await;
}

#[tokio::test]
async fn writes_stay_held_until_the_engine_sees_their_transaction_end() {
    let mut h = Harness::start().await;
    let oracle = h.oracle().await;
    let table = unique("t_guard");
    oracle.batch_execute(&format!("create table public.{table} (n int)")).await.unwrap();
    set_environment(&h, Some(Environment::Staging)).await;
    let tab = h.sql_tab(TxnMode::AutoCommit).await;
    let write = format!("insert into public.{table} values (1)");
    for end in ["commit", "rollback", "end", "abort"] {
        set_environment(&h, Some(Environment::Staging)).await;
        assert_eq!(h.run_ok(tab, &write).await.txn, TxnState::Idle);
        set_environment(&h, Some(Environment::Production)).await;
        let asked = asked_to_run(&h, tab, script("select 1"), None).await;
        assert_eq!(asked.holds.statements, [write.clone()], "an Idle the engine infers does not end the write's transaction");
        set_environment(&h, Some(Environment::Staging)).await;
        h.run_ok(tab, end).await;
        set_environment(&h, Some(Environment::Production)).await;
        h.run_ok(tab, "select 1").await;
    }
    assert_eq!(count(&oracle, &table).await, 4);
    oracle.batch_execute(&format!("drop table public.{table}")).await.unwrap();
}

#[tokio::test]
async fn commit_and_chain_keeps_a_transaction_open_and_ends_the_writes_it_held() {
    let mut h = Harness::start().await;
    let oracle = h.oracle().await;
    let table = unique("t_guard");
    oracle.batch_execute(&format!("create table public.{table} (n int)")).await.unwrap();
    set_environment(&h, Some(Environment::Staging)).await;
    let tab = h.sql_tab(TxnMode::AutoCommit).await;
    h.run_ok(tab, "begin").await;
    assert_eq!(h.run_ok(tab, "commit and chain").await.txn, TxnState::Active);
    assert_eq!(h.run_ok(tab, &format!("insert into public.{table} values (1)")).await.txn, TxnState::Active);
    set_environment(&h, Some(Environment::Production)).await;
    assert!(matches!(h.engine.console_commit(tab, None).await, Err(TxnError::ConfirmWrites { .. })));
    asked_to_run(&h, tab, script("commit"), None).await;
    assert_eq!(count(&oracle, &table).await, 0);

    set_environment(&h, Some(Environment::Staging)).await;
    assert_eq!(h.run_ok(tab, "commit work and chain").await.txn, TxnState::Active);
    assert_eq!(count(&oracle, &table).await, 1);
    assert_eq!(h.run_ok(tab, &format!("insert into public.{table} values (2)")).await.txn, TxnState::Active);
    assert_eq!(h.run_ok(tab, "rollback and chain").await.txn, TxnState::Active);
    set_environment(&h, Some(Environment::Production)).await;
    h.run_ok(tab, "select 1").await;
    h.engine.console_commit(tab, None).await.expect("a chained end left no held writes");
    assert_eq!(count(&oracle, &table).await, 1);
    oracle.batch_execute(&format!("drop table public.{table}")).await.unwrap();
}

#[tokio::test]
async fn prepared_transaction_statements_on_production_wait_for_the_digest() {
    let mut h = Harness::start().await;
    let oracle = h.oracle().await;
    let table = unique("t_guard");
    let gid = unique("l1_gid");
    oracle.batch_execute(&format!("create table public.{table} (n int)")).await.unwrap();
    let held = staging_writes_held_then_production(&mut h, &table).await;
    let other = h.sql_tab(TxnMode::AutoCommit).await;
    for (tab, text) in [(held, format!("prepare transaction '{gid}'")), (other, format!("commit prepared '{gid}'")), (other, format!("rollback prepared '{gid}'"))] {
        let asked = asked_to_run(&h, tab, script(&text), None).await;
        assert_eq!(asked.sends.statements, [text.clone()]);
    }
    let prepared: i64 = oracle.query_one(&format!("select count(*) from pg_prepared_xacts where gid = '{gid}'"), &[]).await.unwrap().get(0);
    assert_eq!((prepared, count(&oracle, &table).await), (0, 0), "a refused statement prepares and commits nothing");
    h.engine.console_rollback(held).await.unwrap();
    oracle.batch_execute(&format!("drop table public.{table}")).await.unwrap();
}

#[tokio::test]
async fn a_transaction_prepared_off_production_commits_on_production_only_with_the_digest() {
    let mut h = Harness::start().await;
    let oracle = h.oracle().await;
    let slots: String = oracle.query_one("show max_prepared_transactions", &[]).await.unwrap().get(0);
    if slots == "0" {
        assert!(
            std::env::var_os("L1_DATA_TEST_PG_NO_PREPARED").is_some(),
            "the server has max_prepared_transactions = 0. Run `pnpm db:up` to recreate it with slots, or set L1_DATA_TEST_PG_NO_PREPARED=1 to skip this test"
        );
        return;
    }
    let table = unique("t_guard");
    let gid = unique("l1_gid");
    oracle.batch_execute(&format!("create table public.{table} (n int)")).await.unwrap();
    let held = staging_writes_held_then_production(&mut h, &table).await;
    let prepared = format!("select count(*) from pg_prepared_xacts where gid = '{gid}'");
    let prepared = async || oracle.query_one(&prepared, &[]).await.unwrap().get::<_, i64>(0);

    set_environment(&h, Some(Environment::Staging)).await;
    h.run_ok(held, &format!("prepare transaction '{gid}'")).await;
    assert_eq!(prepared().await, 1);
    set_environment(&h, Some(Environment::Production)).await;
    let other = h.sql_tab(TxnMode::AutoCommit).await;
    let commit = format!("commit prepared '{gid}'");
    let asked = asked_to_run(&h, other, script(&commit), None).await;
    asked_to_run(&h, other, script(&format!("rollback prepared '{gid}'")), None).await;
    assert_eq!((prepared().await, count(&oracle, &table).await), (1, 0));
    let accepted = h.engine.console_run(other, script(&commit), Some(asked.digest)).await.unwrap();
    h.wait_finished(other, accepted.run_id).await;
    assert_eq!((prepared().await, count(&oracle, &table).await), (0, 1));
    oracle.batch_execute(&format!("drop table public.{table}")).await.unwrap();
}

#[tokio::test]
async fn ddl_committed_by_commit_and_chain_refreshes_the_catalog() {
    let mut h = Harness::start().await;
    let tab = h.sql_tab(TxnMode::AutoCommit).await;
    h.engine.connect(h.conn).await.unwrap();
    let table = unique("t_chain");
    let initial = h.engine.catalog(h.db()).await.unwrap().version;
    h.run_ok(tab, &format!("begin;\ncreate table public.{table} (id int);\ncommit and chain;\nrollback")).await;
    let catalog = h.catalog_when(&h.db(), |c| c.version > initial).await;
    assert!(catalog.relation("public", &table).is_some(), "the chained commit kept the table");
    h.run_ok(tab, &format!("drop table public.{table}")).await;
}

const TYPES_NO_SCRIPT_MAKES: &str = "create schema lab; create type lab.mood as enum ('sad', 'ok', 'happy')";

const SEED_RELATIONS: &str = "n.nspname IN ('public', 'lab') AND c.relkind IN ('r', 'p', 'v', 'm') AND c.relname !~ '[0-9a-f]{32}'";

async fn catalog_facts(client: &tokio_postgres::Client, relations: &str) -> Vec<String> {
    let sql = format!(
        "SELECT n.nspname || '.' || c.relname || ' ' || f.fact
         FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
         LEFT JOIN pg_am am ON am.oid = c.relam
         LEFT JOIN pg_foreign_table ft ON ft.ftrelid = c.oid LEFT JOIN pg_foreign_server fs ON fs.oid = ft.ftserver,
         LATERAL (
            SELECT concat_ws(' ', 'relation', c.relkind, c.relpersistence, pg_get_partkeydef(c.oid), pg_get_expr(c.relpartbound, c.oid), obj_description(c.oid, 'pg_class'),
                             c.reloptions, (SELECT t.reloptions FROM pg_class t WHERE t.oid = c.reltoastrelid), c.relrowsecurity, c.relforcerowsecurity, c.relreplident,
                             c.relispopulated, am.amname, fs.srvname, ft.ftoptions, c.reloftype::regtype,
                             (SELECT string_agg(h.inhparent::regclass::text, ',' ORDER BY h.inhseqno) FROM pg_inherits h WHERE h.inhrelid = c.oid))
            UNION ALL
            SELECT concat_ws(' ', 'column', a.attnum, a.attname, format_type(a.atttypid, a.atttypmod), a.attnotnull, a.attidentity, a.attgenerated,
                             pg_get_expr(d.adbin, d.adrelid), (SELECT co.collnamespace::regnamespace::text || '.' || quote_ident(co.collname) FROM pg_collation co WHERE co.oid = a.attcollation), col_description(c.oid, a.attnum), a.attislocal, a.attinhcount,
                             a.attstattarget, a.attstorage, to_jsonb(a) ->> 'attcompression', a.attoptions, a.attfdwoptions)
            FROM pg_attribute a LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum
            WHERE a.attrelid = c.oid AND a.attnum > 0 AND NOT a.attisdropped
            UNION ALL
            SELECT concat_ws(' ', 'sequence', dep.deptype, a.attname, sc.oid::regclass, format_type(s.seqtypid, NULL), s.seqstart, s.seqincrement, s.seqmin, s.seqmax, s.seqcache, s.seqcycle,
                             sc.relpersistence, obj_description(sc.oid, 'pg_class'))
            FROM pg_depend dep JOIN pg_class sc ON sc.oid = dep.objid AND sc.relkind = 'S' JOIN pg_sequence s ON s.seqrelid = sc.oid
            JOIN pg_attribute a ON a.attrelid = dep.refobjid AND a.attnum = dep.refobjsubid
            WHERE dep.classid = 'pg_class'::regclass AND dep.refclassid = 'pg_class'::regclass AND dep.refobjid = c.oid AND dep.deptype IN ('a', 'i')
            UNION ALL
            SELECT concat_ws(' ', 'constraint', k.conname, pg_get_constraintdef(k.oid), k.convalidated, k.conislocal, k.coninhcount, obj_description(k.oid, 'pg_constraint'))
            FROM pg_constraint k WHERE k.conrelid = c.oid
            UNION ALL
            SELECT concat_ws(' ', 'index', pg_get_indexdef(i.indexrelid), i.indisclustered, i.indisreplident, obj_description(i.indexrelid, 'pg_class'),
                             (SELECT string_agg(a.attnum || '=' || a.attstattarget, ',' ORDER BY a.attnum) FROM pg_attribute a WHERE a.attrelid = i.indexrelid),
                             (SELECT ic.reltablespace FROM pg_class ic WHERE ic.oid = i.indexrelid))
            FROM pg_index i WHERE i.indrelid = c.oid
            UNION ALL
            SELECT concat_ws(' ', 'trigger', pg_get_triggerdef(t.oid), t.tgenabled, obj_description(t.oid, 'pg_trigger')) FROM pg_trigger t WHERE t.tgrelid = c.oid AND NOT t.tgisinternal
            UNION ALL
            SELECT concat_ws(' ', 'rule', pg_get_ruledef(r.oid), r.ev_enabled, obj_description(r.oid, 'pg_rewrite')) FROM pg_rewrite r WHERE r.ev_class = c.oid AND r.rulename <> '_RETURN'
            UNION ALL
            SELECT concat_ws(' ', 'policy', p.polname, p.polpermissive, p.polcmd, p.polroles::regrole[], pg_get_expr(p.polqual, p.polrelid), pg_get_expr(p.polwithcheck, p.polrelid),
                             obj_description(p.oid, 'pg_policy'))
            FROM pg_policy p WHERE p.polrelid = c.oid
            UNION ALL
            SELECT concat_ws(' ', 'statistics', pg_get_statisticsobjdef(x.oid), to_jsonb(x) ->> 'stxstattarget', obj_description(x.oid, 'pg_statistic_ext')) FROM pg_statistic_ext x WHERE x.stxrelid = c.oid
            UNION ALL
            SELECT concat_ws(' ', 'query', pg_get_viewdef(c.oid)) WHERE c.relkind IN ('v', 'm')
         ) f(fact)
         WHERE {relations}
         ORDER BY 1"
    );
    client.query(&sql, &[]).await.unwrap().iter().map(|r| r.get(0)).collect()
}

/// `DROP DATABASE ... WITH (FORCE)` needs PostgreSQL 13, and the seeded round trip runs on 12 too.
async fn drop_database(client: &tokio_postgres::Client, name: &str) {
    for _ in 0..50 {
        client.execute("select pg_terminate_backend(pid) from pg_stat_activity where datname = $1 and pid <> pg_backend_pid()", &[&name]).await.unwrap();
        if client.batch_execute(&format!("drop database {name}")).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("could not drop database {name}");
}

/// What a routine's script must recreate. Its owner and privileges are only warned about.
async fn routine_facts(client: &tokio_postgres::Client, schema: &str) -> Vec<String> {
    let sql = "SELECT concat_ws(' ', p.oid::regprocedure, pg_get_functiondef(p.oid), obj_description(p.oid, 'pg_proc'), p.prosecdef,
                                (SELECT string_agg(e.extname, ',' ORDER BY e.extname) FROM pg_depend d JOIN pg_extension e ON e.oid = d.refobjid
                                 WHERE d.classid = 'pg_proc'::regclass AND d.objid = p.oid AND d.refclassid = 'pg_extension'::regclass AND d.deptype = 'x'))
               FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace WHERE n.nspname = $1 ORDER BY 1";
    client.query(sql, &[&schema]).await.unwrap().iter().map(|r| r.get(0)).collect()
}

async fn replay_in_dependency_order(client: &tokio_postgres::Client, mut scripts: Vec<String>) {
    while !scripts.is_empty() {
        let mut failed = vec![];
        for script in &scripts {
            if let Err(e) = client.batch_execute(script).await {
                failed.push((script.clone(), e.as_db_error().map(|e| e.message().to_owned())));
            }
        }
        assert!(failed.len() < scripts.len(), "no script replays: {failed:#?}");
        scripts = failed.into_iter().map(|(script, _)| script).collect();
    }
}

#[tokio::test]
async fn object_scripts_replay_into_an_empty_database_with_the_same_catalog() {
    let h = Harness::start().await;
    h.engine.connect(h.conn).await.unwrap();
    let (shop, connection) = tokio_postgres::connect(&url(), NoTls).await.unwrap();
    tokio::spawn(connection);
    let name = unique("t_db_script");
    shop.batch_execute(&format!("create database {name}")).await.unwrap();
    let mut config: tokio_postgres::Config = url().parse().unwrap();
    let (scratch, connection) = config.dbname(&name).connect(NoTls).await.unwrap();
    tokio::spawn(connection);

    let outcome = futures_util::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(async {
        let script = async |target: ScriptTarget| h.engine.object_script(h.db(), target).await;
        let oids = async |sql: &str| shop.query(sql, &[]).await.unwrap().iter().map(|r| r.get::<_, u32>(0)).collect::<Vec<_>>();

        scratch.batch_execute(TYPES_NO_SCRIPT_MAKES).await.unwrap();
        scratch.batch_execute("set check_function_bodies = off").await.unwrap();
        let routines = oids("select p.oid from pg_proc p join pg_namespace n on n.oid = p.pronamespace where n.nspname = 'public' and p.prokind <> 'a' and p.proname !~ '[0-9a-f]{32}' order by p.oid").await;
        let mut texts = vec![];
        for oid in routines {
            let s = script(ScriptTarget::Routine { oid }).await.unwrap();
            assert!(s.text.starts_with("CREATE OR REPLACE "), "{}", s.text);
            texts.push(s.text);
        }
        replay_in_dependency_order(&scratch, texts).await;

        let relations = oids(&format!("select c.oid from pg_class c join pg_namespace n on n.oid = c.relnamespace where {SEED_RELATIONS} and not c.relispartition order by c.oid")).await;
        let mut texts = vec![];
        for oid in relations {
            texts.push(script(ScriptTarget::Relation { oid }).await.unwrap().text);
        }
        replay_in_dependency_order(&scratch, texts).await;
        assert_eq!(catalog_facts(&scratch, SEED_RELATIONS).await, catalog_facts(&shop, SEED_RELATIONS).await);

        let oid = async |name: &str| oids(&format!("select '{name}'::regclass::oid")).await[0];
        let orders = script(ScriptTarget::Relation { oid: oid("public.orders").await }).await.unwrap();
        assert_eq!(orders.title, "orders.sql");
        assert!(orders.text.starts_with("CREATE TABLE public.orders (\n    id bigint GENERATED ALWAYS AS IDENTITY NOT NULL,\n"), "{}", orders.text);
        for line in [
            "    CONSTRAINT orders_customer_id_fkey FOREIGN KEY (customer_id) REFERENCES public.customers(id),\n",
            "CREATE INDEX orders_open_idx ON public.orders USING btree (status) WHERE (status <> 'paid'::text);\n",
            "CREATE TRIGGER orders_audit AFTER INSERT OR DELETE OR UPDATE ON public.orders FOR EACH ROW EXECUTE FUNCTION public.audit_order();\n",
        ] {
            assert!(orders.text.contains(line), "{line} missing from {}", orders.text);
        }
        let partition = script(ScriptTarget::Relation { oid: oid("public.order_events_2026_h1").await }).await.unwrap();
        assert_eq!(
            partition.text,
            "CREATE TABLE public.order_events_2026_h1 PARTITION OF public.order_events
    FOR VALUES FROM ('2026-01-01') TO ('2026-07-01');

DO $l1$
DECLARE
    entry text[];
    attached regclass;
    occupant regclass;
BEGIN
    FOREACH entry SLICE 1 IN ARRAY ARRAY[
        ['public.order_events_2026_h1', 'public.order_events_kind_idx', 'order_events_2026_h1_kind_idx'],
        ['public.order_events_2026_h1', 'public.order_events_pkey', 'order_events_2026_h1_pkey']
    ] LOOP
        attached := (SELECT h.inhrelid FROM pg_catalog.pg_inherits h JOIN pg_catalog.pg_index i ON i.indexrelid = h.inhrelid
                     WHERE h.inhparent = pg_catalog.to_regclass(entry[2]) AND i.indrelid = entry[1]::regclass);
        occupant := (SELECT c.oid FROM pg_catalog.pg_class c WHERE c.relname = entry[3]
                     AND c.relnamespace = (SELECT a.relnamespace FROM pg_catalog.pg_class a WHERE a.oid = attached));
        IF attached IS NULL THEN
            RAISE WARNING 'L1 Data found no index on % attached to %, so it could not name it %', entry[1], entry[2], entry[3];
        ELSIF occupant IS NULL THEN
            EXECUTE pg_catalog.format('ALTER INDEX %s RENAME TO %I', attached, entry[3]);
        ELSIF occupant <> attached AND EXISTS (SELECT FROM pg_catalog.pg_inherits h JOIN pg_catalog.pg_index i ON i.indexrelid = h.inhrelid
                                               WHERE h.inhrelid = occupant AND i.indrelid = entry[1]::regclass) THEN
            EXECUTE pg_catalog.format('ALTER INDEX %s RENAME TO %I', occupant, 'l1_' || occupant::oid);
            EXECUTE pg_catalog.format('ALTER INDEX %s RENAME TO %I', attached, entry[3]);
        ELSIF occupant <> attached THEN
            IF (SELECT c.relname FROM pg_catalog.pg_class c WHERE c.oid = attached) <> 'l1_' || attached::oid THEN
                EXECUTE pg_catalog.format('ALTER INDEX %s RENAME TO %I', attached, 'l1_' || attached::oid);
            END IF;
            RAISE WARNING 'L1 Data named index % instead of %, because %, which this script did not create, has that name', attached, entry[3], occupant;
        END IF;
    END LOOP;
END
$l1$;
"
        );
        let index = script(ScriptTarget::Index { relation_oid: oid("public.orders").await, name: "orders_open_idx".into() }).await.unwrap();
        assert_eq!((index.title.as_str(), index.text.as_str()), ("orders_open_idx.sql", "CREATE INDEX orders_open_idx ON public.orders USING btree (status) WHERE (status <> 'paid'::text);\n"));
        let pkey = script(ScriptTarget::Index { relation_oid: oid("public.orders").await, name: "orders_pkey".into() }).await.unwrap();
        assert_eq!(pkey.text, "ALTER TABLE public.orders ADD CONSTRAINT orders_pkey PRIMARY KEY (id);\n");
        let trigger = script(ScriptTarget::Trigger { relation_oid: oid("public.orders").await, name: "orders_set_updated_at".into() }).await.unwrap();
        assert_eq!(trigger.text, "CREATE TRIGGER orders_set_updated_at BEFORE UPDATE ON public.orders FOR EACH ROW EXECUTE FUNCTION public.set_updated_at();\n");

        let aggregate = oids("select 'public.product_of(numeric)'::regprocedure::oid").await[0];
        assert!(matches!(script(ScriptTarget::Routine { oid: aggregate }).await, Err(DetailError::Failed { message }) if message == "Aggregates cannot be scripted yet."));
        assert!(matches!(script(ScriptTarget::Index { relation_oid: oid("public.orders").await, name: "missing".into() }).await, Err(DetailError::Gone)));
        let detail = h.engine.relation_detail(h.db(), oid("public.orders").await).await.unwrap();
        assert_eq!(detail.triggers.len(), 2, "the metadata session left the script's read-only transaction");
    }))
    .await;
    drop(scratch);
    drop_database(&shop, &name).await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

/// Names a replay's copies of the parent's indexes would take are held by the partition's own index
/// and own unique constraint, and in one target by an index on another table.
const NAME_CLASHES: &str = "CREATE SCHEMA a;
CREATE TABLE a.nat (id int, k int) PARTITION BY RANGE (id);
CREATE TABLE a.nat_1 PARTITION OF a.nat FOR VALUES FROM (0) TO (10);
CREATE INDEX ON a.nat_1 (k) WHERE k > 0;
ALTER TABLE a.nat_1 ADD CONSTRAINT nat_1_id_idx UNIQUE (id);
CREATE INDEX nat_k ON a.nat (k);
CREATE INDEX nat_id ON a.nat (id) WITH (fillfactor = 50);
ALTER INDEX a.nat_1_id_idx1 SET (fillfactor = 33);
COMMENT ON INDEX a.nat_1_k_idx1 IS 'attached';
ALTER TABLE a.nat_1 CLUSTER ON nat_1_k_idx1;";

#[tokio::test]
async fn a_partition_tree_replays_its_index_names_without_a_clash_and_leaves_other_tables_alone() {
    let h = Harness::start().await;
    h.engine.connect(h.conn).await.unwrap();
    let (shop, connection) = tokio_postgres::connect(&url(), NoTls).await.unwrap();
    tokio::spawn(connection);
    let (source, clean, crowded) = (unique("t_db_clash"), unique("t_db_clash_clean"), unique("t_db_clash_crowded"));
    for name in [&source, &clean, &crowded] {
        shop.batch_execute(&format!("create database {name}")).await.unwrap();
    }
    h.engine.refresh_connection(h.conn).await.unwrap();
    h.edit_view(ViewEdit::Database { name: source.clone(), shown: true }).await;
    let (notices, mut heard) = tokio::sync::mpsc::unbounded_channel::<String>();
    let connect = async |name: &str| {
        let mut config: tokio_postgres::Config = url().parse().unwrap();
        let (client, mut connection) = config.dbname(name).connect(NoTls).await.unwrap();
        let notices = notices.clone();
        tokio::spawn(async move {
            while let Some(Ok(message)) = std::future::poll_fn(|cx| connection.poll_message(cx)).await {
                if let tokio_postgres::AsyncMessage::Notice(notice) = message {
                    let _ = notices.send(notice.message().to_owned());
                }
            }
        });
        client
    };
    let (original, replay, beside) = (connect(&source).await, connect(&clean).await, connect(&crowded).await);
    let schema_a = "n.nspname = 'a' AND c.relkind IN ('r', 'p')";
    let version: i32 = shop.query_one("select current_setting('server_version_num')::int4", &[]).await.unwrap().get(0);
    let space = (version >= 150000).then(|| unique("t_ts"));
    if let Some(space) = &space {
        shop.batch_execute("set allow_in_place_tablespaces = on").await.unwrap();
        shop.batch_execute(&format!("create tablespace {space} location ''")).await.unwrap();
    }

    let outcome = futures_util::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(async {
        original.batch_execute(NAME_CLASHES).await.unwrap();
        if let Some(space) = &space {
            original.batch_execute(&format!("ALTER INDEX a.nat_1_k_idx1 SET TABLESPACE {space}")).await.unwrap();
        }
        let nat: u32 = original.query_one("select 'a.nat'::regclass::oid", &[]).await.unwrap().get(0);
        let script = h.engine.object_script(h.db_named(&source), ScriptTarget::Relation { oid: nat }).await.unwrap();
        assert!(script.warnings.is_empty(), "{:?}", script.warnings);

        replay.batch_execute(&format!("CREATE SCHEMA a;\n{}", script.text)).await.unwrap_or_else(|e| panic!("{e:?}\n{}", script.text));
        assert_eq!(catalog_facts(&replay, schema_a).await, catalog_facts(&original, schema_a).await);

        beside.batch_execute("CREATE SCHEMA a; CREATE TABLE a.bystander (v int); CREATE INDEX nat_1_k_idx1 ON a.bystander (v);").await.unwrap();
        while heard.try_recv().is_ok() {}
        beside.batch_execute(&script.text).await.unwrap_or_else(|e| panic!("{e:?}\n{}", script.text));
        let index_of = "SELECT c.relname, i.indisclustered, coalesce(obj_description(c.oid, 'pg_class'), '') FROM pg_index i JOIN pg_class c ON c.oid = i.indexrelid";
        let bystander = beside.query_one(&format!("{index_of} WHERE i.indrelid = 'a.bystander'::regclass"), &[]).await.unwrap();
        assert_eq!((bystander.get::<_, &str>(0), bystander.get::<_, bool>(1), bystander.get::<_, &str>(2)), ("nat_1_k_idx1", false, ""), "the index on another table keeps its name and gets nothing");
        let copy = beside.query_one(&format!("{index_of} JOIN pg_inherits h ON h.inhrelid = i.indexrelid WHERE i.indrelid = 'a.nat_1'::regclass AND h.inhparent = 'a.nat_k'::regclass"), &[]).await.unwrap();
        let copy_name: String = copy.get(0);
        assert!(copy_name.starts_with("l1_"), "{copy_name}");
        assert_eq!((copy.get::<_, bool>(1), copy.get::<_, &str>(2)), (true, "attached"), "the copy moves out of the way of the partition's own index and still gets its properties");
        let mut warnings = vec![];
        while let Ok(notice) = heard.try_recv() {
            warnings.push(notice);
        }
        assert_eq!(warnings, [format!("L1 Data named index a.{copy_name} instead of nat_1_k_idx1, because a.nat_1_k_idx1, which this script did not create, has that name")]);
    }))
    .await;
    drop((original, replay, beside));
    for name in [&source, &clean, &crowded] {
        drop_database(&shop, name).await;
    }
    if let Some(space) = &space {
        shop.batch_execute(&format!("drop tablespace {space}")).await.unwrap();
    }
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

const HOSTILE: &str = include_str!("fixtures/script-hostile-pg.sql");
const HOSTILE_PRELUDE: &str = include_str!("fixtures/script-hostile-pg-prelude.sql");
const HOSTILE_RELATIONS: &str = "n.nspname IN ('Weird \"S\"', 'app', 'order') AND c.relkind IN ('r', 'p', 'v', 'm', 'f')";

#[tokio::test]
async fn hostile_object_scripts_replay_every_property_or_warn() {
    let h = Harness::start().await;
    h.engine.connect(h.conn).await.unwrap();
    let (shop, connection) = tokio_postgres::connect(&url(), NoTls).await.unwrap();
    tokio::spawn(connection);
    let (source, target) = (unique("t_db_hostile"), unique("t_db_hostile_replay"));
    for name in [&source, &target] {
        shop.batch_execute(&format!("create database {name}")).await.unwrap();
    }
    h.engine.refresh_connection(h.conn).await.unwrap();
    h.edit_view(ViewEdit::Database { name: source.clone(), shown: true }).await;
    let connect = async |name: &str| {
        let mut config: tokio_postgres::Config = url().parse().unwrap();
        let (client, connection) = config.dbname(name).connect(NoTls).await.unwrap();
        tokio::spawn(connection);
        client
    };
    let (original, replay) = (connect(&source).await, connect(&target).await);

    let outcome = futures_util::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(async {
        original.batch_execute(HOSTILE).await.unwrap();
        assert!(original.batch_execute("CREATE UNIQUE INDEX CONCURRENTLY dups_v ON app.dups (v)").await.is_err(), "the duplicate rows leave dups_v invalid");
        replay.batch_execute(HOSTILE_PRELUDE).await.unwrap();
        let db = h.db_named(&source);
        let script = async |target: ScriptTarget| h.engine.object_script(db.clone(), target).await.unwrap();
        let oids = async |sql: &str| original.query(sql, &[]).await.unwrap().iter().map(|r| r.get::<_, u32>(0)).collect::<Vec<_>>();
        let oid = async |name: &str| oids(&format!("select '{name}'::regclass::oid")).await[0];

        let mut texts = vec![];
        let mut warned = std::collections::BTreeMap::new();
        for oid in oids("select p.oid from pg_proc p join pg_namespace n on n.oid = p.pronamespace where n.nspname = 'app' order by p.oid").await {
            let s = script(ScriptTarget::Routine { oid }).await;
            if !s.warnings.is_empty() {
                warned.insert(s.title.clone(), s.warnings.clone());
            }
            texts.push(s.text);
        }
        assert_eq!(
            warned,
            std::collections::BTreeMap::from([
                ("f_out.sql".to_owned(), vec!["owner pg_database_owner of app.f_out(a integer, OUT b integer, OUT c integer)".to_owned()]),
                ("labeled_fn.sql".to_owned(), vec!["security labels on app.labeled_fn()".to_owned()]),
                (
                    "sec_def.sql".to_owned(),
                    vec![
                        "owner l1data of app.sec_def(integer), whose privileges the SECURITY DEFINER function runs with".to_owned(),
                        "privileges on app.sec_def(integer) (EXECUTE revoked from PUBLIC)".to_owned()
                    ]
                ),
            ])
        );
        replay_in_dependency_order(&replay, texts).await;
        assert_eq!(routine_facts(&replay, "app").await, routine_facts(&original, "app").await);

        let mut texts = vec![];
        let mut warned = std::collections::BTreeMap::new();
        for oid in oids(&format!("select c.oid from pg_class c join pg_namespace n on n.oid = c.relnamespace where {HOSTILE_RELATIONS} and not c.relispartition order by c.oid")).await {
            let s = script(ScriptTarget::Relation { oid }).await;
            if !s.warnings.is_empty() {
                assert!(s.text.starts_with(&s.warnings.iter().map(|w| format!("-- L1 Data could not script: {w}\n")).collect::<String>()), "{}", s.text);
                warned.insert(s.title.clone(), s.warnings.clone());
            }
            texts.push(s.text);
        }
        replay_in_dependency_order(&replay, texts).await;
        assert_eq!(
            warned,
            std::collections::BTreeMap::from([
                ("My View.sql".to_owned(), vec!["owner l1data of app.\"My View\", whose privileges the view's query is checked against".to_owned()]),
                ("child.sql".to_owned(), vec!["the column order of app.child, because a replay puts inherited columns first".to_owned()]),
                ("dups.sql".to_owned(), vec!["the invalid state of index app.dups_v, which a replay builds valid".to_owned()]),
                ("group.sql".to_owned(), vec!["membership of \"order\".\"group\" in publication hostile_schema_pub through schema \"order\"".to_owned()]),
                (
                    "labeled.sql".to_owned(),
                    vec![
                        "security labels on app.labeled".to_owned(),
                        "security labels on app.labeled.note".to_owned(),
                        "security labels on app.labeled_id_seq".to_owned(),
                        "security labels on app.labeled_s_seq".to_owned(),
                    ]
                ),
                ("mv.sql".to_owned(), vec!["owner l1data of app.mv, whose privileges a refresh runs the query with".to_owned()]),
                ("pt.sql".to_owned(), vec!["the partial attachment of index app.pt_only, which a replay attaches to every partition".to_owned()]),
                ("refd.sql".to_owned(), vec!["membership of app.refd in publication hostile_pub".to_owned()]),
                ("ren.sql".to_owned(), vec!["privileges on app.ren_custom_seq (USAGE granted to pg_read_all_data)".to_owned()]),
                ("ri.sql".to_owned(), vec!["owner pg_database_owner of app.ri_2".to_owned()]),
                ("select.sql".to_owned(), vec!["privileges on app.\"select\" (SELECT granted to pg_read_all_data, UPDATE (\"Mixed\") granted to pg_read_all_data)".to_owned()]),
                ("v_def.sql".to_owned(), vec!["owner l1data of app.v_def, whose privileges the view's query is checked against".to_owned()]),
            ])
        );
        let reordered = |facts: Vec<String>| facts.into_iter().filter(|f| !f.starts_with("app.child column") && !f.starts_with("app.pt1 index")).collect::<Vec<_>>();
        assert_eq!(reordered(catalog_facts(&replay, HOSTILE_RELATIONS).await), reordered(catalog_facts(&original, HOSTILE_RELATIONS).await));
        let columns = "select string_agg(attname, ',' order by attname) from pg_attribute where attrelid = 'app.child'::regclass and attnum > 0";
        let (a, b) = (original.query_one(columns, &[]).await.unwrap(), replay.query_one(columns, &[]).await.unwrap());
        assert_eq!(a.get::<_, String>(0), b.get::<_, String>(0));

        let mig = script(ScriptTarget::Relation { oid: oid("app.mig").await }).await.text;
        assert!(mig.starts_with("CREATE SEQUENCE app.mig_old_seq AS bigint START WITH 777;\n\nCREATE TABLE app.mig (\n    id integer GENERATED BY DEFAULT AS IDENTITY NOT NULL\n);\n"), "{mig}");
        let ev = script(ScriptTarget::Relation { oid: oid("app.ev").await }).await.text;
        for part in [
            "CREATE TABLE app.ev_a PARTITION OF app.ev\n    FOR VALUES IN ('a') PARTITION BY RANGE (d);\n",
            "CREATE UNLOGGED TABLE app.ev_a_2026 PARTITION OF app.ev_a (\n    CONSTRAINT ev_a_2026_chk CHECK ((v > 0))\n)\n    FOR VALUES FROM ('2026-01-01') TO ('2027-01-01');\n",
            "ALTER TABLE app.ev_a_2026 ALTER COLUMN v SET DEFAULT 9;\n",
            "CREATE INDEX ev_a_2026_local ON app.ev_a_2026 USING btree (v);\n",
            "ALTER TABLE app.ev_b ALTER COLUMN v DROP DEFAULT;\n",
            "ALTER TABLE app.ev_b ALTER COLUMN v SET NOT NULL;\n",
            "CREATE FOREIGN TABLE app.ev_remote PARTITION OF app.ev\n    FOR VALUES IN ('r') SERVER hostile_server OPTIONS (table_name 'ev, remote', \"Odd\" 'it''s');\n",
            "ALTER FOREIGN TABLE app.ev_remote ALTER COLUMN v OPTIONS (ADD column_name 'vv');\n",
        ] {
            assert!(ev.contains(part), "{part} missing from {ev}");
        }
        let leaf = script(ScriptTarget::Relation { oid: oid("app.ev_a").await }).await.text;
        assert!(leaf.contains("CREATE UNLOGGED TABLE app.ev_a_2026 PARTITION OF app.ev_a"), "{leaf}");
        let parent = script(ScriptTarget::Relation { oid: oid("\"Weird \"\"S\"\"\".parent").await }).await.text;
        for part in ["ALTER TABLE \"Weird \"\"S\"\"\".parent ENABLE ROW LEVEL SECURITY;\n", "ALTER TABLE \"Weird \"\"S\"\"\".parent FORCE ROW LEVEL SECURITY;\n", "CREATE POLICY everyone ON"] {
            assert!(parent.contains(part), "{part} missing from {parent}");
        }
        let tree = script(ScriptTarget::Relation { oid: oid("app.tree").await }).await.text;
        assert!(tree.ends_with("CREATE UNIQUE INDEX tree_id ON app.tree USING btree (id);\n\nALTER TABLE app.tree ADD CONSTRAINT tree_parent FOREIGN KEY (parent) REFERENCES app.tree(id);\n"), "{tree}");
        let child = script(ScriptTarget::Relation { oid: oid("app.child").await }).await.text;
        assert!(child.contains(") INHERITS (app.base);\n"), "{child}");
        let attached = script(ScriptTarget::Index { relation_oid: oid("app.nat_1").await, name: "nat_1_k_idx1".into() }).await.text;
        assert_eq!(attached, "CREATE INDEX nat_1_k_idx1 ON app.nat_1 USING btree (k);\n\nALTER INDEX app.nat_k ATTACH PARTITION app.nat_1_k_idx1;\n");
        let attached_key = script(ScriptTarget::Index { relation_oid: oid("app.ffp_1").await, name: "ffp one key".into() }).await.text;
        assert_eq!(
            attached_key,
            "ALTER TABLE app.ffp_1 ADD CONSTRAINT \"ffp one key\" PRIMARY KEY (id, k) WITH (fillfactor=60);\n\nALTER INDEX app.ffp_pkey ATTACH PARTITION app.\"ffp one key\";\n\nCOMMENT ON CONSTRAINT \"ffp one key\" ON app.ffp_1 IS 'cloned key';\n"
        );
        let pkey = script(ScriptTarget::Index { relation_oid: oid("\"Weird \"\"S\"\"\".parent").await, name: "parent_pkey".into() }).await.text;
        assert_eq!(
            pkey,
            "ALTER TABLE \"Weird \"\"S\"\"\".parent ADD CONSTRAINT parent_pkey PRIMARY KEY (\"Id\");\n\nCOMMENT ON INDEX \"Weird \"\"S\"\"\".parent_pkey IS 'pkey comment';\n\nCOMMENT ON CONSTRAINT parent_pkey ON \"Weird \"\"S\"\"\".parent IS 'pkey constraint comment';\n"
        );
    }))
    .await;
    drop((original, replay));
    for name in [&source, &target] {
        drop_database(&shop, name).await;
    }
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test]
async fn explain_fences_keep_the_transaction_and_roll_back_an_update() {
    let mut h = Harness::start().await;
    let oracle = h.oracle().await;
    let table = unique("t_plan");
    oracle.batch_execute(&format!("create table public.{table} (n int)")).await.unwrap();
    oracle.batch_execute(&format!("insert into public.{table} (n) values (1)")).await.unwrap();
    let auto = h.sql_tab(TxnMode::AutoCommit).await;

    let refused = h.engine.console_run(auto, script_mode("select 1; select 2", RunMode::Explain), None).await.unwrap_err();
    assert!(matches!(refused, RunError::Unsupported { ref what } if what == "Explain one statement."), "{refused:?}");
    let refused = h.engine.console_run(auto, script_mode("/* c */ explain select 1", RunMode::Explain), None).await.unwrap_err();
    assert!(matches!(refused, RunError::Unsupported { ref what } if what == "The statement already starts with EXPLAIN."), "{refused:?}");
    let refused = h.engine.console_run(auto, script_mode(" -- nothing\n", RunMode::ExplainAnalyze), None).await.unwrap_err();
    assert!(matches!(refused, RunError::NoStatement), "{refused:?}");
    assert_eq!(h.console(auto).txn, TxnState::Idle, "a refusal sends nothing");

    let select = "select 1";
    h.drain();
    let before = h.log(auto).len();
    let explained = finish_ok(&mut h, auto, select, RunMode::Explain).await;
    h.drain();
    assert_eq!(explained.txn, TxnState::Idle);
    assert_eq!(&h.log(auto)[before..], [format!("0> EXPLAIN (FORMAT JSON) {select}")]);
    let meta = plan_meta(&explained);
    let plan = h.engine.result_plan(meta.r#ref).unwrap();
    assert_eq!(plan.statements.len(), 1);
    assert_eq!(plan.statements[0].root.operation, "Result");
    assert!(plan.statements[0].planning_time_ms.is_none(), "plain explain has no planning time until analyze");
    assert!(plan.statements[0].execution_time_ms.is_none(), "plain explain has no execution time");
    let cell = h.engine.result_cell(meta.r#ref, 0, 0).unwrap().unwrap();
    assert!(cell.contains("\"Node Type\""), "{cell}");
    let (summary, file) = h.export(&meta, ExportFormat::Json, ExportScope::Loaded, "plan.json").await;
    assert!(summary.rows >= 1);
    assert!(file.contains("Node Type"), "{file}");
    let all = h.engine.result_export(meta.r#ref, ExportFormat::Json, ExportScope::All, h.dir.path().join("all.json").display().to_string()).await.unwrap_err();
    assert!(matches!(all, ExportError::NotRerunnable), "{all:?}");
    wait_for_history(&h, &format!("EXPLAIN (FORMAT JSON) {select}")).await;

    let rows = h.run_ok(auto, "select 1").await;
    let err = h.engine.result_plan(Harness::result(&rows, 0).r#ref).unwrap_err();
    assert!(matches!(err, ResultError::NotAPlan { .. }), "{err:?}");

    let update = format!("update public.{table} set n = 2");
    h.drain();
    let before = h.log(auto).len();
    let analyzed = finish_ok(&mut h, auto, &update, RunMode::ExplainAnalyze).await;
    h.drain();
    assert_eq!(analyzed.txn, TxnState::Idle, "an idle analyze rolls its transaction back");
    assert_eq!(&h.log(auto)[before..], [format!("0> EXPLAIN (FORMAT JSON, ANALYZE, BUFFERS) {update}")]);
    let analyzed_plan = h.engine.result_plan(plan_meta(&analyzed).r#ref).unwrap();
    let modify = find_node(&analyzed_plan.statements[0].root, "ModifyTable").expect("update plan");
    assert_eq!(modify.detail, format!("on {table}"));
    assert!(modify.actual_rows.is_some());
    assert!(modify.buffers.is_some(), "analyze asks for buffers");
    assert!(analyzed_plan.statements[0].planning_time_ms.is_some());
    assert!(analyzed_plan.statements[0].execution_time_ms.is_some());
    assert_eq!(count(&oracle, &table).await, 1, "the analyzed update did not commit");
    assert_eq!(h.scalar(auto, &format!("select n from public.{table}")).await, "1");

    h.drain();
    let before = h.log(auto).len();
    let missing = h.engine.console_run(auto, script_mode("select * from public.does_not_exist", RunMode::Explain), None).await.unwrap();
    let failed = h.wait_finished(auto, missing.run_id).await;
    h.drain();
    assert!(matches!(failed.run, RunState::Finished { outcome: RunOutcome::Failed, .. }), "{:?}", failed.run);
    assert_eq!(failed.txn, TxnState::Idle, "a failed explain still rolls back");
    assert!(h.log(auto)[before..].iter().any(|line| line.contains("ERROR")), "{:?}", &h.log(auto)[before..]);
    assert!(!h.log(auto)[before..].iter().any(|line| line.contains("BEGIN")), "the fence stays out of the log");
    assert_eq!(h.scalar(auto, "select 1").await, "1", "the session still runs");

    let manual = h.sql_tab(TxnMode::Manual).await;
    h.run_ok(manual, &format!("insert into public.{table} (n) values (3)")).await;
    assert_eq!(h.console(manual).txn, TxnState::Active);
    let nested = format!("select n from public.{table}");
    h.drain();
    let before = h.log(manual).len();
    let inside = finish_ok(&mut h, manual, &nested, RunMode::Explain).await;
    h.drain();
    assert_eq!(inside.txn, TxnState::Active, "a savepoint explain leaves the transaction open");
    assert_eq!(&h.log(manual)[before..], [format!("0> EXPLAIN (FORMAT JSON) {nested}")]);
    let nested_plan = h.engine.result_plan(plan_meta(&inside).r#ref).unwrap();
    assert!(!nested_plan.statements[0].root.operation.is_empty());
    assert_eq!(h.scalar(manual, &format!("select count(*) from public.{table}")).await, "2", "the inserted row is still visible");

    let rewrite = format!("update public.{table} set n = 9");
    let rewritten = finish_ok(&mut h, manual, &rewrite, RunMode::ExplainAnalyze).await;
    assert_eq!(rewritten.txn, TxnState::Active, "analyze inside a transaction rolls back to the savepoint");
    assert_eq!(h.scalar(manual, &format!("select count(*) from public.{table} where n = 9")).await, "0");
    assert_eq!(count(&oracle, &table).await, 1, "the open transaction is not visible outside");

    h.drain();
    let before = h.log(manual).len();
    let broken = h.engine.console_run(manual, script_mode("select * from public.does_not_exist", RunMode::ExplainAnalyze), None).await.unwrap();
    let broken = h.wait_finished(manual, broken.run_id).await;
    h.drain();
    assert!(matches!(broken.run, RunState::Finished { outcome: RunOutcome::Failed, .. }), "{:?}", broken.run);
    assert_eq!(broken.txn, TxnState::Active, "a failed analyze returns to the savepoint");
    assert!(h.log(manual)[before..].iter().any(|line| line.contains("ERROR")));
    assert_eq!(h.scalar(manual, &format!("select count(*) from public.{table}")).await, "2");

    let aborted = h.run(manual, "select 1/0").await;
    assert_eq!(aborted.txn, TxnState::Aborted);
    let refused = h.engine.console_run(manual, script_mode("select 1", RunMode::Explain), None).await.unwrap_err();
    assert!(matches!(refused, RunError::Unsupported { ref what } if what == "The transaction must be rolled back first."), "{refused:?}");
    assert_eq!(h.console(manual).txn, TxnState::Aborted);
    h.engine.console_rollback(manual).await.unwrap();
    assert_eq!(h.console(manual).txn, TxnState::Idle);
    assert_eq!(count(&oracle, &table).await, 1);

    oracle.batch_execute(&format!("drop table public.{table}")).await.unwrap();
}

fn script_mode(text: &str, mode: RunMode) -> RunRequest {
    RunRequest::Script { text: text.into(), target: RunTarget::All, mode, params: Vec::new() }
}

async fn finish_ok(h: &mut Harness, tab: TabId, sql: &str, mode: RunMode) -> ConsoleState {
    let accepted = h.engine.console_run(tab, script_mode(sql, mode), None).await.unwrap();
    let state = h.wait_finished(tab, accepted.run_id).await;
    assert!(matches!(state.run, RunState::Finished { outcome: RunOutcome::Ok, .. }), "{sql}: {:?}", state.run);
    state
}

fn plan_meta(state: &ConsoleState) -> ResultMeta {
    match &state.run {
        RunState::Finished { statements, .. } => match statements[0].outputs.first() {
            Some(RunOutput::Plan { result, format: PlanFormat::Json }) => result.clone(),
            other => panic!("not a plan: {other:?}"),
        },
        other => panic!("not finished: {other:?}"),
    }
}

fn find_node<'a>(node: &'a PlanNode, operation: &str) -> Option<&'a PlanNode> {
    if node.operation == operation {
        return Some(node);
    }
    node.children.iter().find_map(|child| find_node(child, operation))
}

async fn wait_for_history(h: &Harness, sql: &str) {
    let started = Instant::now();
    loop {
        let page = h.engine.history(HistoryQuery { text: Some(sql.into()), connection_id: Some(h.conn), before_id: None, limit: 20 });
        if page.entries.iter().any(|entry| entry.sql == sql) {
            return;
        }
        assert!(started.elapsed() < Duration::from_secs(2), "history never recorded {sql}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn parameters_quote_values_keep_ranges_history_and_explain() {
    let mut h = Harness::start().await;
    let tab = h.sql_tab(TxnMode::AutoCommit).await;
    h.run_ok(tab, "SET standard_conforming_strings = off").await;
    let text = "select '😀'; select :value::text, :value::text, $1::text, (ARRAY[1,2,3])[1:2]";
    let value = "x'\\; select pg_sleep(9); -- 😀";
    let params = vec![ParamValue { name: ":value".into(), value: Some(value.into()) }, ParamValue { name: "$1".into(), value: None }];
    let accepted = h.engine.console_run(tab, RunRequest::Script { text: text.into(), target: RunTarget::All, mode: RunMode::Run, params }, None).await.unwrap();
    assert_eq!(accepted.statements[1].from, DocOffset(13));
    let state = h.wait_finished(tab, accepted.run_id).await;
    let rows = h.page(&Harness::result(&state, 1), 0, 1);
    assert_eq!(rows[0], [Some(value.into()), Some(value.into()), None, Some("{1,2}".into())]);
    let executed = format!("select E'{}'::text, E'{}'::text, NULL::text, (ARRAY[1,2,3])[1:2]", value.replace('\\', "\\\\").replace('\'', "''"), value.replace('\\', "\\\\").replace('\'', "''"));
    wait_for_history(&h, &executed).await;
    let bad = h.engine.console_run(tab, RunRequest::Script { text: "select :x::integer".into(), target: RunTarget::All, mode: RunMode::Run, params: vec![ParamValue { name: ":x".into(), value: Some("invalid".into()) }] }, None).await.unwrap();
    let failed = h.wait_finished(tab, bad.run_id).await;
    let RunState::Finished { statements, .. } = failed.run else { panic!("not finished") };
    let StmtStatus::Failed { error } = &statements[0].status else { panic!("expected error") };
    assert_eq!(error.position, None);
    let mixed = "select :x; select missing_p11_column";
    let accepted = h.engine.console_run(tab, RunRequest::Script { text: mixed.into(), target: RunTarget::All, mode: RunMode::Run, params: vec![ParamValue { name: ":x".into(), value: Some("length changes here".into()) }] }, None).await.unwrap();
    let state = h.wait_finished(tab, accepted.run_id).await;
    let RunState::Finished { statements, .. } = state.run else { panic!("not finished") };
    let StmtStatus::Failed { error } = &statements[1].status else { panic!("expected error") };
    assert_eq!(error.position, Some(DocOffset(mixed.find("missing_p11_column").unwrap() as u32)));
    let accepted = h.engine.console_run(tab, RunRequest::Script { text: "-- 😀\nselect :x::integer".into(), target: RunTarget::All, mode: RunMode::Explain, params: vec![ParamValue { name: ":x".into(), value: Some("42".into()) }] }, None).await.unwrap();
    let state = h.wait_finished(tab, accepted.run_id).await;
    assert!(matches!(state.run, RunState::Finished { outcome: RunOutcome::Ok, .. }), "{:?}", state.run);
}

#[tokio::test]
async fn production_parameter_confirmation_covers_exact_values() {
    let mut h = Harness::start().await;
    let oracle = h.oracle().await;
    let table = unique("t_params");
    oracle.batch_execute(&format!("create table public.{table} (n text)")).await.unwrap();
    set_environment(&h, Some(Environment::Production)).await;
    let tab = h.sql_tab(TxnMode::AutoCommit).await;
    let request = |value: &str| RunRequest::Script { text: format!("insert into public.{table} values (:value)"), target: RunTarget::All, mode: RunMode::Run, params: vec![ParamValue { name: ":value".into(), value: Some(value.into()) }] };
    let asked = asked_to_run(&h, tab, request("one"), None).await;
    assert_eq!(asked.sends.statements, [format!("insert into public.{table} values (E'one')")]);
    let changed = asked_to_run(&h, tab, request("two"), Some(&asked.digest)).await;
    assert_ne!(asked.digest, changed.digest);
    assert_eq!(count(&oracle, &table).await, 0);
    let accepted = h.engine.console_run(tab, request("two"), Some(changed.digest)).await.unwrap();
    h.wait_finished(tab, accepted.run_id).await;
    assert_eq!(oracle.query_one(&format!("select n from public.{table}"), &[]).await.unwrap().get::<_, String>(0), "two");
    oracle.batch_execute(&format!("drop table public.{table}")).await.unwrap();
}

#[tokio::test]
async fn postgres_server_parameters_keep_their_runtime_arguments() {
    let mut h = Harness::start().await;
    let tab = h.sql_tab(TxnMode::AutoCommit).await;
    let prepared = unique("p_native_params");
    let state = h.run_ok(tab, &format!("PREPARE {prepared}(int) AS SELECT $1; EXECUTE {prepared}(2)")).await;
    assert_eq!(h.page(&Harness::result(&state, 1), 0, 1), [vec![Some("2".into())]]);
    let function = unique("f_native_params");
    h.run_ok(tab, &format!("CREATE FUNCTION public.{function}(int) RETURNS int LANGUAGE SQL RETURN $1")).await;
    assert_eq!(h.scalar(tab, &format!("SELECT public.{function}(3)")).await, "3");
    h.run_ok(tab, &format!("DROP FUNCTION public.{function}(int)")).await;
}

async fn import_target(h: &Harness, table: &str) -> ImportTarget {
    h.engine.connect(h.conn).await.unwrap();
    let catalog = h.catalog_when(&h.db(), |c| c.relation("public", table).is_some()).await;
    ImportTarget { db: h.db(), schema: "public".into(), table: table.into(), oid: catalog.relation("public", table).unwrap().oid }
}

async fn csv_request(h: &Harness, tab: TabId, target: &ImportTarget, text: &str, header: bool) -> ImportRequest {
    let path = h.dir.path().join(format!("{}.csv", uuid::Uuid::new_v4()));
    std::fs::write(&path, text).unwrap();
    let source = h.engine.table_import_prepare(tab, target.clone(), path.to_string_lossy().into()).await.unwrap();
    let preview = h.engine.table_import_preview(tab, source, header).await.unwrap();
    ImportRequest { source, header, mapping: preview.mapping }
}

#[tokio::test]
async fn csv_import_preserves_mapping_defaults_exact_values_and_prepared_bytes() {
    let mut h = Harness::start().await;
    let oracle = h.oracle().await;
    let table = unique("p12_values");
    oracle.batch_execute(&format!("CREATE TABLE {table} (id int GENERATED ALWAYS AS IDENTITY, n numeric, txt text, fallback text DEFAULT 'kept')")).await.unwrap();
    let tab = h.sql_tab(TxnMode::AutoCommit).await;
    let target = import_target(&h, &table).await;
    let path = h.dir.path().join("exact.csv");
    std::fs::write(&path, "txt,unused,n\r\n\"a,b\n雪\",skip,9007199254740993\r\n,skip,2\r\n\"\",skip,3").unwrap();
    let source = h.engine.table_import_prepare(tab, target.clone(), path.to_string_lossy().into()).await.unwrap();
    let preview = h.engine.table_import_preview(tab, source, true).await.unwrap();
    assert_eq!(preview.rows, 3);
    assert_eq!(preview.mapping, vec![Some("txt".into()), None, Some("n".into())]);
    assert!(!preview.columns.iter().any(|c| c.name == "id"));
    let without_header = h.engine.table_import_preview(tab, source, false).await.unwrap();
    assert_eq!(without_header.rows, 4);
    assert_eq!(without_header.mapping, vec![Some("n".into()), Some("txt".into()), Some("fallback".into())]);
    std::fs::write(&path, "n\n99\n").unwrap();
    let request = ImportRequest { source, header: true, mapping: preview.mapping };
    let summary = h.engine.table_import(tab, request.clone(), None).await.unwrap();
    assert_eq!((summary.rows, summary.committed), (3, true));
    assert!(matches!(h.engine.table_import(tab, request, None).await, Err(ImportError::Gone)));
    let rows = oracle.query(&format!("SELECT n::text,txt,fallback FROM {table} ORDER BY id"), &[]).await.unwrap();
    assert_eq!(rows[0].get::<_, String>(0), "9007199254740993");
    assert_eq!(rows[0].get::<_, String>(1), "a,b\n雪");
    assert_eq!(rows[1].get::<_, Option<String>>(1), None);
    assert_eq!(rows[2].get::<_, Option<String>>(1), Some("".into()));
    assert!(rows.iter().all(|r| r.get::<_, String>(2) == "kept"));
    h.run_ok(tab, "SELECT 1").await;
    oracle.batch_execute(&format!("DROP TABLE {table}")).await.unwrap();
}

#[tokio::test]
async fn csv_import_bad_row_and_savepoint_keep_the_prior_transaction() {
    let mut h = Harness::start().await;
    let oracle = h.oracle().await;
    let table = unique("p12_atomic");
    oracle.batch_execute(&format!("CREATE TABLE {table} (n int CHECK (n > 0))")).await.unwrap();
    let tab = h.sql_tab(TxnMode::AutoCommit).await;
    let target = import_target(&h, &table).await;
    let request = csv_request(&h, tab, &target, "n\n1\n2\nwrong\n", true).await;
    let error = h.engine.table_import(tab, request, None).await.unwrap_err();
    assert!(matches!(&error, ImportError::Query { error } if error.context.as_ref().is_some_and(|c| c.contains("line 3"))), "{error:?}");
    assert_eq!(count(&oracle, &table).await, 0);
    h.run_ok(tab, &format!("BEGIN; INSERT INTO {table} VALUES (42)")).await;
    let request = csv_request(&h, tab, &target, "n\n3\n-1\n", true).await;
    assert!(h.engine.table_import(tab, request, None).await.is_err());
    assert_eq!(h.console(tab).txn, TxnState::Active);
    assert_eq!(h.scalar(tab, &format!("SELECT count(*)::text FROM {table}")).await, "1");
    let request = csv_request(&h, tab, &target, "n\n7\n8\n", true).await;
    assert!(!h.engine.table_import(tab, request, None).await.unwrap().committed);
    assert_eq!(count(&oracle, &table).await, 0);
    h.engine.console_commit(tab, None).await.unwrap();
    assert_eq!(count(&oracle, &table).await, 3);
    oracle.batch_execute(&format!("DROP TABLE {table}")).await.unwrap();
}

#[tokio::test]
async fn csv_import_manual_rollback_and_production_bind_the_whole_request() {
    let h = Harness::start().await;
    let oracle = h.oracle().await;
    let table = unique("p12_guard");
    oracle.batch_execute(&format!("CREATE TABLE {table} (a int, b int)")).await.unwrap();
    let tab = h.sql_tab(TxnMode::Manual).await;
    let target = import_target(&h, &table).await;
    let request = csv_request(&h, tab, &target, "a,b\n1,2\n", true).await;
    assert!(!h.engine.table_import(tab, request, None).await.unwrap().committed);
    assert_eq!(h.console(tab).txn, TxnState::Active);
    h.engine.console_rollback(tab).await.unwrap();
    assert_eq!(count(&oracle, &table).await, 0);
    set_environment(&h, Some(Environment::Production)).await;
    let request = csv_request(&h, tab, &target, "a,b\n3,4\n", true).await;
    let Err(ImportError::ConfirmWrites { confirmation }) = h.engine.table_import(tab, request.clone(), None).await else { panic!("Production must ask") };
    assert_eq!(count(&oracle, &table).await, 0);
    assert_eq!(confirmation.sends.count, 1);
    assert!(confirmation.sends.statements[0].starts_with("COPY"));
    let swapped = ImportRequest { mapping: vec![Some("b".into()), Some("a".into())], ..request.clone() };
    assert!(matches!(h.engine.table_import(tab, swapped, Some(confirmation.digest.clone())).await, Err(ImportError::ConfirmWrites { .. })));
    assert!(!h.engine.table_import(tab, request, Some(confirmation.digest)).await.unwrap().committed);
    h.engine.console_commit(tab, None).await.unwrap();
    assert_eq!(count(&oracle, &table).await, 1);
    let request = csv_request(&h, tab, &target, "a,b\n5,6\n", true).await;
    let Err(ImportError::ConfirmWrites { confirmation }) = h.engine.table_import(tab, request.clone(), None).await else { panic!() };
    h.engine.table_import_discard(tab, request.source).await.unwrap();
    assert!(matches!(h.engine.table_import(tab, request, Some(confirmation.digest)).await, Err(ImportError::Gone)));
    assert_eq!(count(&oracle, &table).await, 1);
    oracle.batch_execute(&format!("DROP TABLE {table}")).await.unwrap();
}

#[tokio::test]
async fn csv_import_preparation_replacement_discard_close_and_invalid_files() {
    let h = Harness::start().await;
    let oracle = h.oracle().await;
    let table = unique("p12_prepare");
    oracle.batch_execute(&format!("CREATE TABLE {table} (n int)")).await.unwrap();
    let tab = h.sql_tab(TxnMode::AutoCommit).await;
    let target = import_target(&h, &table).await;
    let path = h.dir.path().join("large.csv");
    std::fs::write(&path, "1\n".repeat(1_000_000)).unwrap();
    let first = h.engine.table_import_prepare(tab, target.clone(), path.to_string_lossy().into()).await.unwrap();
    let second = h.engine.table_import_prepare(tab, target.clone(), path.to_string_lossy().into()).await.unwrap();
    assert!(matches!(h.engine.table_import_preview(tab, first, false).await, Err(ImportError::Gone)));
    h.engine.table_import_discard(tab, first).await.unwrap();
    h.engine.table_import_discard(tab, second).await.unwrap();
    assert!(matches!(h.engine.table_import_preview(tab, second, false).await, Err(ImportError::Gone)));
    let third = h.engine.table_import_prepare(tab, target.clone(), path.to_string_lossy().into()).await.unwrap();
    h.engine.console_close(tab, true).await.unwrap();
    assert!(matches!(h.engine.table_import_preview(tab, third, false).await, Err(ImportError::Gone)));
    let tab = h.sql_tab(TxnMode::AutoCommit).await;
    for bytes in [b"n\n\"bad".as_slice(), b"n\n1,2", b"n\n\xff", b"n\na\0"] {
        std::fs::write(&path, bytes).unwrap();
        let source = h.engine.table_import_prepare(tab, target.clone(), path.to_string_lossy().into()).await.unwrap();
        assert!(matches!(h.engine.table_import_preview(tab, source, true).await, Err(ImportError::Csv { .. })));
    }
    let source = h.engine.table_import_prepare(tab, target, h.dir.path().to_string_lossy().into()).await.unwrap();
    assert!(matches!(h.engine.table_import_preview(tab, source, false).await, Err(ImportError::Io { .. })));
    oracle.batch_execute(&format!("DROP TABLE {table}")).await.unwrap();
}

#[tokio::test]
async fn csv_import_cancel_is_busy_atomic_and_survives_a_dropped_waiter() {
    let mut h = Harness::start().await;
    let oracle = h.oracle().await;
    let table = unique("p12_cancel");
    oracle.batch_execute(&format!("CREATE TABLE {table} (n int); CREATE FUNCTION {table}_slow() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_sleep(0.001); RETURN NEW; END $$; CREATE TRIGGER slow BEFORE INSERT ON {table} FOR EACH ROW EXECUTE FUNCTION {table}_slow()" )).await.unwrap();
    let tab = h.sql_tab(TxnMode::AutoCommit).await;
    let target = import_target(&h, &table).await;
    let request = csv_request(&h, tab, &target, &"1\n".repeat(100_000), false).await;
    let engine = h.engine.clone();
    let waiter = tokio::spawn(async move { engine.table_import(tab, request, None).await });
    let start = Instant::now();
    loop {
        if h.console(tab).import.is_some_and(|p| p.bytes > 0) { break; }
        assert!(start.elapsed() < Duration::from_secs(10));
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(matches!(h.engine.console_run(tab, script("SELECT 1"), None).await, Err(RunError::Busy)));
    assert!(matches!(h.engine.console_commit(tab, None).await, Err(TxnError::Busy)));
    waiter.abort();
    h.engine.console_cancel(tab).await;
    let start = Instant::now();
    while h.console(tab).work.is_some() {
        assert!(start.elapsed() < Duration::from_secs(12));
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(count(&oracle, &table).await, 0);
    assert_eq!(h.console(tab).txn, TxnState::Idle);
    h.run_ok(tab, "SELECT 1").await;
    oracle.batch_execute(&format!("DROP TABLE {table}; DROP FUNCTION {table}_slow()")).await.unwrap();
}

#[tokio::test]
async fn csv_import_drafts_end_on_reconfigure_disconnect_and_reject_fifos() {
    let h = Harness::start().await;
    let oracle = h.oracle().await;
    let table = unique("p12_lifecycle");
    oracle.batch_execute(&format!("CREATE TABLE {table} (n int)")).await.unwrap();
    let tab = h.sql_tab(TxnMode::AutoCommit).await;
    let target = import_target(&h, &table).await;
    let request = csv_request(&h, tab, &target, "n\n1\n", true).await;
    h.engine.console_configure(tab, ConsoleConfig { connection_id: h.conn, database: h.database.clone(), schema: None, mode: TxnMode::Manual }).await.unwrap();
    assert!(matches!(h.engine.table_import(tab, request, None).await, Err(ImportError::Gone)));
    let request = csv_request(&h, tab, &target, "n\n1\n", true).await;
    h.engine.disconnect(h.conn, true).await.unwrap();
    assert!(matches!(h.engine.table_import_preview(tab, request.source, true).await, Err(ImportError::Gone)));
    #[cfg(unix)] {
        let fifo = h.dir.path().join("pipe.csv");
        assert!(std::process::Command::new("mkfifo").arg(&fifo).status().unwrap().success());
        let source = h.engine.table_import_prepare(tab, target, fifo.to_string_lossy().into()).await.unwrap();
        let result = tokio::time::timeout(Duration::from_secs(2), h.engine.table_import_preview(tab, source, false)).await.unwrap();
        assert!(matches!(result, Err(ImportError::Io { .. })), "{result:?}");
    }
    oracle.batch_execute(&format!("DROP TABLE {table}")).await.unwrap();
}

#[tokio::test]
async fn csv_import_pending_writes_and_stale_production_target_remain_guarded() {
    let h = Harness::start().await;
    let oracle = h.oracle().await;
    let table = unique("p12_held");
    oracle.batch_execute(&format!("CREATE TABLE {table} (n int)")).await.unwrap();
    let tab = h.sql_tab(TxnMode::Manual).await;
    let target = import_target(&h, &table).await;
    let request = csv_request(&h, tab, &target, "n\n7\n", true).await;
    h.engine.table_import(tab, request, None).await.unwrap();
    set_environment(&h, Some(Environment::Production)).await;
    let Err(TxnError::ConfirmWrites { confirmation }) = h.engine.console_commit(tab, None).await else { panic!("pending import must be covered") };
    assert_eq!(confirmation.holds.count, 1);
    h.engine.console_rollback(tab).await.unwrap();
    let request = csv_request(&h, tab, &target, "n\n8\n", true).await;
    let Err(ImportError::ConfirmWrites { confirmation }) = h.engine.table_import(tab, request.clone(), None).await else { panic!() };
    set_environment(&h, Some(Environment::Staging)).await;
    assert!(matches!(h.engine.table_import(tab, request, Some(confirmation.digest)).await, Err(ImportError::TargetChanged)));
    assert_eq!(count(&oracle, &table).await, 0);
    oracle.batch_execute(&format!("DROP TABLE {table}")).await.unwrap();
}

#[tokio::test]
async fn csv_import_rejects_a_replacement_relation_even_with_a_stale_catalog() {
    let h = Harness::start().await;
    let oracle = h.oracle().await;
    let table = unique("p12_oid");
    oracle.batch_execute(&format!("CREATE TABLE {table} (n int)")).await.unwrap();
    let tab = h.sql_tab(TxnMode::AutoCommit).await;
    let target = import_target(&h, &table).await;
    let request = csv_request(&h, tab, &target, "n\n1\n", true).await;
    oracle.batch_execute(&format!("DROP TABLE {table}; CREATE TABLE {table} (n int)")).await.unwrap();
    assert!(matches!(h.engine.table_import(tab, request, None).await, Err(ImportError::TargetChanged)));
    assert_eq!(count(&oracle, &table).await, 0);
    assert_eq!(h.console(tab).txn, TxnState::Idle);
    oracle.batch_execute(&format!("DROP TABLE {table}")).await.unwrap();
}

#[tokio::test]
async fn csv_import_close_cannot_publish_into_a_reopened_tab() {
    let mut h = Harness::start().await;
    let oracle = h.oracle().await;
    let table = unique("p12_orphan");
    oracle.batch_execute(&format!("CREATE TABLE {table} (n int)")).await.unwrap();
    let tab = h.sql_tab(TxnMode::AutoCommit).await;
    let target = import_target(&h, &table).await;
    oracle.batch_execute(&format!("BEGIN; LOCK TABLE {table} IN ACCESS EXCLUSIVE MODE")).await.unwrap();
    let request = csv_request(&h, tab, &target, "n\n1\n", true).await;
    let engine = h.engine.clone();
    let duplicate = request.clone();
    let worker = tokio::spawn(async move { engine.table_import(tab, request, None).await });
    let until = Instant::now();
    while h.console(tab).work != Some(ConsoleWork::Import) {
        assert!(until.elapsed() < Duration::from_secs(5));
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(matches!(h.engine.table_import(tab, duplicate, None).await, Err(ImportError::Gone)));
    h.engine.console_close(tab, true).await.unwrap();
    h.engine.console_configure(tab, ConsoleConfig { connection_id: h.conn, database: h.database.clone(), schema: None, mode: TxnMode::AutoCommit }).await.unwrap();
    h.run_ok(tab, "SELECT 1").await;
    oracle.batch_execute("ROLLBACK").await.unwrap();
    assert!(matches!(worker.await.unwrap(), Err(ImportError::Gone)));
    assert_eq!(h.console(tab).work, None);
    assert!(matches!(h.console(tab).run, RunState::Finished { outcome: RunOutcome::Ok, .. }));
    assert_eq!(count(&oracle, &table).await, 0);
    oracle.batch_execute(&format!("DROP TABLE {table}")).await.unwrap();
}

#[tokio::test]
async fn csv_import_lost_cleanup_reports_unknown_and_retires_the_session() {
    let h = Harness::start().await;
    let oracle = h.oracle().await;
    let table = unique("p12_lost");
    oracle.batch_execute(&format!("CREATE TABLE {table} (n int)")).await.unwrap();
    let tab = h.sql_tab(TxnMode::AutoCommit).await;
    let target = import_target(&h, &table).await;
    oracle.batch_execute(&format!("BEGIN; LOCK TABLE {table} IN ACCESS EXCLUSIVE MODE")).await.unwrap();
    let request = csv_request(&h, tab, &target, "n\n1\n", true).await;
    let engine = h.engine.clone();
    let worker = tokio::spawn(async move { engine.table_import(tab, request, None).await });
    let until = Instant::now();
    let pid = loop {
        let state = h.console(tab);
        if state.work == Some(ConsoleWork::Import) && let SessionState::Open { backend_pid } = state.session { break backend_pid; }
        assert!(until.elapsed() < Duration::from_secs(5));
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    loop {
        let row = oracle.query_one("SELECT wait_event_type FROM pg_stat_activity WHERE pid = $1", &[&pid]).await.unwrap();
        if row.get::<_, Option<String>>(0).as_deref() == Some("Lock") { break; }
        assert!(until.elapsed() < Duration::from_secs(5));
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    oracle.query_one("SELECT pg_terminate_backend($1)", &[&pid]).await.unwrap();
    oracle.batch_execute("ROLLBACK").await.unwrap();
    assert!(matches!(worker.await.unwrap(), Err(ImportError::OutcomeUnknown { .. })));
    assert!(matches!(h.console(tab).session, SessionState::Lost { transaction: TransactionLoss::Unknown, .. }));
    assert_eq!(count(&oracle, &table).await, 0);
    oracle.batch_execute(&format!("DROP TABLE {table}")).await.unwrap();
}

#[tokio::test]
async fn csv_import_quotes_identifiers_and_uses_utf8_despite_session_settings() {
    let mut h = Harness::start().await;
    let oracle = h.oracle().await;
    let table = unique("p12_quoted_é_\\'\"");
    let quoted = format!("\"{}\"", table.replace('"', "\"\""));
    oracle.batch_execute(&format!("CREATE TABLE {quoted} (txt text)")).await.unwrap();
    let tab = h.sql_tab(TxnMode::AutoCommit).await;
    let target = import_target(&h, &table).await;
    h.run_ok(tab, "SET standard_conforming_strings = off; SET client_encoding = 'LATIN1'").await;
    let request = csv_request(&h, tab, &target, "txt\n雪\\path\n", true).await;
    assert_eq!(h.engine.table_import(tab, request, None).await.unwrap().rows, 1);
    let row = oracle.query_one(&format!("SELECT txt FROM {quoted}"), &[]).await.unwrap();
    assert_eq!(row.get::<_, String>(0), "雪\\path");
    oracle.batch_execute(&format!("DROP TABLE {quoted}")).await.unwrap();
}

#[tokio::test]
async fn csv_import_preserves_unicode_column_identity_with_a_latin1_session() {
    let mut h = Harness::start().await;
    let oracle = h.oracle().await;
    let table = unique("p12_encoding");
    oracle.batch_execute(&format!("CREATE TABLE {table} (\"é\" text, \"Ã©\" text)")).await.unwrap();
    let tab = h.sql_tab(TxnMode::Manual).await;
    let target = import_target(&h, &table).await;
    h.run_ok(tab, "SET standard_conforming_strings = off; SET client_encoding = 'LATIN1'").await;
    let request = csv_request(&h, tab, &target, "é\n雪\n", true).await;
    assert!(!h.engine.table_import(tab, request, None).await.unwrap().committed);
    assert_eq!(h.scalar(tab, "SHOW client_encoding").await, "LATIN1");
    h.engine.console_commit(tab, None).await.unwrap();
    let row = oracle.query_one(&format!("SELECT \"é\", \"Ã©\" FROM {table}"), &[]).await.unwrap();
    assert_eq!(row.get::<_, Option<String>>(0), Some("雪".into()));
    assert_eq!(row.get::<_, Option<String>>(1), None);
    oracle.batch_execute(&format!("DROP TABLE {table}")).await.unwrap();
}

#[tokio::test]
async fn csv_import_deferred_constraint_rejection_keeps_the_session_reusable() {
    let mut h = Harness::start().await;
    let oracle = h.oracle().await;
    let table = unique("p12_deferred");
    oracle.batch_execute(&format!("CREATE TABLE {table} (n int UNIQUE DEFERRABLE INITIALLY DEFERRED)")).await.unwrap();
    let tab = h.sql_tab(TxnMode::AutoCommit).await;
    let target = import_target(&h, &table).await;
    let request = csv_request(&h, tab, &target, "n\n1\n1\n", true).await;
    let error = h.engine.table_import(tab, request, None).await.unwrap_err();
    assert!(matches!(&error, ImportError::Query { error } if error.sqlstate.as_deref() == Some("23505")), "{error:?}");
    assert!(matches!(h.console(tab).session, SessionState::Open { .. }));
    assert_eq!(h.console(tab).txn, TxnState::Idle);
    assert_eq!(count(&oracle, &table).await, 0);
    h.run_ok(tab, "SELECT 1").await;
    oracle.batch_execute(&format!("DROP TABLE {table}")).await.unwrap();
}

#[tokio::test]
async fn csv_import_lock_wait_cancel_reports_cancellation() {
    let mut h = Harness::start().await;
    let oracle = h.oracle().await;
    let table = unique("p12_lock_cancel");
    oracle.batch_execute(&format!("CREATE TABLE {table} (n int)")).await.unwrap();
    let tab = h.sql_tab(TxnMode::AutoCommit).await;
    let target = import_target(&h, &table).await;
    let request = csv_request(&h, tab, &target, "n\n1\n", true).await;
    oracle.batch_execute(&format!("BEGIN; LOCK TABLE {table} IN ACCESS EXCLUSIVE MODE")).await.unwrap();
    let engine = h.engine.clone();
    let worker = tokio::spawn(async move { engine.table_import(tab, request, None).await });
    let start = Instant::now();
    loop {
        if let SessionState::Open { backend_pid } = h.console(tab).session {
            let waiting: bool = oracle.query_one("SELECT EXISTS (SELECT FROM pg_stat_activity WHERE pid = $1 AND wait_event_type = 'Lock')", &[&backend_pid]).await.unwrap().get(0);
            if waiting { break; }
        }
        assert!(start.elapsed() < Duration::from_secs(5));
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    h.engine.console_cancel(tab).await;
    let result = tokio::time::timeout(Duration::from_secs(5), worker).await.unwrap().unwrap();
    oracle.batch_execute("ROLLBACK").await.unwrap();
    assert!(matches!(result, Err(ImportError::Cancelled)), "{result:?}");
    assert_eq!(count(&oracle, &table).await, 0);
    h.run_ok(tab, "SELECT 1").await;
    oracle.batch_execute(&format!("DROP TABLE {table}")).await.unwrap();
}

#[tokio::test]
async fn csv_import_cancelled_preparation_settles_its_pending_connection() {
    let h = Harness::start().await;
    let oracle = h.oracle().await;
    let table = unique("p12_connect_cancel");
    oracle.batch_execute(&format!("CREATE TABLE {table} (n int)")).await.unwrap();
    let tab = h.sql_tab(TxnMode::AutoCommit).await;
    let target = import_target(&h, &table).await;
    h.engine.disconnect(h.conn, true).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (accepted, received) = tokio::sync::oneshot::channel();
    let (release, released) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        accepted.send(()).unwrap();
        let _ = released.await;
        drop(socket);
    });
    let draft = ConnectionDraft { id: Some(h.conn), host: "127.0.0.1".into(), port, ..draft_from_url(&url(), "dev").unwrap() };
    h.engine.save_connection(draft).await.unwrap();
    let path = h.dir.path().join("connect.csv");
    std::fs::write(&path, "n\n1\n").unwrap();
    let source = h.engine.table_import_prepare(tab, target, path.to_string_lossy().into()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), received).await.unwrap().unwrap();
    h.engine.table_import_discard(tab, source).await.unwrap();
    release.send(()).unwrap();
    server.await.unwrap();
    let settled = tokio::time::timeout(Duration::from_secs(3), h.wait_status(|s| !matches!(s, ConnStatus::Connecting))).await;
    assert!(matches!(settled, Ok(ConnStatus::Failed { .. })), "{settled:?}");
    let retried = tokio::time::timeout(Duration::from_secs(3), h.engine.connect(h.conn)).await;
    assert!(matches!(retried, Ok(Err(_))), "{retried:?}");
    oracle.batch_execute(&format!("DROP TABLE {table}")).await.unwrap();
}

#[tokio::test]
async fn csv_import_round_trips_the_existing_exporter() {
    let mut h = Harness::start().await;
    let oracle = h.oracle().await;
    let table = unique("p12_export");
    let columns = "id,b,n,t,vc,jb,arr_t,m,tstz";
    oracle.batch_execute(&format!("CREATE TABLE {table} AS SELECT {columns} FROM lab.type_zoo WITH NO DATA")).await.unwrap();
    let tab = h.sql_tab(TxnMode::AutoCommit).await;
    let target = import_target(&h, &table).await;
    let state = h.run_ok(tab, &format!("SELECT {columns} FROM lab.type_zoo ORDER BY id")).await;
    let meta = Harness::result(&state, 0);
    let path = h.dir.path().join("round-trip.csv");
    let exported = h.engine.result_export(meta.r#ref, ExportFormat::Csv, ExportScope::Loaded, path.to_string_lossy().into()).await.unwrap();
    let source = h.engine.table_import_prepare(tab, target, path.to_string_lossy().into()).await.unwrap();
    let preview = h.engine.table_import_preview(tab, source, true).await.unwrap();
    let imported = h.engine.table_import(tab, ImportRequest { source, header: true, mapping: preview.mapping }, None).await.unwrap();
    assert_eq!(imported.rows, exported.rows);
    let row = oracle.query_one(&format!("SELECT count(*) FROM ((SELECT {columns} FROM lab.type_zoo EXCEPT ALL SELECT {columns} FROM {table}) UNION ALL (SELECT {columns} FROM {table} EXCEPT ALL SELECT {columns} FROM lab.type_zoo)) differences"), &[]).await.unwrap();
    assert_eq!(row.get::<_, i64>(0), 0);
    oracle.batch_execute(&format!("DROP TABLE {table}")).await.unwrap();
}

#[path = "common/database_map.rs"]
mod database_map_cases;

#[tokio::test]
async fn database_map_pages_declared_neighborhoods() {
    let h = Harness::start().await;
    h.engine.connect(h.conn).await.unwrap();
    let stem = format!("map_{}", uuid::Uuid::new_v4().simple());
    let (oracle, connection) = tokio_postgres::connect(&url(), NoTls).await.unwrap();
    tokio::spawn(connection);
    oracle.batch_execute(&database_map_cases::fixture("public",&stem,DatabaseKind::Postgres)).await.unwrap();
    database_map_cases::verify(&h.engine,h.db(),"public",&stem,DatabaseKind::Postgres).await;
    oracle.batch_execute(&format!("DROP TABLE public.{stem}_fan,public.{stem}_child,public.{stem}_root,public.{stem}_anchor,public.{stem}_alone")).await.unwrap();
}
