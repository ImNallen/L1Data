//! SQL Server staged edits: the advisory support check and Submit's admission and task. The driver in
//! `session/mssql/edit.rs` sends the batches.
use std::sync::Arc;

use crate::{
    Shared,
    console::{self, Console, SessionSlot, Work},
    model::*,
    session::{self, EditAnswer, EditCheck, EditOutcome},
    writes,
};

enum Checked {
    Table(EditCheck),
    Refused(EditRefusal),
}

/// P2 on the database's metadata session, for the relation the tab's catalog shows.
async fn check(s: &Arc<Shared>, config: &ConsoleConfigView, relation: &Relation) -> Result<Checked, ApplyError> {
    if relation.kind != RelKind::Table {
        return Err(ApplyError::Unsupported { what: "Only SQL Server tables accept staged edits.".into() });
    }
    let instance = s.instance(config.connection_id).ok_or(ApplyError::NotConfigured)?;
    let slot = instance.slot(&config.database).ok_or(ApplyError::NotConfigured)?;
    let mut meta = slot.meta.lock().await;
    let session = s.session_in(&instance, &slot, &mut meta).await.map_err(|error| ApplyError::Connect { error })?;
    let answer = match session.edit_check(relation.oid, &relation.qualified_name).await {
        Err(ApplyError::Refused { reason }) => return Ok(Checked::Refused(reason)),
        answer => answer?,
    };
    match answer {
        EditAnswer::Changed => Err(ApplyError::TableChanged),
        EditAnswer::Refused(reason) => Ok(Checked::Refused(reason)),
        EditAnswer::Checked(check) => {
            let same = check.columns.len() == relation.columns.len()
                && check
                    .columns
                    .iter()
                    .zip(&relation.columns)
                    .all(|(now, shown)| now.name == shown.name && now.type_display() == shown.type_display);
            if same { Ok(Checked::Table(check)) } else { Err(ApplyError::TableChanged) }
        }
    }
}

async fn target(s: &Arc<Shared>, tab: TabId) -> Result<(Arc<Console>, ConsoleConfigView), ApplyError> {
    let console = s.console(tab).ok_or(ApplyError::NotConfigured)?;
    let config = console.inner.lock().await.config.clone();
    let connection = s.config(config.connection_id).ok_or(ApplyError::NotConfigured)?;
    if connection.database_kind != DatabaseKind::SqlServer {
        return Err(ApplyError::Unsupported { what: "PostgreSQL tables take edits without a support check.".into() });
    }
    s.connect(config.connection_id).await.map_err(|error| ApplyError::Connect { error })?;
    Ok((console, config))
}

pub async fn support(s: &Arc<Shared>, tab: TabId, schema: String, table: String) -> Result<EditSupport, ApplyError> {
    let (_, config) = target(s, tab).await?;
    let catalog = s.catalog(&config.db()).await.map_err(|_| ApplyError::NotConfigured)?;
    let relation = catalog.relation(&schema, &table).ok_or(ApplyError::NotConfigured)?;
    Ok(match check(s, &config, relation).await? {
        Checked::Table(check) => check.support(),
        Checked::Refused(reason) => EditSupport::Refused { reason },
    })
}

fn verb(kind: ChangeKind) -> &'static str {
    match kind {
        ChangeKind::Insert => "INSERT",
        ChangeKind::Update => "UPDATE",
        ChangeKind::Delete => "DELETE",
    }
}

/// The Output line for a Submit that applied nothing.
fn rolled_back(error: &ApplyError) -> QueryError {
    let reason = match error {
        ApplyError::Failed { error, .. } | ApplyError::Transaction { error } => error.message.clone(),
        ApplyError::RowChanged { .. } => "a row changed since the page read it".into(),
        ApplyError::RowMismatch { rows, .. } => format!("a row to change matched {rows} rows"),
        ApplyError::Locked => "a row or table is locked by another session".into(),
        ApplyError::TableChanged => "the table changed since the page read it".into(),
        ApplyError::Cancelled => "cancelled".into(),
        other => format!("{other:?}"),
    };
    session::error(format!("Staged edits were not applied: {reason}"))
}

pub async fn apply(
    s: &Arc<Shared>,
    tab: TabId,
    changes: TableChanges,
    confirmed: Option<WriteDigest>,
) -> Result<ApplySummary, ApplyError> {
    let console = s.console(tab).ok_or(ApplyError::NotConfigured)?;
    if console.inner.lock().await.config.mode != TxnMode::AutoCommit {
        return Err(ApplyError::Unsupported { what: "SQL Server staged edits need a native Auto-commit tab.".into() });
    }
    let (console, config) = target(s, tab).await?;
    let catalog = s.catalog(&config.db()).await.map_err(|_| ApplyError::NotConfigured)?;
    let relation = catalog.relation(&changes.schema, &changes.table).ok_or(ApplyError::NotConfigured)?;
    let check = match check(s, &config, relation).await? {
        Checked::Table(check) => check,
        Checked::Refused(reason) => return Err(ApplyError::Refused { reason }),
    };
    let plan =
        session::plan_mssql_edits(&changes, &check, &check.support(), &relation.qualified_name, session::LOCK_TIMEOUT)?;
    let needs_session = matches!(console.inner.lock().await.session, SessionSlot::Empty | SessionSlot::Lost { .. });
    let mut fresh = None;
    if needs_session {
        fresh = Some(s.open_tab_session(&config).await.map_err(|error| ApplyError::Connect { error })?);
    }

    let (mut session, stop, run_id) = {
        let mut inner = console.inner.lock().await;
        let current = s.config(inner.config.connection_id).ok_or(ApplyError::NotConfigured)?;
        if inner.config != config || current.database_kind != DatabaseKind::SqlServer {
            return Err(ApplyError::Busy);
        }
        if let Some(idle) = fresh {
            match inner.session {
                SessionSlot::Empty | SessionSlot::Lost { .. } => inner.session = SessionSlot::Idle(idle),
                SessionSlot::Idle(_) => {}
                SessionSlot::Busy { .. } => return Err(ApplyError::Busy),
            }
        }
        let shown_version = s.instance(current.id).and_then(|i| i.catalogs().get(&config.database).map(|c| c.version));
        if shown_version != Some(catalog.version) {
            return Err(ApplyError::TableChanged);
        }
        if inner.txn != TxnState::Idle {
            return Err(ApplyError::Session { reason: SessionRefusal::OpenTransaction });
        }
        let SessionSlot::Idle(idle) = &inner.session else { return Err(ApplyError::Busy) };
        let shown: Vec<&str> = plan.writes.iter().map(|w| w.display.as_str()).collect();
        let bound = serde_json::to_string(&(&plan, catalog.version, relation.oid)).expect("plans serialize");
        let target = writes::Target {
            saved: &current,
            opened: idle.opened(),
            console: &config,
            txn: inner.txn,
            unasked: &inner.unasked,
        };
        let checked = match writes::check_edits(&target, &shown, &bound, confirmed.as_ref()) {
            Ok(checked) => checked,
            Err(confirmation) => return Err(ApplyError::ConfirmWrites { confirmation }),
        };
        let run_id = s.next_run_id();
        let Some((session, stop)) = console.take_session(&mut inner, run_id, Work::Edit(checked)) else {
            return Err(ApplyError::Busy);
        };
        console.publish(&mut inner, s);
        (session, stop, run_id)
    };

    let shared = s.clone();
    let task_console = console.clone();
    let label = format!("{}.{}", changes.schema, changes.table);
    let applied = s.run_holding(console, run_id, async move {
        let s = &shared;
        let run = session.apply_edits(&plan, &stop).await;
        let mut lines = Vec::new();
        for (i, (write, (rows, elapsed_ms))) in plan.writes.iter().zip(&run.counts).enumerate() {
            lines.push(console::statement_line(i as u16, &write.display));
            lines.push(OutputBody::Command {
                statement: i as u16,
                tag: format!("{} {rows}", verb(write.change.kind)),
                elapsed_ms: *elapsed_ms,
            });
        }
        let failed_at = (run.counts.len() < plan.writes.len()).then_some(run.counts.len() as u16);
        let outcome = match run.outcome {
            EditOutcome::Applied => {
                lines.push(OutputBody::Notice {
                    severity: "INFO".into(),
                    message: format!("{} changes saved to {label} (committed)", plan.writes.len()),
                    detail: None,
                });
                Ok(ApplySummary { committed: true })
            }
            EditOutcome::RolledBack(error) => {
                let statement = matches!(error, ApplyError::Failed { .. }).then_some(failed_at).flatten();
                lines.push(OutputBody::Error { statement, error: rolled_back(&error) });
                Err(error)
            }
            EditOutcome::Unknown(error) => {
                lines.push(OutputBody::Error { statement: None, error: error.clone() });
                Err(ApplyError::OutcomeUnknown { error })
            }
        };
        let unknown = matches!(outcome, Err(ApplyError::OutcomeUnknown { .. }));
        let lost = session.closed_reason_settled(None).await;
        let txn = session.observed_txn().unwrap_or(TxnState::Idle);
        let returned = task_console
            .edit(s, run_id, |inner| {
                inner.unasked = WriteList::default();
                Console::return_settled(inner, session, txn, lost, unknown)
            })
            .await;
        task_console.output_all(s, run_id, lines);
        if let Ok(Some((db, owed))) = returned {
            s.schedule_refreshes(db, owed);
        }
        outcome
    });
    applied.await.unwrap_or_else(|| {
        Err(ApplyError::OutcomeUnknown {
            error: session::error("The edit stopped inside L1 Data and its session was closed."),
        })
    })
}
