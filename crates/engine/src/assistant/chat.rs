use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use serde_json::Value;
use tokio::sync::{mpsc, watch};
use tokio::task::{AbortHandle, JoinHandle};

use super::Assistant;
use super::tools::Ask;
use crate::Shared;
use crate::model::{
    AppEvent, ApprovalId, ApproveError, ChatId, ChatState, ChatStatus, DatabaseRef, Decision, Entry, EntrySnapshot,
    Permission, ToolCallId,
};
use crate::provider::agent::{Agent, CLOSE_GRACE};
use crate::store::ChatRecord;

const CLOSING_WAIT: Duration = Duration::from_secs(CLOSE_GRACE.as_secs() + 1);

pub(crate) struct Chat {
    pub id: ChatId,
    pub inner: Mutex<ChatInner>,
}

pub(crate) struct ChatInner {
    pub record: ChatRecord,
    pub status: ChatStatus,
    pub rev: u64,
    pub turn: Option<ActiveTurn>,
    pub closing: Option<Closing>,
    pub live: BTreeMap<u32, EntrySnapshot>,
    pub ledger: Ledger,
    pub last_context: Option<LastContext>,
}

impl ChatInner {
    pub(super) fn deleted(&self) -> bool {
        self.status == ChatStatus::Deleted
    }

    pub(super) fn stored(&self) -> bool {
        self.status != ChatStatus::Draft
    }

    pub(super) fn next_seq(&mut self) -> u32 {
        let seq = self.record.entry_count;
        self.record.entry_count += 1;
        seq
    }
}

pub(crate) struct ActiveTurn {
    pub db: Option<DatabaseRef>,
    pub control: mpsc::Sender<TurnControl>,
    pub supervisor: Option<JoinHandle<()>>,
    pub abort: Option<AbortHandle>,
}

pub(crate) enum TurnControl {
    Stop { grace: Duration },
}

#[derive(Clone)]
pub(crate) struct Closing(watch::Receiver<bool>);

impl Closing {
    pub(crate) fn start(shared: &Shared, agent: Agent) -> Self {
        let (reaped, handle) = watch::channel(false);
        shared.spawn(async move {
            agent.close().await;
            let _ = reaped.send(true);
        });
        Self(handle)
    }

    pub(crate) async fn wait(mut self) -> Closed {
        match tokio::time::timeout(CLOSING_WAIT, self.0.wait_for(|reaped| *reaped)).await {
            Ok(Ok(_)) => Closed::Reaped,
            Ok(Err(_)) => Closed::Dropped,
            Err(_) => Closed::TimedOut,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Closed {
    Reaped,
    Dropped,
    TimedOut,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct LastContext {
    pub session: String,
    pub db: DatabaseRef,
    pub catalog_version: u64,
    pub sql_hash: u64,
}

#[derive(Default)]
pub(crate) struct Ledger {
    calls: HashMap<ToolCallId, Gate>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Refusal {
    Policy(String),
    Declined,
    Stopped,
    Cancelled,
    Expired,
    Mismatch,
}

type Verdict = Result<(), Refusal>;

enum Gate {
    Pending { approval: ApprovalId, input: Value, answer: watch::Sender<Option<Verdict>> },
    Run { input: Value },
    Denied { why: Refusal },
}

impl Ledger {
    fn decide(&mut self, call: &ToolCallId, verdict: Verdict) -> bool {
        if !matches!(self.calls.get(call), Some(Gate::Pending { .. })) {
            return false;
        }
        let Some(Gate::Pending { input, answer, .. }) = self.calls.remove(call) else { unreachable!("checked above") };
        let settled = match &verdict {
            Ok(()) => Gate::Run { input },
            Err(why) => Gate::Denied { why: why.clone() },
        };
        self.calls.insert(call.clone(), settled);
        let _ = answer.send(Some(verdict));
        true
    }

    fn pending(&self) -> Vec<ToolCallId> {
        self.calls.iter().filter(|(_, g)| matches!(g, Gate::Pending { .. })).map(|(c, _)| c.clone()).collect()
    }
}

pub(crate) enum Gated {
    Run,
    Deny(Refusal),
    Wait { approval: ApprovalId, answer: watch::Receiver<Option<Verdict>>, created_here: bool },
}

impl Chat {
    pub(crate) fn new(id: ChatId, record: ChatRecord, status: ChatStatus) -> Self {
        Self {
            id,
            inner: Mutex::new(ChatInner {
                record,
                status,
                rev: 0,
                turn: None,
                closing: None,
                live: BTreeMap::new(),
                ledger: Ledger::default(),
                last_context: None,
            }),
        }
    }

    /// The lock, or `None` once the chat is deleted. Every store and publish path takes it
    /// here, so a deleted chat is inert without each caller remembering to check.
    pub(super) fn unless_deleted(&self) -> Option<MutexGuard<'_, ChatInner>> {
        let inner = self.inner.lock().unwrap();
        (!inner.deleted()).then_some(inner)
    }

    fn persists(&self) -> bool {
        self.unless_deleted().is_some_and(|inner| inner.stored())
    }

    pub(crate) fn permission(&self) -> Permission {
        self.inner.lock().unwrap().record.options.permission
    }

    /// The one answer to "may this call run?". Idempotent per call id: the provider asks before
    /// the call and the engine asks again at execution; the second ask returns what the first
    /// recorded, and a call whose input differs from the approved or pending one is denied.
    /// A current policy refusal takes precedence over any recorded decision.
    pub(crate) fn gate(&self, assistant: &Assistant, call: &ToolCallId, input: &Value, ask: &Ask) -> Gated {
        let mut inner = self.inner.lock().unwrap();
        if let Ask::Refuse(message) = ask {
            return Gated::Deny(Refusal::Policy(message.clone()));
        }
        match inner.ledger.calls.get(call) {
            Some(Gate::Run { input: approved }) => {
                return if approved == input { Gated::Run } else { Gated::Deny(Refusal::Mismatch) };
            }
            Some(Gate::Denied { why }) => return Gated::Deny(why.clone()),
            Some(Gate::Pending { approval, input: asked, answer }) => {
                return if asked == input {
                    Gated::Wait { approval: *approval, answer: answer.subscribe(), created_here: false }
                } else {
                    Gated::Deny(Refusal::Mismatch)
                };
            }
            None => {}
        }
        match ask {
            Ask::Allow => {
                inner.ledger.calls.insert(call.clone(), Gate::Run { input: input.clone() });
                Gated::Run
            }
            Ask::Refuse(message) => {
                let why = Refusal::Policy(message.clone());
                inner.ledger.calls.insert(call.clone(), Gate::Denied { why: why.clone() });
                Gated::Deny(why)
            }
            Ask::User(_) => {
                let approval = assistant.next_approval();
                let (answer, receiver) = watch::channel(None);
                inner.ledger.calls.insert(call.clone(), Gate::Pending { approval, input: input.clone(), answer });
                Gated::Wait { approval, answer: receiver, created_here: true }
            }
        }
    }

    pub(crate) fn answer(&self, approval: ApprovalId, decision: Decision) -> Result<(), ApproveError> {
        let mut inner = self.inner.lock().unwrap();
        let call = inner
            .ledger
            .calls
            .iter()
            .find(|(_, g)| matches!(g, Gate::Pending { approval: a, .. } if *a == approval))
            .map(|(c, _)| c.clone())
            .ok_or(ApproveError::NotPending)?;
        let verdict = match decision {
            Decision::Run => Ok(()),
            Decision::Deny => Err(Refusal::Declined),
        };
        inner.ledger.decide(&call, verdict);
        Ok(())
    }

    pub(crate) fn deny_pending(&self, why: Refusal) -> Vec<ToolCallId> {
        let mut inner = self.inner.lock().unwrap();
        let pending = inner.ledger.pending();
        for call in &pending {
            inner.ledger.decide(call, Err(why.clone()));
        }
        pending
    }

    pub(crate) fn cancel(&self, call: &ToolCallId) {
        self.inner.lock().unwrap().ledger.decide(call, Err(Refusal::Cancelled));
    }

    pub(crate) fn has_pending(&self) -> bool {
        self.inner.lock().unwrap().ledger.calls.values().any(|g| matches!(g, Gate::Pending { .. }))
    }

    pub(crate) fn is_pending(&self, call: &ToolCallId) -> bool {
        matches!(self.inner.lock().unwrap().ledger.calls.get(call), Some(Gate::Pending { .. }))
    }

    pub(crate) fn publish_entry(&self, shared: &Shared, seq: u32, turn: u32, entry: Entry) {
        let snapshot = EntrySnapshot { chat_id: self.id, seq, rev: shared.next_chat_rev(), turn, entry };
        {
            let Some(mut inner) = self.unless_deleted() else { return };
            inner.live.insert(seq, snapshot.clone());
        }
        shared.emit(AppEvent::ChatEntryChanged(snapshot));
    }

    pub(crate) fn store_entry(&self, shared: &Shared, seq: u32, turn: u32, entry: &Entry) {
        if self.persists()
            && let Err(e) = shared.store.put_entry(self.id, seq, turn, entry)
        {
            eprintln!("chat {} entry {seq} save failed: {e}", self.id.0);
        }
    }

    pub(super) fn store_entries(&self, shared: &Shared, rows: &[(u32, u32, Entry)]) {
        if self.persists()
            && let Err(e) = shared.store.put_entries(self.id, rows)
        {
            eprintln!("chat {} recovery failed: {e}", self.id.0);
        }
    }

    pub(crate) fn store_record(&self, shared: &Shared) {
        let record = match self.unless_deleted() {
            Some(inner) if inner.stored() => inner.record.clone(),
            _ => return,
        };
        if let Err(e) = shared.store.put_chat(self.id, &record) {
            eprintln!("chat {} save failed: {e}", self.id.0);
        }
    }

    pub(crate) fn publish_state(&self, shared: &Shared) -> ChatState {
        let state = {
            let Some(mut inner) = self.unless_deleted() else { return self.state() };
            inner.rev = shared.next_chat_rev();
            Self::state_of(self.id, &inner)
        };
        shared.emit(AppEvent::ChatChanged(state.clone()));
        state
    }

    pub(crate) fn state(&self) -> ChatState {
        Self::state_of(self.id, &self.inner.lock().unwrap())
    }

    pub(crate) fn state_of(id: ChatId, inner: &ChatInner) -> ChatState {
        ChatState {
            id,
            rev: inner.rev,
            title: inner.record.title.clone(),
            updated_at_ms: inner.record.updated_at_ms,
            options: inner.record.options.clone(),
            status: inner.status.clone(),
            entry_count: inner.record.entry_count,
        }
    }
}

pub(crate) async fn wait(mut answer: watch::Receiver<Option<Verdict>>) -> Verdict {
    match answer.wait_for(Option::is_some).await {
        Ok(verdict) => verdict.clone().expect("wait_for returns a Some"),
        Err(_) => Err(Refusal::Expired),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_refusal_precedes_a_cached_approval() {
        let record = ChatRecord {
            title: String::new(),
            created_at_ms: 0,
            updated_at_ms: 0,
            options: crate::model::ChatOptions {
                provider: crate::model::ProviderKind::ALL[0],
                model: crate::model::ModelId("test".into()),
                effort: None,
                permission: Permission::Auto,
            },
            session: None,
            entry_count: 0,
            access: None,
            recap_floor: 0,
        };
        let chat = Chat::new(ChatId(uuid::Uuid::new_v4()), record, ChatStatus::Idle);
        let assistant = Assistant::new();
        let call = ToolCallId("approved".into());
        let input = serde_json::json!({ "sql": "select 1" });
        assert!(matches!(chat.gate(&assistant, &call, &input, &Ask::Allow), Gated::Run));
        assert!(matches!(
            chat.gate(&assistant, &call, &input, &Ask::Refuse("schema only".into())),
            Gated::Deny(Refusal::Policy(_))
        ));
    }
}
