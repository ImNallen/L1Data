use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use futures_util::StreamExt;

use crate::console::{self, statement_line, Console, ConsoleInner, Orphaned, Work};
use crate::model::*;
use crate::results::Caps;
use crate::session::{self, QueryEvent, Session};
use crate::sql::StmtSpan;
use crate::writes;
use crate::Shared;

pub(crate) enum ExplainMode {
    Explain,
    Analyze,
}

impl RunMode {
    pub(crate) fn explain(self) -> Option<ExplainMode> {
        match self {
            RunMode::Run => None,
            RunMode::Explain => Some(ExplainMode::Explain),
            RunMode::ExplainAnalyze => Some(ExplainMode::Analyze),
        }
    }
}

#[derive(Debug)]
enum Bracket {
    IdleExplain { body: String },
    IdleAnalyze { body: String },
    ActiveExplain { body: String },
    ActiveAnalyze { body: String },
}

impl Bracket {
    fn new(mode: ExplainMode, txn: TxnState, statement: &str) -> Self {
        let body = match mode {
            ExplainMode::Explain => format!("EXPLAIN (FORMAT JSON) {statement}"),
            ExplainMode::Analyze => format!("EXPLAIN (FORMAT JSON, ANALYZE, BUFFERS) {statement}"),
        };
        match (txn, mode) {
            (TxnState::Idle, ExplainMode::Explain) => Bracket::IdleExplain { body },
            (TxnState::Idle, ExplainMode::Analyze) => Bracket::IdleAnalyze { body },
            (TxnState::Active, ExplainMode::Explain) => Bracket::ActiveExplain { body },
            (TxnState::Active, ExplainMode::Analyze) => Bracket::ActiveAnalyze { body },
            (TxnState::Aborted | TxnState::Open, _) => {
                unreachable!("aborted and open are refused before a bracket")
            }
        }
    }

    fn body(&self) -> &str {
        match self {
            Bracket::IdleExplain { body }
            | Bracket::IdleAnalyze { body }
            | Bracket::ActiveExplain { body }
            | Bracket::ActiveAnalyze { body } => body,
        }
    }

    fn open(&self) -> &str {
        match self {
            Bracket::IdleExplain { .. } => "BEGIN READ ONLY",
            Bracket::IdleAnalyze { .. } => "BEGIN",
            Bracket::ActiveExplain { .. } => "SAVEPOINT l1_explain; SET LOCAL transaction_read_only = on",
            Bracket::ActiveAnalyze { .. } => "SAVEPOINT l1_explain",
        }
    }

    fn close(&self) -> &str {
        match self {
            Bracket::IdleExplain { .. } | Bracket::IdleAnalyze { .. } => "ROLLBACK",
            Bracket::ActiveExplain { .. } | Bracket::ActiveAnalyze { .. } => {
                "ROLLBACK TO SAVEPOINT l1_explain; RELEASE SAVEPOINT l1_explain"
            }
        }
    }

    fn restores(&self) -> TxnState {
        match self {
            Bracket::IdleExplain { .. } | Bracket::IdleAnalyze { .. } => TxnState::Idle,
            Bracket::ActiveExplain { .. } | Bracket::ActiveAnalyze { .. } => TxnState::Active,
        }
    }

    fn sent(&self) -> [&str; 3] {
        [self.open(), self.body(), self.close()]
    }
}

#[derive(Debug)]
pub(crate) struct Prepared {
    bracket: Bracket,
    checked: writes::WritesChecked,
    origin: Option<DocOffset>,
    prefix: u32,
}

pub(crate) fn prepare(
    mode: ExplainMode,
    kind: DatabaseKind,
    txn: TxnState,
    text: &str,
    spans: &[StmtSpan],
    target: &writes::Target,
    confirmed: Option<&WriteDigest>,
) -> Result<Prepared, RunError> {
    if kind != DatabaseKind::Postgres {
        return Err(unsupported("Explain is only for PostgreSQL."));
    }
    if spans.is_empty() {
        return Err(RunError::NoStatement);
    }
    if spans.len() != 1 {
        return Err(unsupported("Explain one statement."));
    }
    let span = StmtSpan { start: spans[0].start, end: spans[0].end };
    let statement = &text[span.start..span.end];
    if starts_with_explain(statement) {
        return Err(unsupported("The statement already starts with EXPLAIN."));
    }
    match txn {
        TxnState::Aborted => return Err(unsupported("The transaction must be rolled back first.")),
        TxnState::Open => return Err(unsupported("Explain is only for PostgreSQL.")),
        TxnState::Idle | TxnState::Active => {}
    }
    let bracket = Bracket::new(mode, txn, statement);
    let sent = bracket.sent();
    let origin = crate::text::OffsetMap::new(text).to_utf16(span.start);
    let prefix = prefix_units(bracket.body(), statement);
    match writes::check(target, &sent, confirmed) {
        Ok(checked) => Ok(Prepared { bracket, checked, origin: Some(origin), prefix }),
        Err(confirmation) => Err(RunError::ConfirmWrites { confirmation }),
    }
}

fn prefix_units(body: &str, statement: &str) -> u32 {
    let prefix = body.len() - statement.len();
    body[..prefix].encode_utf16().count() as u32
}

fn in_document(mut error: QueryError, origin: Option<DocOffset>, prefix: u32) -> QueryError {
    if let (Some(position), Some(origin)) = (error.position, origin) {
        let within = position.0.saturating_sub(prefix);
        error.position = Some(DocOffset(origin.0.saturating_add(within)));
    } else {
        error.position = None;
    }
    error
}

impl Prepared {
    pub(crate) fn source(mut self, origin: DocOffset, changed: bool) -> Self {
        self.origin = (!changed).then_some(origin);
        self
    }
}

fn unsupported(what: &str) -> RunError {
    RunError::Unsupported { what: what.to_string() }
}

fn starts_with_explain(sql: &str) -> bool {
    let rest = skip_noise(sql);
    let Some(head) = rest.get(..7) else {
        return false;
    };
    if !head.eq_ignore_ascii_case("explain") {
        return false;
    }
    !rest.as_bytes().get(7).copied().is_some_and(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn skip_noise(sql: &str) -> &str {
    let mut rest = sql;
    loop {
        let trimmed = rest.trim_start();
        if let Some(comment) = trimmed.strip_prefix("--") {
            rest = match comment.find('\n') {
                Some(index) => &comment[index + 1..],
                None => "",
            };
            continue;
        }
        if let Some(comment) = trimmed.strip_prefix("/*") {
            rest = match comment.find("*/") {
                Some(index) => &comment[index + 2..],
                None => return "",
            };
            continue;
        }
        return trimmed;
    }
}

pub(crate) struct Launch {
    session: Session,
    stop: Arc<AtomicBool>,
    bracket: Bracket,
    schema: Option<String>,
    run_id: RunId,
    connection: ConnectionConfig,
    database: String,
    entered: TxnState,
    origin: Option<DocOffset>,
    prefix: u32,
}

pub(crate) fn launch(
    console: &Console,
    inner: &mut ConsoleInner,
    run_id: RunId,
    prepared: Prepared,
    schema: Option<String>,
    connection: ConnectionConfig,
    database: String,
    entered: TxnState,
) -> Result<Launch, RunError> {
    let Prepared { bracket, checked, origin, prefix } = prepared;
    let Some((session, stop)) = console.take_session(inner, run_id, Work::Run(checked)) else {
        return Err(RunError::Busy);
    };
    Ok(Launch { session, stop, bracket, schema, run_id, connection, database, entered, origin, prefix })
}

pub(crate) async fn run(console: Arc<Console>, shared: Arc<Shared>, launch: Launch) -> Result<(), Orphaned> {
    let Launch { mut session, stop, bracket, schema, run_id, connection, database, entered, origin, prefix } = launch;
    let body = bracket.body().to_owned();
    let restored = bracket.restores();
    let started = Instant::now();
    let settings = shared.settings();
    let caps = Caps { max_rows: settings.max_rows, max_bytes: settings.max_result_bytes };

    if let Err(error) = session.use_schema(schema.as_deref(), entered).await {
        return finish(&console, &shared, session, run_id, &body, &connection, &database, started, entered, None, failed(error)).await;
    }
    if let Err(error) = session.batch(bracket.open()).await {
        return finish(&console, &shared, session, run_id, &body, &connection, &database, started, entered, None, failed(error)).await;
    }
    console.output(&shared, run_id, statement_line(0, &body));
    let read = read_plan(&mut session, &body, &stop).await;
    console.output_all(&shared, run_id, console::notice_lines(&mut session));
    if let Err(error) = session.batch(bracket.close()).await {
        let lost = Some(error.message.clone());
        let end = match read {
            Read::Cancelled => cancelled(),
            Read::Failed(error) => failed(in_document(error, origin, prefix)),
            Read::Ready { .. } => failed(error),
        };
        return finish(&console, &shared, session, run_id, &body, &connection, &database, started, entered, lost, end).await;
    }
    match read {
        Read::Cancelled => finish(&console, &shared, session, run_id, &body, &connection, &database, started, restored, None, cancelled()).await,
        Read::Failed(error) => finish(&console, &shared, session, run_id, &body, &connection, &database, started, restored, None, failed(in_document(error, origin, prefix))).await,
        Read::Ready { cell, columns } => {
            let output = match store_plan(&console, run_id, &schema, &connection, &database, &cell, columns, &caps, started) {
                Ok(output) => output,
                Err(error) => return finish(&console, &shared, session, run_id, &body, &connection, &database, started, restored, None, failed(error)).await,
            };
            finish(&console, &shared, session, run_id, &body, &connection, &database, started, restored, None, ok_plan(output)).await
        }
    }
}

enum Read {
    Ready { cell: String, columns: Vec<ColumnMeta> },
    Failed(QueryError),
    Cancelled,
}

async fn read_plan(session: &mut Session, sql: &str, stop: &AtomicBool) -> Read {
    let mut stream = session.query(sql);
    let mut columns = None;
    let mut cell = None;
    let mut failure = None;
    while let Some(event) = stream.next().await {
        match event {
            Err(error) => {
                failure = Some(error);
                break;
            }
            Ok(QueryEvent::Prepared) if stop.load(Ordering::Relaxed) => {
                failure = Some(session::error("Cancelled"));
                break;
            }
            Ok(QueryEvent::Prepared) | Ok(QueryEvent::Notice(_)) | Ok(QueryEvent::ServerDone) => {}
            Ok(QueryEvent::Columns(next)) => columns = Some(next),
            Ok(QueryEvent::Row(row)) if cell.is_none() => cell = row.into_iter().next().flatten(),
            Ok(QueryEvent::Row(_)) | Ok(QueryEvent::Complete { .. }) => {}
            Ok(QueryEvent::ServerError(error)) => {
                failure = Some(error);
                break;
            }
        }
    }
    drop(stream);
    if stop.load(Ordering::Relaxed) && cell.is_none() {
        return Read::Cancelled;
    }
    if let Some(error) = failure {
        return if stop.load(Ordering::Relaxed) { Read::Cancelled } else { Read::Failed(error) };
    }
    let Some(mut columns) = columns.filter(|columns| !columns.is_empty()) else {
        return Read::Failed(session::error("Driver returned a row without result metadata"));
    };
    let Some(cell) = cell else {
        return Read::Failed(session::error("The result is not a plan."));
    };
    columns.truncate(1);
    Read::Ready { cell, columns }
}

fn store_plan(
    console: &Console,
    run_id: RunId,
    schema: &Option<String>,
    connection: &ConnectionConfig,
    database: &str,
    cell: &str,
    columns: Vec<ColumnMeta>,
    caps: &Caps,
    started: Instant,
) -> Result<RunOutput, QueryError> {
    let (set, output) = crate::results::plan_output(
        console.tab,
        run_id,
        DatabaseKind::Postgres,
        PlanFormat::Json,
        schema,
        connection.id,
        database,
        cell,
        columns,
        caps,
        started,
    )?;
    console.results.lock().unwrap().push(set);
    Ok(output)
}

struct End {
    outcome: RunOutcome,
    outputs: Vec<RunOutput>,
    error: Option<QueryError>,
}

fn failed(error: QueryError) -> End {
    End { outcome: RunOutcome::Failed, outputs: Vec::new(), error: Some(error) }
}

fn cancelled() -> End {
    End { outcome: RunOutcome::Cancelled, outputs: Vec::new(), error: None }
}

fn ok_plan(output: RunOutput) -> End {
    End { outcome: RunOutcome::Ok, outputs: vec![output], error: None }
}

async fn finish(
    console: &Console,
    shared: &Arc<Shared>,
    session: Session,
    run_id: RunId,
    body: &str,
    connection: &ConnectionConfig,
    database: &str,
    started: Instant,
    txn: TxnState,
    lost: Option<String>,
    end: End,
) -> Result<(), Orphaned> {
    let plan_end = match end {
        End { error: Some(error), .. } => console::PlanEnd::Failed(error),
        End { outcome: RunOutcome::Cancelled, .. } => console::PlanEnd::Cancelled,
        End { outputs, .. } => console::PlanEnd::Ready(outputs.into_iter().next().expect("a finished explain stores one plan")),
    };
    console::finish_plan(console, shared, connection, database, console::PlanFinish { session, run_id, sql: body.to_owned(), started, txn, lost, end: plan_end }).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn span_of(text: &str) -> StmtSpan {
        StmtSpan { start: 0, end: text.len() }
    }

    struct Prod {
        saved: ConnectionConfig,
        console: ConsoleConfigView,
        unasked: WriteList,
    }

    impl Prod {
        fn new() -> Self {
            let id = ConnectionId(Uuid::from_u128(1));
            Self {
                saved: ConnectionConfig {
                    database_kind: DatabaseKind::Postgres,
                    id,
                    name: "prod".into(),
                    host: "db".into(),
                    port: 5432,
                    database: "shop".into(),
                    user: "app".into(),
                    ssl_mode: SslMode::Prefer,
                    ca_cert_path: None,
                    has_password: false,
                    color: None,
                    ssh: None,
                    environment: Some(Environment::Production),
                    sql_server_auth: None,
                },
                console: ConsoleConfigView { connection_id: id, database: "shop".into(), schema: None, mode: TxnMode::AutoCommit },
                unasked: WriteList::default(),
            }
        }

        fn target(&self) -> writes::Target<'_> {
            writes::Target { saved: &self.saved, opened: &self.saved, console: &self.console, txn: TxnState::Idle, unasked: &self.unasked }
        }
    }

    #[test]
    fn four_brackets_send_these_statements() {
        let text = "select 1";
        let idle_explain = Bracket::new(ExplainMode::Explain, TxnState::Idle, text);
        assert_eq!(idle_explain.sent(), ["BEGIN READ ONLY", "EXPLAIN (FORMAT JSON) select 1", "ROLLBACK",]);
        assert_eq!(idle_explain.restores(), TxnState::Idle);

        let idle_analyze = Bracket::new(ExplainMode::Analyze, TxnState::Idle, text);
        assert_eq!(idle_analyze.sent(), ["BEGIN", "EXPLAIN (FORMAT JSON, ANALYZE, BUFFERS) select 1", "ROLLBACK",]);
        assert_eq!(idle_analyze.restores(), TxnState::Idle);

        let active_explain = Bracket::new(ExplainMode::Explain, TxnState::Active, text);
        assert_eq!(
            active_explain.sent(),
            [
                "SAVEPOINT l1_explain; SET LOCAL transaction_read_only = on",
                "EXPLAIN (FORMAT JSON) select 1",
                "ROLLBACK TO SAVEPOINT l1_explain; RELEASE SAVEPOINT l1_explain",
            ]
        );
        assert_eq!(active_explain.restores(), TxnState::Active);

        let active_analyze = Bracket::new(ExplainMode::Analyze, TxnState::Active, text);
        assert_eq!(
            active_analyze.sent(),
            [
                "SAVEPOINT l1_explain",
                "EXPLAIN (FORMAT JSON, ANALYZE, BUFFERS) select 1",
                "ROLLBACK TO SAVEPOINT l1_explain; RELEASE SAVEPOINT l1_explain",
            ]
        );
        assert_eq!(active_analyze.restores(), TxnState::Active);
    }

    fn expect_unsupported(err: RunError, what: &str) {
        match err {
            RunError::Unsupported { what: got } => assert_eq!(got, what),
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    #[test]
    fn refusals() {
        let select = "select 1";
        let one = [span_of(select)];
        let prod = Prod::new();
        let target = prod.target();

        let aborted = prepare(ExplainMode::Explain, DatabaseKind::Postgres, TxnState::Aborted, select, &one, &target, None).unwrap_err();
        expect_unsupported(aborted, "The transaction must be rolled back first.");

        let two = [StmtSpan { start: 0, end: 6 }, StmtSpan { start: 6, end: select.len() }];
        let several = prepare(ExplainMode::Explain, DatabaseKind::Postgres, TxnState::Idle, select, &two, &target, None).unwrap_err();
        expect_unsupported(several, "Explain one statement.");

        let explained = "/* c */ explain select 1";
        let already = prepare(ExplainMode::Explain, DatabaseKind::Postgres, TxnState::Idle, explained, &[span_of(explained)], &target, None).unwrap_err();
        expect_unsupported(already, "The statement already starts with EXPLAIN.");

        let sql_server = prepare(ExplainMode::Analyze, DatabaseKind::SqlServer, TxnState::Idle, select, &one, &target, None).unwrap_err();
        expect_unsupported(sql_server, "Explain is only for PostgreSQL.");

        let none = prepare(ExplainMode::Explain, DatabaseKind::Postgres, TxnState::Idle, select, &[], &target, None).unwrap_err();
        assert!(matches!(none, RunError::NoStatement), "got {none:?}");

        let open = prepare(ExplainMode::Explain, DatabaseKind::Postgres, TxnState::Open, select, &one, &target, None).unwrap_err();
        expect_unsupported(open, "Explain is only for PostgreSQL.");
    }

    #[test]
    fn production_update_analyze_confirms_and_explain_does_not() {
        let text = "update t set n = 1";
        let spans = [span_of(text)];
        let prod = Prod::new();
        let target = prod.target();
        let analyze = prepare(ExplainMode::Analyze, DatabaseKind::Postgres, TxnState::Idle, text, &spans, &target, None);
        let Err(RunError::ConfirmWrites { confirmation }) = analyze else {
            panic!("analyze update should confirm, got {analyze:?}");
        };
        let explain = prepare(ExplainMode::Explain, DatabaseKind::Postgres, TxnState::Idle, text, &spans, &target, None);
        assert!(explain.is_ok(), "explain update should pass: {explain:?}");

        let digest = confirmation.digest.clone();
        let active = prepare(ExplainMode::Analyze, DatabaseKind::Postgres, TxnState::Active, text, &spans, &target, Some(&digest));
        assert!(matches!(active, Err(RunError::ConfirmWrites { .. })), "idle digest must not accept active analyze, got {active:?}");
    }

    #[test]
    fn a_statement_error_points_at_the_document() {
        let text = "select 1; select '你', missing_column";
        let start = text.find("select '你'").unwrap();
        let statement = &text[start..];
        let map = crate::text::OffsetMap::new(text);
        let expected = map.to_utf16(text.find("missing_column").unwrap());
        for mode in [ExplainMode::Explain, ExplainMode::Analyze] {
            let body = Bracket::new(mode, TxnState::Idle, statement).body().to_owned();
            let prefix = prefix_units(&body, statement);
            let at = body.find("missing_column").unwrap();
            let n = u32::try_from(body[..at].chars().count()).unwrap() + 1;
            let seen = crate::text::OffsetMap::new(&body).pg_position(&StmtSpan { start: 0, end: body.len() }, n);
            let error = QueryError {
                severity: "ERROR".into(),
                sqlstate: Some("42703".into()),
                message: "column missing_column does not exist".into(),
                detail: None,
                hint: None,
                context: None,
                position: Some(seen),
            };
            assert_eq!(in_document(error, Some(map.to_utf16(start)), prefix).position, Some(expected));
        }
    }
}
