mod encode;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures_util::StreamExt;
use tokio::sync::{mpsc, oneshot};

use crate::Shared;
use crate::console::{Console, ConsoleInner, OutputBatch, STREAM_EMIT_INTERVAL, SessionSlot, Work};
use crate::files::Replacement;
use crate::model::*;
use crate::results::{ResultSet, Rerun};
use crate::session::{QueryEvent, Session};
use crate::sql::{self, Vet};
use crate::text::{quote_ident, quote_ident_in};
use encode::{Encoder, Format, InsertTarget};

const CHUNK: usize = 256 * 1024;
const IN_FLIGHT: usize = 4;

const MAY_WRITE: &str = "The statement may write, so it does not run again.";

/// A table page's browse is never vetted: every page flip already runs it. `READ ONLY` does not
/// stop a call that acts outside the transaction, so an unvetted statement needs a confirm.
pub(crate) fn rerun(kind: DatabaseKind, sql: &str, page: bool) -> Rerun {
    match kind {
        DatabaseKind::Postgres if page => match sql::is_read_statement(sql) {
            true => Rerun::Ready(sql.into()),
            false => Rerun::Refused(MAY_WRITE),
        },
        DatabaseKind::Postgres => match sql::vet(sql) {
            Vet::Clear => Rerun::Ready(sql.into()),
            Vet::Unvetted(calls) => Rerun::Unvetted { sql: sql.into(), calls: calls.into() },
            Vet::NotARead => Rerun::Refused(MAY_WRITE),
        },
        DatabaseKind::SqlServer => match sql::tsql::rerun_refusal(sql) {
            None => Rerun::Ready(sql.into()),
            Some(reason) => Rerun::Refused(reason),
        },
    }
}

pub(crate) async fn export(shared: &Arc<Shared>, r: ResultRef, format: ExportFormat, scope: ExportScope, path: String) -> Result<ExportSummary, ExportError> {
    let set = shared.result_set(r).ok_or(ExportError::Gone)?;
    let sql = match scope {
        ExportScope::Loaded => None,
        ExportScope::All => Some(set.origin.rerun.sql().ok_or(ExportError::NotRerunnable)?.clone()),
    };
    let encoder = Encoder::new(resolve(shared, &set, format).await, set.database_kind, &set.columns);
    match sql {
        None => blocking(shared, move || write_loaded(&set, encoder, &path)).await,
        Some(sql) => run_again(shared, set, format, sql, encoder, path).await,
    }
}

async fn blocking<T: Send + 'static>(shared: &Shared, f: impl FnOnce() -> Result<T, ExportError> + Send + 'static) -> Result<T, ExportError> {
    shared.runtime.spawn_blocking(f).await.unwrap_or_else(|e| Err(ExportError::Io { message: e.to_string() }))
}

fn io(e: std::io::Error) -> ExportError {
    ExportError::Io { message: e.to_string() }
}

async fn resolve(shared: &Arc<Shared>, set: &ResultSet, format: ExportFormat) -> Format {
    match format {
        ExportFormat::Csv => Format::Csv,
        ExportFormat::Json => Format::Json,
        ExportFormat::Markdown => Format::Markdown,
        ExportFormat::SqlInsert => Format::Insert(insert_target(shared, set).await),
    }
}

/// Uses the table's own column names, so `SELECT id AS order_id` still inserts into `id`.
async fn insert_target(shared: &Arc<Shared>, set: &ResultSet) -> InsertTarget {
    let dialect = set.database_kind;
    let placeholder = || InsertTarget::placeholder(dialect, &set.columns);
    let Some(oid) = single_source(&set.columns) else { return placeholder() };
    let Ok(catalog) = shared.catalog(&set.db).await else { return placeholder() };
    let Some((schema, relation)) = catalog.relation_by_oid(oid) else { return placeholder() };
    let found = set.columns.iter().map(|c| relation.columns.iter().find(|k| c.source.is_some_and(|s| s.attnum == k.attnum))).collect::<Option<Vec<_>>>();
    let Some(sources) = found else { return placeholder() };
    // `writable` is false both for an identity the server numbers and for a column it computes.
    // Only the identity takes a value, so the detail tells them apart.
    let identities = match sources.iter().any(|k| !k.writable) {
        false => vec![],
        true => match shared.relation_detail(&set.db, oid).await {
            Ok(detail) => detail.defaults.into_iter().filter(|d| matches!(d.value, DefaultValue::Identity { .. })).map(|d| d.column).collect(),
            Err(_) => return placeholder(),
        },
    };
    let columns: Vec<_> = sources.iter().map(|k| (k.writable || identities.contains(&k.name)).then(|| quote_ident_in(dialect, &k.name))).collect();
    if columns.iter().all(Option::is_none) {
        return placeholder();
    }
    InsertTarget {
        table: format!("{}.{}", quote_ident_in(dialect, schema), quote_ident_in(dialect, &relation.name)),
        columns,
        identity: sources.iter().any(|k| !k.writable && identities.contains(&k.name)),
    }
}

fn single_source(columns: &[ColumnMeta]) -> Option<u32> {
    let oid = columns.first()?.source?.table_oid;
    let mut attnums = std::collections::HashSet::new();
    columns.iter().all(|c| c.source.is_some_and(|s| s.table_oid == oid && attnums.insert(s.attnum))).then_some(oid)
}

fn write_loaded(set: &ResultSet, mut encoder: Encoder, path: &str) -> Result<ExportSummary, ExportError> {
    let mut file = Replacement::begin(path).map_err(io)?;
    let mut buf = Vec::with_capacity(CHUNK + 4096);
    encoder.begin(&mut buf);
    let (rows, truncated) = set
        .each_row(|cells| {
            encoder.row(cells.iter().copied(), &mut buf);
            if buf.len() >= CHUNK {
                file.write_all(&buf)?;
                buf.clear();
            }
            Ok(())
        })
        .map_err(io)?;
    encoder.finish(&mut buf);
    file.write_all(&buf).map_err(io)?;
    let bytes = file.commit().map_err(io)?;
    Ok(ExportSummary { rows, bytes, truncated })
}

/// How a PostgreSQL export fences itself off from the tab. Whatever the statement does, the close
/// undoes it, so the tab's transaction state, search path, and data are as they were. SQL Server
/// has no server-enforced read-only transaction, so it runs bare behind `rerun_refusal`.
enum Fence {
    /// Outside a transaction, in either mode. Manual mode's implicit BEGIN is not taken.
    ReadOnlyTxn,
    /// Inside the user's open transaction, so the export sees its uncommitted rows. A failure or
    /// Esc cannot abort that transaction, and `ROLLBACK TO` reverts the `SET LOCAL`s on success too.
    ReadOnlySavepoint,
    Bare,
}

impl Fence {
    fn of(kind: DatabaseKind, txn: TxnState) -> Result<Self, ExportError> {
        match (kind, txn) {
            (_, TxnState::Aborted) => Err(ExportError::TransactionAborted),
            (DatabaseKind::Postgres, TxnState::Idle) => Ok(Fence::ReadOnlyTxn),
            (DatabaseKind::Postgres, TxnState::Active | TxnState::Open) => Ok(Fence::ReadOnlySavepoint),
            (DatabaseKind::SqlServer, _) => Ok(Fence::Bare),
        }
    }

    fn open(&self, schema: Option<&str>) -> Option<String> {
        let path = schema.map(|s| format!("; SET LOCAL search_path TO {}", quote_ident(s))).unwrap_or_default();
        match self {
            Fence::ReadOnlyTxn => Some(format!("BEGIN TRANSACTION READ ONLY{path}")),
            Fence::ReadOnlySavepoint => Some(format!("SAVEPOINT l1_export; SET LOCAL transaction_read_only = on{path}")),
            Fence::Bare => None,
        }
    }

    fn close(&self) -> Option<&'static str> {
        match self {
            Fence::ReadOnlyTxn => Some("ROLLBACK"),
            Fence::ReadOnlySavepoint => Some("ROLLBACK TO SAVEPOINT l1_export; RELEASE SAVEPOINT l1_export"),
            Fence::Bare => None,
        }
    }
}

struct Job {
    console: Arc<Console>,
    shared: Arc<Shared>,
    run_id: RunId,
    txn: TxnState,
    fence: Fence,
    sql: Arc<str>,
    set: Arc<ResultSet>,
    format: ExportFormat,
    path: String,
}

async fn run_again(shared: &Arc<Shared>, set: Arc<ResultSet>, format: ExportFormat, sql: Arc<str>, encoder: Encoder, path: String) -> Result<ExportSummary, ExportError> {
    let console = shared.console(set.r#ref.tab_id).ok_or(ExportError::Gone)?;
    shared.connect(set.db.connection_id).await.map_err(|error| ExportError::Connect { error })?;
    let instance = shared.instance(set.db.connection_id).ok_or(ExportError::Gone)?;
    let check = |inner: &ConsoleInner| {
        if console.closed.load(Ordering::Relaxed) || console.result_set(set.r#ref).is_none() {
            return Err(ExportError::Gone);
        }
        if inner.config.connection_id != set.db.connection_id || instance.canonical_database(&inner.config.database) != instance.canonical_database(&set.db.database) {
            return Err(ExportError::TargetChanged);
        }
        if matches!(inner.session, SessionSlot::Busy { .. }) {
            return Err(ExportError::Busy);
        }
        Fence::of(set.database_kind, inner.txn)
    };
    let (config, needs_session) = {
        let inner = console.inner.lock().await;
        check(&inner)?;
        (inner.config.clone(), matches!(inner.session, SessionSlot::Empty | SessionSlot::Lost { .. }))
    };
    // A bad folder or a read-only file fails here, before the tab ever shows busy.
    let target = path.clone();
    let file = blocking(shared, move || Replacement::begin(&target).map_err(io)).await?;
    let fresh = if needs_session {
        Some(shared.open_tab_session(&config).await.map_err(|error| ExportError::Connect { error })?)
    } else {
        None
    };
    let run_id = shared.next_run_id();
    let (session, stop, txn, fence) = {
        let mut inner = console.inner.lock().await;
        let fence = check(&inner)?;
        if let Some(idle) = fresh
            && matches!(inner.session, SessionSlot::Empty | SessionSlot::Lost { .. })
        {
            inner.session = SessionSlot::Idle(idle);
        }
        let progress = ExportProgress { result: set.r#ref, format, rows: 0, bytes: 0, write_error: None };
        let (session, stop) = console.take_session(&mut inner, run_id, Work::Export(progress)).ok_or(ExportError::Busy)?;
        let txn = inner.txn;
        console.publish(&mut inner, shared);
        (session, stop, txn, fence)
    };
    let (reply, done) = oneshot::channel();
    let job = Job { console, shared: shared.clone(), run_id, txn, fence, sql, set, format, path };
    shared.spawn_holding(job.console.clone(), run_id, export_task(job, session, encoder, file, stop, reply));
    // The task does not stop when this caller goes away, such as on a webview reload: progress
    // lives in the console snapshot and the outcome in the Output log.
    done.await.unwrap_or(Err(ExportError::Gone))
}

enum Halt {
    Cancelled,
    Query(QueryError),
    Disk(String),
    ShapeChanged,
    Orphaned,
}

enum Feed {
    Bytes(Vec<u8>),
    Commit,
}

struct Writer {
    feed: mpsc::Sender<Feed>,
    handle: Option<tokio::task::JoinHandle<std::io::Result<u64>>>,
    error: Option<String>,
}

impl Writer {
    fn spawn(shared: &Shared, mut file: Replacement) -> Self {
        let (feed, mut rx) = mpsc::channel::<Feed>(IN_FLIGHT);
        let handle = shared.runtime.spawn_blocking(move || {
            while let Some(feed) = rx.blocking_recv() {
                match feed {
                    Feed::Bytes(bytes) => file.write_all(&bytes)?,
                    Feed::Commit => return file.commit(),
                }
            }
            Err(std::io::Error::other("The export was abandoned."))
        });
        Self { feed, handle: Some(handle), error: None }
    }

    /// Waits while the queue is full, so a slow disk stops reading the stream, and the socket's
    /// window then stops the server.
    async fn send(&mut self, bytes: Vec<u8>) -> Result<(), String> {
        if self.error.is_none() && self.feed.send(Feed::Bytes(bytes)).await.is_err() {
            let failed = self.join().await.err();
            self.error = Some(failed.unwrap_or_else(|| "The export writer stopped.".into()));
        }
        self.error.clone().map_or(Ok(()), Err)
    }

    async fn join(&mut self) -> Result<u64, String> {
        match self.handle.take() {
            Some(handle) => match handle.await {
                Ok(written) => written.map_err(|e| e.to_string()),
                Err(e) => Err(e.to_string()),
            },
            None => Err(self.error.clone().unwrap_or_else(|| "The export writer stopped.".into())),
        }
    }

    async fn commit(mut self, tail: Vec<u8>) -> Result<u64, String> {
        self.send(tail).await?;
        let _ = self.feed.send(Feed::Commit).await;
        self.join().await
    }

    async fn abandon(self) {
        let Self { feed, handle, .. } = self;
        drop(feed);
        if let Some(handle) = handle {
            let _ = handle.await;
        }
    }
}

async fn export_task(job: Job, mut session: Session, mut encoder: Encoder, file: Replacement, stop: Arc<AtomicBool>, reply: oneshot::Sender<Result<ExportSummary, ExportError>>) {
    let mut writer = Writer::spawn(&job.shared, file);
    let schema = job.set.origin.schema.as_deref().filter(|s| session.search_path.as_deref() != Some(*s));
    let opened = match job.fence.open(schema) {
        Some(sql) => session.batch(&sql).await,
        None => Ok(()),
    };
    let streamed = match opened {
        Ok(()) => pump(&job, &mut session, &mut encoder, &mut writer, &stop).await,
        Err(_) if stop.load(Ordering::Relaxed) => Err(Halt::Cancelled),
        Err(e) => Err(Halt::Query(e)),
    };
    // Every path closes the fence. A close that fails leaves the session in an unknown
    // transaction, so it is retired, as `table_apply` retires after a failed rollback.
    if let Some(close) = job.fence.close()
        && session.closed_reason().is_none()
        && let Err(e) = session.batch(close).await
    {
        session.retire(format!("The export could not end its read-only transaction: {}", e.message));
    }
    job.console.output_all(&job.shared, job.run_id, session.drain_notices());

    // Esc means no file, even when it lands after the last row.
    let outcome = match streamed {
        Ok(rows) if !stop.load(Ordering::Relaxed) => {
            let mut tail = Vec::new();
            encoder.finish(&mut tail);
            match writer.commit(tail).await {
                Ok(bytes) => Ok(ExportSummary { rows, bytes, truncated: false }),
                Err(message) => Err(ExportError::Io { message }),
            }
        }
        streamed => {
            writer.abandon().await;
            Err(match streamed {
                Ok(_) | Err(Halt::Cancelled) => ExportError::Cancelled,
                Err(Halt::Query(error)) => ExportError::Query { error },
                Err(Halt::Disk(message)) => ExportError::Io { message },
                Err(Halt::ShapeChanged) => ExportError::ShapeChanged,
                Err(Halt::Orphaned) => ExportError::Gone,
            })
        }
    };

    let fatal = match &outcome {
        Err(ExportError::Query { error }) if error.severity == "FATAL" => Some(error.message.clone()),
        _ => None,
    };
    let lost = session.closed_reason_settled(fatal).await;
    // PostgreSQL: the fence put `txn` back. SQL Server: what the server reports, which a failed
    // batch under XACT_ABORT can change.
    let after = session.observed_txn().unwrap_or(job.txn);
    let mut lines: Vec<OutputBody> = log_line(&outcome, job.format, &job.path).into_iter().collect();
    lines.extend(lost.as_deref().map(|reason| crate::console::loss_line(&session, after, reason)));
    job.console.output_all(&job.shared, job.run_id, lines);
    let returned = job.console.edit(&job.shared, job.run_id, |inner| Console::return_session(inner, session, after, lost).or_else(|| inner.finish_refreshes(false))).await;
    let outcome = match returned {
        Ok(due) => {
            if let Some((db, owed)) = due {
                job.shared.schedule_refreshes(db, owed);
            }
            outcome
        }
        // Close or disconnect took the slot. A file already in place stays in place.
        Err(_) => outcome.or(Err(ExportError::Gone)),
    };
    let _ = reply.send(outcome);
}

/// Every halt after the stream starts reads it to its end. Where a cancel keeps the session
/// (PostgreSQL), a halt the user did not ask for also cancels, so the server stops sending; on SQL
/// Server a cancel would reset the session and its transaction, so the rest is read and dropped.
async fn pump(job: &Job, session: &mut Session, encoder: &mut Encoder, writer: &mut Writer, stop: &AtomicBool) -> Result<u64, Halt> {
    let cancel = session.cancel_handle();
    let cancel_keeps_session = session.cancel_keeps_session();
    let nth = job.set.origin.nth;
    let mut buf = Vec::with_capacity(CHUNK + 4096);
    encoder.begin(&mut buf);
    let (mut rows, mut sent) = (0u64, 0u64);
    let mut set: Option<u16> = None;
    let mut halt: Option<Halt> = None;
    // A server flushes rows in bursts, so progress follows a clock rather than the rows.
    let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + STREAM_EMIT_INTERVAL, STREAM_EMIT_INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut published = ExportProgress { result: job.set.r#ref, format: job.format, rows: 0, bytes: 0, write_error: None };
    let mut log = OutputBatch::new(&job.console, &job.shared, job.run_id);
    let mut stream = session.query(&job.sql);
    loop {
        let event = tokio::select! {
            event = async { if log.is_empty() { stream.next().await } else { log.wait(&mut stream).await } } => match event {
                Some(event) => Some(event),
                None => break,
            },
            _ = tick.tick() => None,
        };
        let mut emit = event.is_none();
        match event {
            None => log.send(),
            // After a halt, the first cause wins over the error the server sends for it.
            Some(Err(_)) if halt.is_some() => break,
            Some(Err(_)) if stop.load(Ordering::Relaxed) => {
                halt = Some(Halt::Cancelled);
                break;
            }
            Some(Err(e)) => {
                halt = Some(Halt::Query(e));
                break;
            }
            Some(Ok(QueryEvent::Prepared)) if stop.load(Ordering::Relaxed) => {
                halt = Some(Halt::Cancelled);
                break;
            }
            Some(Ok(QueryEvent::Prepared | QueryEvent::Complete { .. } | QueryEvent::ServerDone)) => {}
            Some(Ok(QueryEvent::Notice(body))) => log.push(body),
            Some(Ok(QueryEvent::ServerError(e))) => {
                halt.get_or_insert(Halt::Query(e));
            }
            Some(Ok(QueryEvent::Columns(columns))) => {
                let index = set.map_or(0, |i| i.saturating_add(1));
                set = Some(index);
                // The encoder types values by the loaded categories. A SQL Server page loaded only
                // when its live categories matched the catalog's, so the same check holds there.
                let same = columns.len() == job.set.columns.len() && columns.iter().zip(&job.set.columns).all(|(a, b)| a.name == b.name && a.r#type.category == b.r#type.category);
                if index == nth && !same && halt.is_none() {
                    halt = Some(Halt::ShapeChanged);
                    if cancel_keeps_session {
                        cancel.cancel().await;
                    }
                }
            }
            Some(Ok(QueryEvent::Row(cells))) if set == Some(nth) && halt.is_none() => {
                encoder.row(cells.iter().map(Option::as_deref), &mut buf);
                rows += 1;
                if buf.len() >= CHUNK {
                    sent += buf.len() as u64;
                    if let Err(message) = writer.send(std::mem::take(&mut buf)).await {
                        halt = Some(Halt::Disk(message));
                        emit = true;
                        if cancel_keeps_session {
                            cancel.cancel().await;
                        }
                    }
                }
            }
            Some(Ok(QueryEvent::Row(_))) => {}
        }
        let progress = emit.then(|| ExportProgress { rows, bytes: sent, write_error: writer.error.clone(), ..published.clone() });
        if let Some(progress) = progress.filter(|p| *p != published) {
            let next = progress.clone();
            let progressed = job.console.edit(&job.shared, job.run_id, |inner| {
                if let Some(p) = inner.export_progress_mut() {
                    *p = next;
                }
            });
            if progressed.await.is_err() {
                halt = Some(Halt::Orphaned);
                break;
            }
            published = progress;
        }
    }
    drop(stream);
    log.send();
    match halt {
        Some(halt) => Err(halt),
        None if set.is_none_or(|i| i < nth) => Err(Halt::ShapeChanged),
        None if buf.is_empty() => Ok(rows),
        None => writer.send(buf).await.map(|()| rows).map_err(Halt::Disk),
    }
}

fn log_line(outcome: &Result<ExportSummary, ExportError>, format: ExportFormat, path: &str) -> Option<OutputBody> {
    let unchanged = "The file was not changed.";
    let info = |message: String| Some(OutputBody::Info { message });
    match outcome {
        Ok(s) => info(format!("Exported {} as {} to {path} ({}).", rows_text(s.rows), format_label(format), size_text(s.bytes))),
        Err(ExportError::Cancelled) => info(format!("Export to {path} cancelled. {unchanged}")),
        Err(ExportError::Io { message }) => info(format!("Export to {path} failed: {message}. {unchanged}")),
        Err(ExportError::ShapeChanged) => info(format!("Export to {path} failed: the query returned other columns when it ran again. {unchanged}")),
        Err(ExportError::Query { error }) => Some(OutputBody::Error {
            statement: None,
            error: QueryError { message: format!("Export to {path} failed: {}. {unchanged}", error.message), ..error.clone() },
        }),
        Err(_) => None,
    }
}

fn format_label(format: ExportFormat) -> &'static str {
    match format {
        ExportFormat::Csv => "CSV",
        ExportFormat::Json => "JSON",
        ExportFormat::Markdown => "Markdown",
        ExportFormat::SqlInsert => "SQL INSERT",
    }
}

fn rows_text(rows: u64) -> String {
    let digits = rows.to_string();
    let mut grouped = String::new();
    for (i, d) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(d);
    }
    format!("{grouped} {}", if rows == 1 { "row" } else { "rows" })
}

fn size_text(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    match bytes as f64 {
        b if b < KB => format!("{bytes} B"),
        b if b < KB * KB => format!("{:.1} KB", b / KB),
        b => format!("{:.1} MB", b / (KB * KB)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_line_groups_rows_and_names_the_format() {
        let ok = Ok(ExportSummary { rows: 1_284_551, bytes: 3 * 1024 * 1024 + 300 * 1024, truncated: false });
        let Some(OutputBody::Info { message }) = log_line(&ok, ExportFormat::Csv, "/tmp/o.csv") else { panic!() };
        assert_eq!(message, "Exported 1,284,551 rows as CSV to /tmp/o.csv (3.3 MB).");
        let one = Ok(ExportSummary { rows: 1, bytes: 12, truncated: false });
        let Some(OutputBody::Info { message }) = log_line(&one, ExportFormat::SqlInsert, "/tmp/o.sql") else { panic!() };
        assert_eq!(message, "Exported 1 row as SQL INSERT to /tmp/o.sql (12 B).");
        assert!(log_line(&Err(ExportError::Gone), ExportFormat::Json, "/x").is_none());
    }

    #[test]
    fn only_reads_run_again_and_unvetted_calls_ask_first() {
        use DatabaseKind::{Postgres, SqlServer};
        assert!(matches!(rerun(Postgres, "select * from orders", false), Rerun::Ready(_)));
        assert!(matches!(rerun(Postgres, "select nextval('s'), pg_sleep(1)", false), Rerun::Unvetted { calls, .. } if &*calls == ["nextval", "pg_sleep"]));
        assert!(matches!(rerun(Postgres, "delete from orders returning *", false), Rerun::Refused(_)));
        assert!(matches!(rerun(Postgres, "select * from orders where pg_sleep(0) is not null", true), Rerun::Ready(_)), "a page is never asked about");
        assert!(matches!(rerun(SqlServer, "SELECT * FROM dbo.orders", false), Rerun::Ready(_)));
        assert!(matches!(rerun(SqlServer, "SELECT 1; COMMIT", false), Rerun::Refused(_)));
    }
}
