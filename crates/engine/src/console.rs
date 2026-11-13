use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use futures_util::{Stream, StreamExt};
use tokio::sync::{Mutex, watch};

use crate::Shared;
use crate::model::*;
use crate::results::{Caps, Origin, Rerun, ResultSet};
use crate::session::{CancelHandle, QueryEvent, Session};
use crate::sql::{Invalidates, StmtKind, StmtSpan};
use crate::store::HistoryInsert;
use crate::text::synthesize_tag;
use crate::txn::{StmtOutcome, TransactionPlan, txn_after};
use crate::writes::{self, ReadOnly, WritesChecked};

pub(crate) const STREAM_EMIT_INTERVAL: Duration = Duration::from_millis(100);
const OUTPUT_COALESCE: Duration = Duration::from_millis(5);

pub(crate) struct OutputBatch<'a> {
    console: &'a Console,
    shared: &'a Shared,
    run_id: RunId,
    lines: Vec<OutputBody>,
}

impl<'a> OutputBatch<'a> {
    pub fn new(console: &'a Console, shared: &'a Shared, run_id: RunId) -> Self {
        Self { console, shared, run_id, lines: Vec::new() }
    }

    pub fn push(&mut self, body: OutputBody) {
        self.lines.push(body);
    }

    pub fn send(&mut self) {
        self.console.output_all(self.shared, self.run_id, std::mem::take(&mut self.lines));
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    pub async fn wait<S: Stream + Unpin>(&mut self, stream: &mut S) -> Option<S::Item> {
        match tokio::time::timeout(OUTPUT_COALESCE, stream.next()).await {
            Ok(next) => next,
            Err(_) => {
                self.send();
                stream.next().await
            }
        }
    }
}

pub struct Console {
    pub tab: TabId,
    pub inner: Mutex<ConsoleInner>,
    pub results: parking_lot::Mutex<Vec<Arc<ResultSet>>>,
    pub latest: parking_lot::Mutex<ConsoleState>,
    pub closed: AtomicBool,
}

pub struct ConsoleInner {
    pub import: Option<crate::import::Draft>,
    pub config: ConsoleConfigView,
    pub session: SessionSlot,
    pub txn: TxnState,
    pub txn_depth: Option<u32>,
    pub unasked: WriteList,
    /// Outlives the session, so a statement a fence refused keeps asking on this tab.
    pub proven: writes::Proven,
    pub run: RunState,
    pub rev: u64,
    refreshes: Option<PendingRefreshes>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Owed {
    pub catalog: bool,
    pub databases: bool,
}

impl Owed {
    pub fn add(&mut self, invalidates: Invalidates) {
        match invalidates {
            Invalidates::Nothing => {}
            Invalidates::Catalog => self.catalog = true,
            Invalidates::DatabaseList => self.databases = true,
            Invalidates::Both => {
                self.catalog = true;
                self.databases = true;
            }
        }
    }
}

struct PendingRefreshes {
    db: DatabaseRef,
    policy: RefreshPolicy,
    owed: Owed,
}

#[derive(PartialEq, Eq)]
enum RefreshPolicy {
    Commit,
    NativeEnd,
}

impl ConsoleInner {
    pub(crate) fn invalidate(&mut self, database_kind: DatabaseKind, invalidates: Invalidates) {
        if invalidates == Invalidates::Nothing {
            return;
        }
        let db = self.config.db();
        let pending = self.refreshes.get_or_insert_with(|| PendingRefreshes {
            db,
            policy: match database_kind {
                DatabaseKind::Postgres => RefreshPolicy::Commit,
                DatabaseKind::SqlServer => RefreshPolicy::NativeEnd,
            },
            owed: Owed::default(),
        });
        pending.owed.add(invalidates);
    }

    /// The running export's progress. Reached only through `Console::edit`, which checks that the
    /// caller's run id still owns the slot.
    pub fn export_progress_mut(&mut self) -> Option<&mut ExportProgress> {
        match &mut self.session {
            SessionSlot::Busy { work: Work::Export(progress), .. } => Some(progress),
            _ => None,
        }
    }

    /// Every way a session leaves the tab goes through here, so its transaction state and held writes leave with it.
    pub fn end_session(&mut self, next: SessionSlot) -> SessionSlot {
        self.import = None;
        self.txn = TxnState::Idle;
        self.txn_depth = None;
        self.unasked = WriteList::default();
        std::mem::replace(&mut self.session, next)
    }

    /// Finishes a run whose task will not.
    fn end_run(&mut self, in_flight: StmtStatus, rows: RowsState, outcome: RunOutcome) -> Option<u16> {
        let RunState::Running { run_id, started_at_ms, statements } = &mut self.run else { return None };
        let mut stopped = None;
        for s in statements.iter_mut() {
            if matches!(s.status, StmtStatus::Pending | StmtStatus::Preparing | StmtStatus::Running) {
                for output in &mut s.outputs {
                    if let RunOutput::Rows { state, .. } = output
                        && *state == RowsState::Streaming
                    {
                        *state = rows;
                    }
                }
                s.status = if stopped.is_none() { in_flight.clone() } else { StmtStatus::Skipped };
                stopped.get_or_insert(s.index);
            }
        }
        self.run = RunState::Finished {
            run_id: *run_id,
            elapsed_ms: (now_ms() - *started_at_ms).clamp(0, u32::MAX as i64) as u32,
            statements: std::mem::take(statements),
            outcome,
        };
        stopped
    }

    pub fn finish_refreshes(&mut self, committed: bool) -> Option<(DatabaseRef, Owed)> {
        if self.txn != TxnState::Idle {
            return None;
        }
        self.end_refreshes(committed)
    }

    /// A chained `COMMIT` or `ROLLBACK` ends the transaction that owes the refreshes while the next one opens.
    fn end_refreshes(&mut self, committed: bool) -> Option<(DatabaseRef, Owed)> {
        let pending = self.refreshes.take()?;
        // Native batches may have committed DDL before opening the transaction that ended.
        (committed || pending.policy == RefreshPolicy::NativeEnd).then_some((pending.db, pending.owed))
    }
}

/// What holds a tab's session while it is out of its slot. `ConsoleState.work` and
/// `ConsoleState.export` both derive from this one value, so an export's progress leaves the
/// snapshot on every path out of `Busy`: return, close, and disconnect.
pub enum Work {
    Run(WritesChecked),
    Count,
    Commit(WritesChecked),
    Rollback,
    Edit(WritesChecked),
    Probe,
    Export(ExportProgress),
    Import { checked: WritesChecked, progress: ImportProgress },
}

impl Work {
    pub fn kind(&self) -> ConsoleWork {
        match self {
            Work::Run(_) => ConsoleWork::Run,
            Work::Count => ConsoleWork::Count,
            Work::Commit(_) | Work::Rollback => ConsoleWork::Transaction,
            Work::Edit(_) => ConsoleWork::Edit,
            Work::Probe => ConsoleWork::Probe,
            Work::Export(_) => ConsoleWork::Export,
            Work::Import { .. } => ConsoleWork::Import,
        }
    }
}

pub struct IdleSession {
    session: Session,
    opened: ConnectionConfig,
}

impl IdleSession {
    pub fn new(session: Session, opened: ConnectionConfig) -> Self {
        Self { session, opened }
    }

    pub fn opened(&self) -> &ConnectionConfig {
        &self.opened
    }

    pub fn database_kind(&self) -> DatabaseKind {
        self.session.database_kind()
    }
}

pub enum SessionSlot {
    Empty,
    Idle(IdleSession),
    Busy {
        opened: ConnectionConfig,
        run_id: RunId,
        work: Work,
        backend_pid: i32,
        cancel: Option<CancelHandle>,
        stop: Arc<AtomicBool>,
        session_release: watch::Receiver<Option<String>>,
    },
    Lost {
        reason: String,
        transaction: TransactionLoss,
    },
}

/// Disconnect or close reassigned the slot while a task held the session. The task's edits no
/// longer apply, and it must stop.
pub struct Orphaned;

impl Console {
    pub fn new(tab: TabId, config: ConsoleConfigView) -> Self {
        let inner = ConsoleInner {
            import: None,
            config,
            session: SessionSlot::Empty,
            txn: TxnState::Idle,
            txn_depth: None,
            unasked: WriteList::default(),
            proven: writes::Proven::default(),
            run: RunState::Idle,
            rev: 0,
            refreshes: None,
        };
        let latest = ConsoleState {
            tab_id: tab,
            rev: 0,
            config: inner.config.clone(),
            session: SessionState::None,
            work: None,
            export: None,
            import: None,
            txn: TxnState::Idle,
            txn_depth: None,
            run: RunState::Idle,
        };
        Self {
            tab,
            inner: Mutex::new(inner),
            results: parking_lot::Mutex::new(Vec::new()),
            latest: parking_lot::Mutex::new(latest),
            closed: AtomicBool::new(false),
        }
    }

    pub fn result_set(&self, r: ResultRef) -> Option<Arc<ResultSet>> {
        self.results.lock().iter().find(|s| s.r#ref == r).cloned()
    }

    pub fn snapshot(&self, inner: &ConsoleInner) -> ConsoleState {
        ConsoleState {
            tab_id: self.tab,
            rev: inner.rev,
            config: inner.config.clone(),
            session: match &inner.session {
                SessionSlot::Empty => SessionState::None,
                SessionSlot::Idle(idle) => SessionState::Open { backend_pid: idle.session.backend_pid },
                SessionSlot::Busy { backend_pid, .. } => SessionState::Open { backend_pid: *backend_pid },
                SessionSlot::Lost { reason, transaction } => {
                    SessionState::Lost { reason: reason.clone(), transaction: *transaction }
                }
            },
            txn: inner.txn,
            work: match &inner.session {
                SessionSlot::Busy { work, .. } => Some(work.kind()),
                _ => None,
            },
            export: match &inner.session {
                SessionSlot::Busy { work: Work::Export(progress), .. } => Some(progress.clone()),
                _ => None,
            },
            import: match &inner.session {
                SessionSlot::Busy { work: Work::Import { progress, .. }, .. } => Some(progress.clone()),
                _ => None,
            },
            txn_depth: inner.txn_depth,
            run: inner.run.clone(),
        }
    }

    pub fn publish(&self, inner: &mut ConsoleInner, shared: &Shared) -> ConsoleState {
        inner.rev += 1;
        let state = self.snapshot(inner);
        *self.latest.lock() = state.clone();
        if !self.closed.load(Ordering::Relaxed) {
            shared.emit(AppEvent::ConsoleChanged(state.clone()));
        }
        state
    }

    pub fn output(&self, shared: &Shared, run_id: RunId, body: OutputBody) {
        self.output_all(shared, run_id, vec![body]);
    }

    pub fn output_all(&self, shared: &Shared, run_id: RunId, bodies: Vec<OutputBody>) {
        if bodies.is_empty() || self.closed.load(Ordering::Relaxed) {
            return;
        }
        let at_ms = now_ms();
        shared.emit(AppEvent::ConsoleOutput {
            tab_id: self.tab,
            entries: bodies.into_iter().map(|body| OutputEntry { at_ms, run_id, body }).collect(),
        });
    }

    pub fn take_session(
        &self,
        inner: &mut ConsoleInner,
        run_id: RunId,
        work: Work,
    ) -> Option<(Session, Arc<AtomicBool>)> {
        let SessionSlot::Idle(_) = inner.session else { return None };
        let SessionSlot::Idle(IdleSession { session, opened }) =
            std::mem::replace(&mut inner.session, SessionSlot::Empty)
        else {
            unreachable!()
        };
        if let Work::Run(checked) | Work::Commit(checked) | Work::Edit(checked) | Work::Import { checked, .. } = &work {
            checked.mark_held_writes_asked(&mut inner.unasked);
        }
        let stop = Arc::new(AtomicBool::new(false));
        let cancel = (!(matches!(work, Work::Import { .. }) && session.database_kind() == DatabaseKind::SqlServer))
            .then(|| session.cancel_handle());
        inner.session = SessionSlot::Busy {
            opened,
            run_id,
            work,
            backend_pid: session.backend_pid,
            cancel,
            stop: stop.clone(),
            session_release: session.closed_watch(),
        };
        Some((session, stop))
    }

    /// The one way a task that took the session edits console state. The edit applies and is
    /// published only while `run_id` still owns the slot; otherwise the task is `Orphaned`.
    pub async fn edit<T>(
        &self,
        shared: &Shared,
        run_id: RunId,
        edit: impl FnOnce(&mut ConsoleInner) -> T,
    ) -> Result<T, Orphaned> {
        let mut inner = self.inner.lock().await;
        if !matches!(inner.session, SessionSlot::Busy { run_id: r, .. } if r == run_id) {
            return Err(Orphaned);
        }
        let out = edit(&mut inner);
        self.publish(&mut inner, shared);
        Ok(out)
    }

    /// Puts the session back after a run or a transaction command and applies its final `txn`.
    pub fn return_session(
        inner: &mut ConsoleInner,
        session: Session,
        txn: TxnState,
        lost: Option<String>,
    ) -> Option<(DatabaseRef, Owed)> {
        let SessionSlot::Busy { opened, .. } = std::mem::replace(&mut inner.session, SessionSlot::Empty) else {
            unreachable!("Console::edit holds the slot")
        };
        inner.txn = txn;
        inner.txn_depth = session.txn_depth();
        match lost {
            Some(reason) => {
                let transaction = session.transaction_loss(txn);
                inner.end_session(SessionSlot::Lost { reason, transaction });
                inner.finish_refreshes(false)
            }
            None => {
                inner.session = SessionSlot::Idle(IdleSession { session, opened });
                None
            }
        }
    }

    /// Cancels any run and drops the session; the console itself stays. A task holding the
    /// session finds itself `Orphaned` at its next edit, so the run is finished here: the
    /// statement in flight is Cancelled and the rest Skipped.
    pub async fn drop_session(&self, shared: &Arc<Shared>) {
        let mut inner = self.inner.lock().await;
        let session = inner.end_session(SessionSlot::Empty);
        if let SessionSlot::Busy { stop, .. } = &session {
            stop.store(true, Ordering::Relaxed);
        }
        let due = inner.finish_refreshes(false);
        inner.end_run(StmtStatus::Cancelled, RowsState::Cancelled, RunOutcome::Cancelled);
        self.publish(&mut inner, shared);
        drop(inner);
        match session {
            SessionSlot::Busy { cancel, mut session_release, .. } => {
                let worker = shared.clone();
                shared.spawn(async move {
                    if let Some(cancel) = cancel {
                        cancel.cancel().await;
                    }
                    if let Some((db, owed)) = due {
                        while session_release.changed().await.is_ok() {}
                        worker.schedule_refreshes(db, owed);
                    }
                });
            }
            session => {
                drop(session);
                if let Some((db, owed)) = due {
                    shared.schedule_refreshes(db, owed);
                }
            }
        }
    }

    /// Ends the work of a task that panicked while it held the session. The session died with the
    /// task, so the slot is `Lost` and the next run opens a new one.
    pub async fn fail_task(&self, shared: &Arc<Shared>, run_id: RunId, message: String) {
        let error = crate::session::error(message.clone());
        let edited = self
            .edit(shared, run_id, |inner| {
                let statement =
                    inner.end_run(StmtStatus::Failed { error: error.clone() }, RowsState::Failed, RunOutcome::Failed);
                let held =
                    inner.end_session(SessionSlot::Lost { reason: message, transaction: TransactionLoss::Unknown });
                (statement, held, inner.finish_refreshes(false))
            })
            .await;
        let Ok((statement, held, due)) = edited else { return };
        self.output(shared, run_id, OutputBody::Error { statement, error });
        if let Some((db, owed)) = due
            && let SessionSlot::Busy { mut session_release, .. } = held
        {
            while session_release.changed().await.is_ok() {}
            shared.schedule_refreshes(db, owed);
        }
    }

    pub fn statements_mut(inner: &mut ConsoleInner) -> &mut Vec<StatementRun> {
        match &mut inner.run {
            RunState::Running { statements, .. } | RunState::Finished { statements, .. } => statements,
            RunState::Idle => unreachable!("run state is set before the run task starts"),
        }
    }
}

pub struct RunPlan {
    pub run_id: RunId,
    pub text: String,
    pub statements: Vec<PlannedStmt>,
    pub txn: TxnState,
    pub schema: Option<String>,
    /// The database the session is on; catalog refreshes and history rows name it.
    pub database: String,
    pub connection: ConnectionConfig,
    pub page: Option<TablePage>,
    pub writes_stay_unasked: bool,
    pub read_only: ReadOnly,
}

pub struct TablePage {
    pub relation: Option<Relation>,
    pub rerun: Rerun,
}

pub struct PlannedStmt {
    pub span: StmtSpan,
    pub origin: DocOffset,
    pub changed: bool,
    pub kind: StmtKind,
    pub transaction: TransactionPlan,
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

fn elapsed_ms(since: Instant) -> u32 {
    since.elapsed().as_millis().min(u32::MAX as u128) as u32
}

pub(crate) enum PlanEnd {
    Failed(QueryError),
    Cancelled,
    Ready(RunOutput),
}

pub(crate) struct PlanFinish {
    pub session: Session,
    pub run_id: RunId,
    pub sql: String,
    pub started: Instant,
    pub txn: TxnState,
    pub lost: Option<String>,
    pub end: PlanEnd,
}

pub(crate) async fn finish_plan(
    console: &Console,
    shared: &Arc<Shared>,
    connection: &ConnectionConfig,
    database: &str,
    finish: PlanFinish,
) -> Result<(), Orphaned> {
    let PlanFinish { session, run_id, sql, started, txn, lost, end } = finish;
    if let Some(reason) = &lost {
        console.output(shared, run_id, loss_line(&session, txn, reason));
    }
    let (status, outcome, outputs, history, error) = match end {
        PlanEnd::Failed(error) => (
            StmtStatus::Failed { error: error.clone() },
            RunOutcome::Failed,
            Vec::new(),
            HistoryOutcome::Error { message: error.message.clone() },
            Some(error),
        ),
        PlanEnd::Cancelled => {
            (StmtStatus::Cancelled, RunOutcome::Cancelled, Vec::new(), HistoryOutcome::Cancelled, None)
        }
        PlanEnd::Ready(output) => {
            let rows = match &output {
                RunOutput::Plan { result, .. } => result.row_count,
                _ => 0,
            };
            (StmtStatus::Finished, RunOutcome::Ok, vec![output], HistoryOutcome::Rows { rows: u64::from(rows) }, None)
        }
    };
    if let Some(error) = &error {
        console.output(shared, run_id, OutputBody::Error { statement: Some(0), error: error.clone() });
    }
    let elapsed = elapsed_ms(started);
    let due = console
        .edit(shared, run_id, |inner| {
            let statements = Console::statements_mut(inner);
            if let Some(statement) = statements.get_mut(0) {
                statement.status = status;
                statement.outputs = outputs;
            }
            let due = Console::return_session(inner, session, txn, lost);
            let due =
                if matches!(outcome, RunOutcome::Ok) { due.or_else(|| inner.finish_refreshes(true)) } else { due };
            let statements = Console::statements_mut(inner).clone();
            inner.run = RunState::Finished { run_id, elapsed_ms: elapsed, statements, outcome };
            due
        })
        .await?;
    if let Some((db, owed)) = due {
        shared.schedule_refreshes(db, owed);
    }
    shared.record_history(HistoryInsert {
        database_kind: connection.database_kind,
        at_ms: now_ms(),
        connection_id: connection.id,
        connection_name: connection.name.clone(),
        database: database.to_owned(),
        sql,
        elapsed_ms: elapsed,
        outcome: history,
    });
    Ok(())
}

/// Every console write goes through `Console::edit`, so an orphaned task returns at its next
/// write and drops the session with it.
pub async fn run_task(
    console: Arc<Console>,
    shared: Arc<Shared>,
    mut session: Session,
    plan: RunPlan,
    stop: Arc<AtomicBool>,
) -> Result<(), Orphaned> {
    let started = Instant::now();
    let run_id = plan.run_id;
    let settings = shared.settings();
    let caps = Caps { max_rows: settings.max_rows, max_bytes: settings.max_result_bytes };
    let mut txn = plan.txn;
    let mut outcome = RunOutcome::Ok;
    let mut fatal = None;
    let mut index = 0;
    while index < plan.statements.len() {
        let stmt = &plan.statements[index];
        let i = index as u16;
        if stop.load(Ordering::Relaxed) {
            set_status(&console, &shared, run_id, i, StmtStatus::Cancelled).await?;
            outcome = RunOutcome::Cancelled;
            break;
        }
        let sql = stmt.span.text(&plan.text);
        if let Err(error) = session.use_schema(plan.schema.as_deref(), txn).await {
            console.output(&shared, run_id, OutputBody::Error { statement: None, error });
        }
        if stmt.transaction == TransactionPlan::BeginIfIdle && txn == TxnState::Idle {
            match session.batch("BEGIN").await {
                Ok(()) => {
                    txn = TxnState::Active;
                    console
                        .edit(&shared, run_id, |inner| {
                            inner.txn = txn;
                            inner.txn_depth = session.txn_depth();
                        })
                        .await?;
                    console.output(&shared, run_id, OutputBody::Info { message: "BEGIN (implicit)".into() });
                }
                Err(error) => {
                    set_status(&console, &shared, run_id, i, StmtStatus::Failed { error: error.clone() }).await?;
                    console.output(&shared, run_id, OutputBody::Error { statement: Some(i), error });
                    outcome = RunOutcome::Failed;
                    break;
                }
            }
        }
        let fence = if plan.read_only.covers(index) { ReadFence::of(txn) } else { None };
        console.output(&shared, run_id, statement_line(i, sql));
        console
            .edit(&shared, run_id, |inner| {
                if plan.connection.database_kind == DatabaseKind::SqlServer {
                    inner.invalidate(DatabaseKind::SqlServer, crate::sql::tsql::invalidates(sql));
                }
                Console::statements_mut(inner)[index].status = StmtStatus::Preparing;
            })
            .await?;
        let unit_started = Instant::now();
        let opened = match fence {
            Some(fence) => session.batch(fence.open()).await.map(|()| Some(fence)),
            None => Ok(None),
        };
        let (fence, ran) = match opened {
            Ok(fence) => (fence, run_outputs(&console, &shared, &mut session, &plan, index, &caps, &stop).await?),
            Err(error) => (None, Err(Failed { error, logged: false, server_done: false })),
        };
        let logged = matches!(ran, Err(Failed { logged: true, .. }));
        let unread = matches!(ran, Err(Failed { server_done: true, .. }));
        let result = ran.map_err(|failed| failed.error);
        let cancelled = stop.load(Ordering::Relaxed) && result.is_err();
        let result = result.map_err(|mut e| {
            e.position =
                if stmt.changed { None } else { e.position.map(|p| DocOffset(stmt.origin.0.saturating_add(p.0))) };
            e
        });
        if let Err(e) = &result
            && e.severity == "FATAL"
        {
            fatal = Some(e.message.clone());
        }
        let finished = if result.is_ok() { StmtOutcome::Ok } else { StmtOutcome::Err };
        // A cancel still closes the fence, or the session would stay inside it.
        if let Some(fence) = fence
            && fatal.is_none()
            && session.closed_reason().is_none()
            && let Err(error) = session.batch(fence.close(result.is_ok())).await
        {
            session.retire(format!("The run could not end its read-only transaction: {}", error.message));
        }
        let refused =
            fence.is_some() && matches!(&result, Err(error) if error.sqlstate.as_deref() == Some(READ_ONLY_VIOLATION));
        if fence.is_none() {
            // A result this side could not read did not fail on the server, so it aborted nothing there.
            let on_server = if unread { StmtOutcome::Ok } else { finished };
            txn = session.observed_txn().unwrap_or_else(|| txn_after(txn, &stmt.kind, on_server));
        }
        let (status, history) = match &result {
            Ok((rows, tags)) => (
                StmtStatus::Finished,
                if let Some(rows) = rows {
                    HistoryOutcome::Rows { rows: *rows }
                } else {
                    HistoryOutcome::Command { tag: tags.join("; ") }
                },
            ),
            Err(_) if cancelled => (StmtStatus::Cancelled, HistoryOutcome::Cancelled),
            Err(error) => {
                (StmtStatus::Failed { error: error.clone() }, HistoryOutcome::Error { message: error.message.clone() })
            }
        };
        let unasked_write = plan.writes_stay_unasked && crate::text::is_write(sql, plan.connection.database_kind);
        let chained = result.is_ok() && matches!(stmt.kind, StmtKind::CommitAndChain | StmtKind::RollbackAndChain);
        let ended = crate::txn::ended(&stmt.kind, finished, session.observed_txn());
        let due = console
            .edit(&shared, run_id, |inner| {
                let committed = result.is_ok()
                    && inner.txn != TxnState::Aborted
                    && !matches!(stmt.kind, StmtKind::Rollback | StmtKind::RollbackAndChain);
                inner.txn = txn;
                inner.txn_depth = session.txn_depth();
                if unasked_write {
                    inner.unasked.add(sql);
                }
                if refused {
                    inner.proven.add(sql);
                }
                if ended {
                    inner.unasked = WriteList::default();
                }
                Console::statements_mut(inner)[index].status = status;
                if plan.connection.database_kind == DatabaseKind::Postgres && result.is_ok() {
                    inner.invalidate(DatabaseKind::Postgres, stmt.kind.invalidates());
                }
                if chained { inner.end_refreshes(committed) } else { inner.finish_refreshes(committed) }
            })
            .await?;
        if let Some((db, owed)) = due {
            shared.schedule_refreshes(db, owed);
        }
        let mut lines = notice_lines(&mut session);
        if let Err(error) = &result
            && !logged
        {
            lines.push(OutputBody::Error { statement: Some(i), error: error.clone() });
        }
        if refused {
            lines.push(OutputBody::Info { message: "Production: this statement tried to write, so it ran read-only and nothing changed. Run it again to confirm the write.".into() });
        }
        console.output_all(&shared, run_id, lines);
        shared.record_history(HistoryInsert {
            database_kind: plan.connection.database_kind,
            at_ms: now_ms(),
            connection_id: plan.connection.id,
            connection_name: plan.connection.name.clone(),
            database: plan.database.clone(),
            sql: sql.to_owned(),
            elapsed_ms: elapsed_ms(unit_started),
            outcome: history,
        });
        index += 1;
        if cancelled {
            outcome = RunOutcome::Cancelled;
            break;
        }
        if result.is_err() {
            outcome = RunOutcome::Failed;
            if settings.stop_on_error {
                break;
            }
        }
        if fatal.is_some() || session.closed_reason().is_some() {
            break;
        }
    }
    let lost = session.closed_reason_settled(fatal).await;
    let due = console
        .edit(&shared, run_id, |inner| {
            let statements = Console::statements_mut(inner);
            for s in statements.iter_mut().skip(index) {
                if matches!(s.status, StmtStatus::Pending) {
                    s.status = StmtStatus::Skipped;
                }
            }
            let statements = statements.clone();
            if let Some(reason) = &lost {
                console.output(&shared, run_id, loss_line(&session, txn, reason));
            }
            let due = Console::return_session(inner, session, txn, lost);
            inner.run = RunState::Finished { run_id, elapsed_ms: elapsed_ms(started), statements, outcome };
            due
        })
        .await?;
    if let Some((db, owed)) = due {
        shared.schedule_refreshes(db, owed);
    }
    Ok(())
}

const READ_ONLY_VIOLATION: &str = "25006";

/// How a run keeps a Production read from writing. The close undoes whatever the statement did, so a fenced
/// read never ends, begins, or aborts the user's transaction.
#[derive(Clone, Copy)]
enum ReadFence {
    /// `COMMIT` on success keeps a session setting the read changed, as auto-commit would.
    Txn,
    /// Read-only cannot be switched off once the statement has read, and a `RELEASE` would leave the user's
    /// transaction read-only, so the close rolls back to the savepoint.
    Savepoint,
}

impl ReadFence {
    fn of(txn: TxnState) -> Option<Self> {
        match txn {
            TxnState::Idle => Some(ReadFence::Txn),
            TxnState::Active | TxnState::Aborted => Some(ReadFence::Savepoint),
            TxnState::Open => None,
        }
    }

    fn open(self) -> &'static str {
        match self {
            ReadFence::Txn => "BEGIN READ ONLY",
            ReadFence::Savepoint => "SAVEPOINT l1_read; SET LOCAL transaction_read_only = on",
        }
    }

    fn close(self, succeeded: bool) -> &'static str {
        match (self, succeeded) {
            (ReadFence::Txn, true) => "COMMIT",
            (ReadFence::Txn, false) => "ROLLBACK",
            (ReadFence::Savepoint, _) => "ROLLBACK TO SAVEPOINT l1_read; RELEASE SAVEPOINT l1_read",
        }
    }
}

pub async fn count_task(
    console: Arc<Console>,
    shared: Arc<Shared>,
    mut session: Session,
    run_id: RunId,
    sql: String,
    stop: Arc<AtomicBool>,
    mut reply: tokio::sync::oneshot::Sender<Result<u64, CountError>>,
) {
    let result = if stop.load(Ordering::Relaxed) {
        Err(crate::session::error("Count cancelled."))
    } else {
        tokio::select! {
            biased;
            _ = reply.closed() => Err(crate::session::error("Count request was abandoned.")),
            result = session.count(&sql) => result,
        }
    };
    let fatal = result.as_ref().err().filter(|e| e.severity == "FATAL").map(|e| e.message.clone());
    let lost = session.closed_reason_settled(fatal).await;
    let retired = lost.is_some();
    let mut released = session.closed_watch();
    let txn = session.observed_txn().expect("SQL Server observes native transaction presence");
    let edited = console
        .edit(&shared, run_id, |inner| {
            let due = Console::return_session(inner, session, txn, lost);
            due.or_else(|| inner.finish_refreshes(false))
        })
        .await;
    let result = match edited {
        Err(_) => Err(CountError::Failed { message: "The count tab's session was discarded.".into() }),
        Ok(due) => {
            if let Some((db, owed)) = due {
                if retired {
                    while released.changed().await.is_ok() {}
                }
                shared.schedule_refreshes(db, owed);
            }
            if stop.load(Ordering::Relaxed) {
                Err(CountError::Failed { message: "Count cancelled.".into() })
            } else {
                result.map_err(|e| CountError::Failed { message: e.message })
            }
        }
    };
    let _ = reply.send(result);
}

pub fn loss_line(session: &Session, txn: TxnState, reason: &str) -> OutputBody {
    let message = match session.transaction_loss(txn) {
        TransactionLoss::RolledBack => format!("Transaction rolled back: connection lost ({reason})"),
        TransactionLoss::None => format!("Session reset: connection lost ({reason})"),
        TransactionLoss::Unknown => format!(
            "Session reset: earlier commits remain committed; the last transaction outcome may be unknown ({reason})"
        ),
    };
    OutputBody::Info { message }
}

async fn set_status(
    console: &Console,
    shared: &Shared,
    run_id: RunId,
    index: u16,
    status: StmtStatus,
) -> Result<(), Orphaned> {
    console.edit(shared, run_id, |inner| Console::statements_mut(inner)[index as usize].status = status).await
}

pub fn notice_lines(session: &mut Session) -> Vec<OutputBody> {
    session.drain_notices()
}

async fn run_outputs(
    console: &Console,
    shared: &Shared,
    session: &mut Session,
    plan: &RunPlan,
    index: usize,
    caps: &Caps,
    stop: &AtomicBool,
) -> Result<Result<(Option<u64>, Vec<String>), Failed>, Orphaned> {
    let sql = plan.statements[index].span.text(&plan.text);
    let run_id = plan.run_id;
    let started = Instant::now();
    let mut last_emit = Instant::now();
    let mut active: Option<(Arc<ResultSet>, usize)> = None;
    let mut outputs = Vec::new();
    let mut rows: Option<u64> = None;
    let mut tags = Vec::new();
    let mut failure = None;
    let mut reported = None;
    let mut nth: u16 = 0;
    let mut rerun: Option<Rerun> = None;
    let mut server_done = false;
    let mut log = OutputBatch::new(console, shared, run_id);
    let mut stream = session.query(sql);
    loop {
        let next = if log.is_empty() { stream.next().await } else { log.wait(&mut stream).await };
        let Some(event) = next else { break };
        match event {
            Err(error) => {
                failure = Some(error);
                break;
            }
            Ok(QueryEvent::Prepared) if stop.load(Ordering::Relaxed) => {
                failure = Some(crate::session::error("Cancelled"));
                break;
            }
            Ok(QueryEvent::Prepared) => {}
            Ok(QueryEvent::Columns(mut columns)) => {
                if let Some((set, out)) = active.take() {
                    finish_rows(&mut log, index, &set, &mut outputs[out], started, &mut rows);
                }
                if let Some(table) = plan.page.as_ref().and_then(|p| p.relation.as_ref())
                    && plan.connection.database_kind == DatabaseKind::SqlServer
                {
                    if columns.len() != table.columns.len()
                        || columns.iter().zip(&table.columns).any(|(actual, known)| {
                            actual.name != known.name || actual.r#type.category != known.category
                        })
                    {
                        failure = Some(crate::session::error(
                            "Table shape changed. Refresh the catalog before browsing again.",
                        ));
                        break;
                    }
                    for (actual, known) in columns.iter_mut().zip(&table.columns) {
                        actual.source = Some(ColumnSource { table_oid: table.oid, attnum: known.attnum });
                        actual.r#type = SqlTypeRef { display: known.type_display.clone(), category: known.category };
                    }
                }
                let ordinal = match u16::try_from(console.results.lock().len()) {
                    Ok(n) => n,
                    Err(_) => {
                        failure = Some(crate::session::error("Too many result sets"));
                        break;
                    }
                };
                let db = DatabaseRef { connection_id: plan.connection.id, database: plan.database.clone() };
                let rerun = rerun
                    .get_or_insert_with(|| match &plan.page {
                        Some(page) => page.rerun.clone(),
                        None => crate::export::rerun(plan.connection.database_kind, sql, false),
                    })
                    .clone();
                let origin = Origin { rerun, nth, schema: plan.schema.clone(), page: plan.page.is_some() };
                nth = nth.saturating_add(1);
                let set = Arc::new(ResultSet::new(
                    ResultRef { tab_id: console.tab, run_id, index: ordinal },
                    db,
                    plan.connection.database_kind,
                    columns,
                    origin,
                ));
                console.results.lock().push(set.clone());
                active = Some((set.clone(), outputs.len()));
                outputs.push(RunOutput::Rows { result: set.meta(), state: RowsState::Streaming });
                console
                    .edit(shared, run_id, |inner| {
                        let stmt = &mut Console::statements_mut(inner)[index];
                        stmt.status = StmtStatus::Running;
                        stmt.outputs = outputs.clone();
                    })
                    .await?;
            }
            Ok(QueryEvent::Row(cells)) => {
                if let Some((set, _)) = &active {
                    set.push_row(cells.iter().map(|v| v.as_deref()), caps);
                } else {
                    failure = Some(crate::session::error("Driver returned a row without result metadata"));
                    break;
                }
            }
            Ok(QueryEvent::Complete { affected_rows }) => {
                if let Some((set, out)) = active.take() {
                    finish_rows(&mut log, index, &set, &mut outputs[out], started, &mut rows);
                } else {
                    let tag = match (plan.connection.database_kind, affected_rows) {
                        (_, None) => "Commands completed successfully".into(),
                        (DatabaseKind::Postgres, Some(n)) => synthesize_tag(&plan.statements[index].kind, n),
                        (DatabaseKind::SqlServer, Some(1)) => "1 row affected".into(),
                        (DatabaseKind::SqlServer, Some(n)) => format!("{n} rows affected"),
                    };
                    let elapsed_ms = elapsed_ms(started);
                    log.push(OutputBody::Command { statement: index as u16, tag: tag.clone(), elapsed_ms });
                    if matches!(outputs.last(), Some(RunOutput::Command { .. })) {
                        outputs.pop();
                        tags.pop();
                    }
                    tags.push(tag.clone());
                    outputs.push(RunOutput::Command { tag, affected_rows, elapsed_ms });
                }
            }
            Ok(QueryEvent::Notice(body)) => log.push(body),
            Ok(QueryEvent::ServerDone) => server_done = true,
            Ok(QueryEvent::ServerError(error)) => {
                if let Some((set, out)) = active.take() {
                    end_rows(&set, &mut outputs[out], started, RowsState::Failed);
                }
                let stmt = &plan.statements[index];
                let mut shown = error.clone();
                shown.position = if stmt.changed {
                    None
                } else {
                    shown.position.map(|p| DocOffset(stmt.origin.0.saturating_add(p.0)))
                };
                log.push(OutputBody::Error { statement: Some(index as u16), error: shown });
                reported.get_or_insert(error);
            }
        }
        if last_emit.elapsed() >= STREAM_EMIT_INTERVAL {
            last_emit = Instant::now();
            log.send();
            if let Some((set, out)) = &active {
                set.set_elapsed(elapsed_ms(started));
                outputs[*out] = RunOutput::Rows { result: set.meta(), state: RowsState::Streaming };
            }
            console
                .edit(shared, run_id, |inner| Console::statements_mut(inner)[index].outputs = outputs.clone())
                .await?;
        }
    }
    drop(stream);
    if let Some((set, out)) = active {
        if failure.is_none() {
            finish_rows(&mut log, index, &set, &mut outputs[out], started, &mut rows);
        } else {
            end_rows(
                &set,
                &mut outputs[out],
                started,
                if stop.load(Ordering::Relaxed) { RowsState::Cancelled } else { RowsState::Failed },
            );
        }
    }
    log.send();
    console.edit(shared, run_id, |inner| Console::statements_mut(inner)[index].outputs = outputs).await?;
    Ok(match (failure, reported) {
        (Some(error), _) => Err(Failed { error, logged: false, server_done }),
        (None, Some(error)) => Err(Failed { error, logged: true, server_done: false }),
        (None, None) => Ok((rows, tags)),
    })
}

struct Failed {
    error: QueryError,
    logged: bool,
    server_done: bool,
}

fn end_rows(set: &ResultSet, output: &mut RunOutput, started: Instant, state: RowsState) {
    set.set_elapsed(elapsed_ms(started));
    *output = RunOutput::Rows { result: set.meta(), state };
}

fn finish_rows(
    log: &mut OutputBatch,
    index: usize,
    set: &ResultSet,
    output: &mut RunOutput,
    started: Instant,
    rows: &mut Option<u64>,
) {
    set.set_elapsed(elapsed_ms(started));
    let meta = set.meta();
    *rows = Some(rows.unwrap_or(0) + meta.rows_seen);
    log.push(OutputBody::Rows {
        statement: index as u16,
        rows: meta.rows_seen,
        elapsed_ms: meta.elapsed_ms,
        truncated: meta.truncated.is_some(),
    });
    *output = RunOutput::Rows { result: meta, state: RowsState::Complete };
}

/// Caps the logged text so one huge pasted statement cannot bloat the webview's 5,000-entry log.
const STATEMENT_TEXT_CAP: usize = 2_000;

pub fn statement_line(statement: u16, sql: &str) -> OutputBody {
    let text = match sql.char_indices().nth(STATEMENT_TEXT_CAP) {
        Some((cut, _)) => format!("{}…", &sql[..cut]),
        None => sql.to_owned(),
    };
    OutputBody::Statement { statement, text }
}
