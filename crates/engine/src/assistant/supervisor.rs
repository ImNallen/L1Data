use std::sync::Arc;

use tokio::task::JoinHandle;

use super::chat::{Chat, Ledger};
use super::reply;
use crate::Shared;
use crate::console::now_ms;
use crate::model::{AgentError, ChatStatus, Entry, TurnOutcome};

pub(super) async fn supervise(shared: Arc<Shared>, chat: Arc<Chat>, turn: JoinHandle<()>) {
    let Err(e) = turn.await else { return };
    let outcome = match e.try_into_panic() {
        Ok(panic) => {
            let text = panic
                .downcast_ref::<&str>()
                .map(|s| (*s).to_owned())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_default();
            TurnOutcome::Failed {
                error: AgentError::Provider { message: format!("The turn failed inside L1 Data: {text}") },
            }
        }
        Err(_) => TurnOutcome::Lost,
    };
    end_abruptly(&chat, &shared, outcome);
}

fn end_abruptly(chat: &Chat, shared: &Shared, outcome: TurnOutcome) {
    let rows = {
        let Some(mut inner) = chat.unless_deleted() else { return };
        if inner.turn.take().is_none() {
            return;
        }
        inner.ledger = Ledger::default();
        let mut rows = shared.store.chat_entries(chat.id).unwrap_or_default();
        for (seq, live) in &inner.live {
            let row = (*seq, live.turn, live.entry.clone());
            match rows.iter_mut().find(|(s, _, _)| s == seq) {
                Some(stored) => *stored = row,
                None => rows.push(row),
            }
        }
        rows.sort_by_key(|(seq, _, _)| *seq);
        rows
    };
    let mut fixes = reply::recover(&rows, now_ms());
    for (_, _, entry) in &mut fixes {
        if let Entry::End { outcome: o, .. } = entry {
            *o = outcome.clone();
        }
    }
    let next_seq = fixes.iter().map(|(seq, _, _)| seq + 1).max().unwrap_or(0);
    chat.store_entries(shared, &fixes);
    for (seq, turn, entry) in fixes {
        chat.publish_entry(shared, seq, turn, entry);
    }
    {
        let Some(mut inner) = chat.unless_deleted() else { return };
        inner.record.entry_count = inner.record.entry_count.max(next_seq);
        inner.record.updated_at_ms = now_ms();
        inner.status = ChatStatus::Idle;
        inner.live.clear();
    }
    chat.store_record(shared);
    chat.publish_state(shared);
}
