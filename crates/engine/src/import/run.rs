use std::fs::File;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use tokio::sync::{mpsc, watch};

use super::{Execution, ImportPlan, Prepared, csv};
use crate::{
    Shared,
    console::{Console, STREAM_EMIT_INTERVAL, SessionSlot, Work},
    model::*,
    session::{CopyChunk, Session},
};

pub(super) enum Fence {
    Auto,
    Manual,
    Savepoint,
}

impl Fence {
    pub fn new(txn: TxnState, mode: TxnMode) -> Self {
        if txn == TxnState::Active {
            Self::Savepoint
        } else if mode == TxnMode::Manual {
            Self::Manual
        } else {
            Self::Auto
        }
    }
    fn open(&self) -> &'static str {
        match self {
            Self::Savepoint => "SAVEPOINT l1_import",
            _ => "BEGIN",
        }
    }
    fn finish(&self) -> Option<&'static str> {
        match self {
            Self::Auto => Some("COMMIT"),
            Self::Manual => None,
            Self::Savepoint => Some("RELEASE SAVEPOINT l1_import"),
        }
    }
    fn undo(&self) -> &'static str {
        match self {
            Self::Savepoint => "ROLLBACK TO SAVEPOINT l1_import; RELEASE SAVEPOINT l1_import",
            _ => "ROLLBACK",
        }
    }
    pub fn statements<'a>(&self, lock: &'a str, identity: &'a str, copy: &'a str) -> Vec<&'a str> {
        std::iter::once(self.open())
            .chain([lock, identity, copy])
            .chain(self.finish())
            .chain(std::iter::once(self.undo()))
            .collect()
    }
}

fn produce(
    prepared: &Prepared,
    request: &ImportRequest,
    stop: &AtomicBool,
    tx: &mpsc::Sender<Result<CopyChunk, ImportError>>,
) -> Result<(), ImportError> {
    let mut parser = csv::Parser::new(File::open(&prepared.snapshot.path).map_err(csv::io)?)?;
    let mut rows = 0;
    let mut bytes = 0;
    let mut data = Vec::with_capacity(256 * 1024);
    if request.header {
        bytes = parser.next(stop)?.map_or(0, |r| r.bytes);
    }
    while let Some(record) = parser.next(stop)? {
        if stop.load(Ordering::Relaxed) {
            return Err(ImportError::Cancelled);
        }
        csv::encode(&record.fields, &request.mapping, &mut data);
        rows += 1;
        bytes = record.bytes;
        if data.len() >= 256 * 1024 {
            tx.blocking_send(Ok(CopyChunk {
                data: std::mem::replace(&mut data, Vec::with_capacity(256 * 1024)),
                bytes,
                rows,
            }))
            .map_err(|_| ImportError::Cancelled)?;
        }
    }
    if !data.is_empty() || rows == 0 {
        tx.blocking_send(Ok(CopyChunk { data, bytes, rows })).map_err(|_| ImportError::Cancelled)?;
    }
    Ok(())
}

pub(super) async fn run(
    s: &Arc<Shared>,
    console: Arc<Console>,
    run_id: RunId,
    mut session: Session,
    stop: Arc<AtomicBool>,
    plan: Arc<ImportPlan>,
    unasked: bool,
) -> Result<ImportSummary, ImportError> {
    if let Execution::SqlServer(insert) = &plan.execution {
        return run_mssql(s, console, run_id, session, stop, &plan, insert).await;
    }
    let Execution::Postgres { sql, lock, identity, fence } = &plan.execution else { unreachable!() };
    let prepared = &plan.prepared;
    let backend_pid = session.backend_pid;
    let (tx, rx) = mpsc::channel(4);
    let producer_stop = stop.clone();
    let source = plan.clone();
    let producer = s.runtime.spawn_blocking(move || {
        let result = produce(&source.prepared, &source.request, &producer_stop, &tx);
        if let Err(error) = &result {
            let _ = tx.blocking_send(Err(error.clone()));
        }
        result
    });
    let (progress, mut changes) = watch::channel((0, 0));
    let mut opened = false;
    let mut commit_unknown = false;
    let mut outcome = async {
        if stop.load(Ordering::Relaxed) { return Err(ImportError::Cancelled); }
        session.batch(fence.open()).await.map_err(|error| ImportError::Query { error })?;
        opened = true;
        if stop.load(Ordering::Relaxed) { return Err(ImportError::Cancelled); }
        session.batch(&lock).await.map_err(|error| ImportError::Query { error })?;
        if session.count(&identity).await.map_err(|error| ImportError::Query { error })? != u64::from(prepared.target.oid) { return Err(ImportError::TargetChanged); }
        let copying = session.import_csv(&sql, rx, &stop, progress, |notice| console.output(s, run_id, notice));
        tokio::pin!(copying);
        let mut tick = tokio::time::interval(STREAM_EMIT_INTERVAL);
        let rows = loop {
            tokio::select! {
                result = &mut copying => break result?,
                _ = tick.tick() => {
                    let (bytes, rows) = *changes.borrow_and_update();
                    if console.edit(s, run_id, |inner| {
                        if let SessionSlot::Busy { work: Work::Import { progress, .. }, .. } = &mut inner.session { progress.bytes = bytes; progress.rows = rows; }
                    }).await.is_err() { stop.store(true, Ordering::Relaxed); }
                }
            }
        };
        Ok(rows)
    }.await;
    if stop.load(Ordering::Relaxed)
        && matches!(&outcome, Err(ImportError::Query { error }) if error.sqlstate.as_deref() == Some("57014"))
    {
        outcome = Err(ImportError::Cancelled);
    }
    if outcome.is_err() {
        stop.store(true, Ordering::Relaxed);
    }
    let produced = producer.await.map_err(|e| ImportError::Io { message: e.to_string() }).and_then(|r| r);
    let mut outcome = outcome.and_then(|rows| produced.map(|()| rows));
    if outcome.is_ok() && stop.load(Ordering::Relaxed) {
        outcome = Err(ImportError::Cancelled);
    }
    if outcome.is_ok() {
        if console
            .edit(s, run_id, |inner| {
                if let SessionSlot::Busy { work: Work::Import { progress, .. }, .. } = &mut inner.session {
                    let (bytes, rows) = *changes.borrow();
                    progress.bytes = bytes;
                    progress.rows = rows;
                    progress.finishing = true;
                }
            })
            .await
            .is_err()
        {
            outcome = Err(ImportError::Cancelled);
        }
    }
    if outcome.is_ok() && stop.load(Ordering::Relaxed) {
        outcome = Err(ImportError::Cancelled);
    }
    if outcome.is_ok() {
        if let Some(finish) = fence.finish() {
            if let Err(error) = session.batch(finish).await {
                commit_unknown = matches!(fence, Fence::Auto)
                    && error.sqlstate.as_deref().is_none_or(|state| state.starts_with("08") || state == "40003");
                outcome = Err(if commit_unknown {
                    ImportError::OutcomeUnknown { error }
                } else {
                    ImportError::Query { error }
                });
            }
        }
    }
    let mut cleanup_failed = false;
    if outcome.is_err() && opened {
        let cleanup = tokio::time::timeout(std::time::Duration::from_secs(10), session.batch(fence.undo())).await;
        if !matches!(cleanup, Ok(Ok(()))) {
            cleanup_failed = true;
            let message = match cleanup {
                Ok(Err(error)) => error.message,
                _ => "Rollback timed out".into(),
            };
            outcome = Err(ImportError::OutcomeUnknown {
                error: crate::session::error(format!(
                    "Import failed and cleanup could not be verified: {message}. The session was retired."
                )),
            });
        }
    }
    if commit_unknown || cleanup_failed {
        session.retire("CSV import transaction outcome is unknown".into());
    }
    let after = match (&fence, &outcome) {
        (Fence::Savepoint, _) | (Fence::Manual, Ok(_)) => TxnState::Active,
        _ => TxnState::Idle,
    };
    let lost = session.closed_reason_settled(None).await;
    let notices = crate::console::notice_lines(&mut session);
    let ended = opened
        && !matches!(fence, Fence::Savepoint)
        && (matches!(fence, Fence::Auto) || outcome.is_err())
        && !commit_unknown
        && !cleanup_failed;
    let line = match &outcome {
        Ok(rows) => OutputBody::Notice {
            severity: "INFO".into(),
            message: format!(
                "{rows} rows imported into {}.{} ({})",
                prepared.target.schema,
                prepared.target.table,
                if after == TxnState::Idle { "committed" } else { "pending commit" }
            ),
            detail: None,
        },
        Err(error) => OutputBody::Error {
            statement: None,
            error: match error {
                ImportError::Query { error } | ImportError::OutcomeUnknown { error } => error.clone(),
                ImportError::Cancelled => crate::session::error("CSV import cancelled; no import rows kept"),
                _ => crate::session::error(format!("CSV import failed: {error:?}")),
            },
        },
    };
    let returned = console
        .edit(s, run_id, |inner| {
            if outcome.is_ok() && unasked && after == TxnState::Active {
                inner.unasked.add(&sql);
            }
            if ended {
                inner.unasked = WriteList::default();
            }
            let due = Console::return_session(inner, session, after, lost);
            if commit_unknown || cleanup_failed {
                if let SessionSlot::Lost { transaction, .. } = &mut inner.session {
                    *transaction = TransactionLoss::Unknown;
                }
            }
            due
        })
        .await;
    match returned {
        Err(_) => return Err(ImportError::Gone),
        Ok(due) => {
            console.output_all(s, run_id, notices.into_iter().chain(std::iter::once(line)).collect());
            if let Some((db, owed)) = due {
                s.schedule_refreshes(db, owed);
            }
        }
    }
    outcome.map(|rows| ImportSummary { backend_pid, rows, committed: after == TxnState::Idle })
}

fn produce_rows(
    plan: &ImportPlan,
    max_rows: usize,
    stop: &AtomicBool,
    tx: &mpsc::Sender<Result<crate::session::ImportRows, ImportError>>,
) -> Result<(), ImportError> {
    let Execution::SqlServer(insert) = &plan.execution else { unreachable!() };
    let mut parser = csv::Parser::new(File::open(&plan.prepared.snapshot.path).map_err(csv::io)?)?;
    if plan.request.header {
        parser.next(stop)?;
    }
    let mut chunk = crate::session::ImportRows { rows: Vec::new(), bytes: 0, records: 0, first_record: 0 };
    let mut size = 0;
    while let Some(record) = parser.next(stop)? {
        let row = record
            .fields
            .into_iter()
            .zip(&plan.request.mapping)
            .filter_map(|(value, column)| column.as_ref().map(|_| value))
            .collect::<Vec<_>>();
        insert.validate_row(&row, record.line, record.number)?;
        let bytes = row.iter().flatten().map(String::len).sum::<usize>();
        if !chunk.rows.is_empty() && (chunk.rows.len() == max_rows || size + bytes > 256 * 1024) {
            let next = crate::session::ImportRows {
                rows: Vec::new(),
                bytes: chunk.bytes,
                records: chunk.records,
                first_record: 0,
            };
            tx.blocking_send(Ok(std::mem::replace(&mut chunk, next))).map_err(|_| ImportError::Cancelled)?;
            size = 0;
        }
        if chunk.rows.is_empty() {
            chunk.first_record = record.number;
        }
        chunk.rows.push(row);
        chunk.records += 1;
        chunk.bytes = record.bytes;
        size += bytes;
    }
    tx.blocking_send(Ok(chunk)).map_err(|_| ImportError::Cancelled)?;
    Ok(())
}

async fn run_mssql(
    s: &Arc<Shared>,
    console: Arc<Console>,
    run_id: RunId,
    mut session: Session,
    stop: Arc<AtomicBool>,
    plan: &Arc<ImportPlan>,
    insert: &crate::session::InsertPlan,
) -> Result<ImportSummary, ImportError> {
    let backend_pid = session.backend_pid;
    let (tx, rx) = mpsc::channel(4);
    let source = plan.clone();
    let producer_stop = stop.clone();
    let max_rows = insert.max_rows();
    let producer = s.runtime.spawn_blocking(move || {
        let result = produce_rows(&source, max_rows, &producer_stop, &tx);
        if let Err(error) = &result {
            let _ = tx.blocking_send(Err(error.clone()));
        }
        result
    });
    let (progress, changes) = watch::channel((0, 0));
    let result = {
        let work = session.import_rows(insert, rx, producer, &stop, progress);
        tokio::pin!(work);
        let mut tick = tokio::time::interval(STREAM_EMIT_INTERVAL);
        loop {
            tokio::select! {
                result = &mut work => break result,
                _ = tick.tick() => {
                    let (bytes, rows) = *changes.borrow();
                    if console.edit(s, run_id, |inner| {
                        if let SessionSlot::Busy { work: Work::Import { progress, .. }, .. } = &mut inner.session {
                            progress.bytes = bytes; progress.rows = rows;
                            progress.finishing = rows == progress.total_rows || stop.load(Ordering::Relaxed);
                        }
                    }).await.is_err() { stop.store(true, Ordering::Relaxed); }
                }
            }
        }
    };
    let lost = session.closed_reason_settled(None).await;
    let txn = session.observed_txn().unwrap_or(TxnState::Idle);
    let outcome_unknown = matches!(result, Err(ImportError::OutcomeUnknown { .. }));
    let returned = console
        .edit(s, run_id, |inner| {
            let due = Console::return_session(inner, session, txn, lost);
            if let SessionSlot::Lost { transaction, .. } = &mut inner.session {
                *transaction = if outcome_unknown { TransactionLoss::Unknown } else { TransactionLoss::None };
            }
            due
        })
        .await;
    let body = match &result {
        Ok(rows) => OutputBody::Notice {
            severity: "INFO".into(),
            message: format!(
                "{rows} rows imported into {}.{} (committed)",
                plan.prepared.target.schema, plan.prepared.target.table
            ),
            detail: None,
        },
        Err(e) => OutputBody::Error {
            statement: None,
            error: match e {
                ImportError::Query { error } | ImportError::OutcomeUnknown { error } => error.clone(),
                ImportError::Cancelled => crate::session::error("CSV import cancelled; no import rows kept"),
                _ => crate::session::error(format!("CSV import failed: {e:?}")),
            },
        },
    };
    if let Ok(due) = returned {
        console.output(s, run_id, body);
        if let Some((db, owed)) = due {
            s.schedule_refreshes(db, owed);
        }
    }
    result.map(|rows| ImportSummary { backend_pid, rows, committed: true })
}
