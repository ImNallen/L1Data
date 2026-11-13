pub(crate) mod chat;
mod context;
mod reply;
mod supervisor;
pub(crate) mod tools;
mod turn;

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use futures_util::future::join_all;
use futures_util::stream::{FuturesUnordered, StreamExt};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use chat::{ActiveTurn, Chat, Closing, TurnControl};
use supervisor::supervise;
use turn::TurnPlan;

use crate::Shared;
use crate::console::now_ms;
use crate::model::*;
use crate::provider::agent::CLOSE_GRACE;
use crate::provider::provider;
use crate::store::{ChatRecord, StoreError};

pub(crate) const RECAP_MAX_CHARS: usize = 16_000;
const TITLE_CHARS: usize = 60;
const STOP_GRACE: Duration = Duration::from_secs(5);
const QUIT_GRACE: Duration = Duration::from_secs(1);
const SHUTDOWN_WAIT: Duration = QUIT_GRACE.saturating_add(CLOSE_GRACE);
const ABORT_WAIT: Duration = Duration::from_secs(1);
pub const SHUTDOWN_BOUND: Duration = SHUTDOWN_WAIT.saturating_add(ABORT_WAIT);
const DELETE_WAIT: Duration = Duration::from_secs(5);

pub(crate) struct Assistant {
    chats: RwLock<HashMap<ChatId, Arc<Chat>>>,
    next_approval: AtomicU64,
}

impl Assistant {
    pub(crate) fn new() -> Self {
        Self { chats: RwLock::new(HashMap::new()), next_approval: AtomicU64::new(1) }
    }

    pub(crate) fn next_approval(&self) -> ApprovalId {
        ApprovalId(self.next_approval.fetch_add(1, Ordering::Relaxed))
    }

    fn get(&self, id: ChatId) -> Option<Arc<Chat>> {
        self.chats.read().unwrap().get(&id).cloned()
    }

    fn all(&self) -> Vec<Arc<Chat>> {
        self.chats.read().unwrap().values().cloned().collect()
    }

    pub(crate) fn running_databases(&self, connection: ConnectionId) -> BTreeSet<String> {
        self.all()
            .iter()
            .filter_map(|chat| chat.inner.lock().unwrap().turn.as_ref().and_then(|t| t.db.clone()))
            .filter(|db| db.connection_id == connection)
            .map(|db| db.database)
            .collect()
    }

    pub(crate) fn running_chats(&self) -> Vec<ChatId> {
        self.all().iter().filter(|chat| chat.inner.lock().unwrap().turn.is_some()).map(|chat| chat.id).collect()
    }

    pub(crate) fn live_states(&self) -> HashMap<ChatId, ChatState> {
        self.all().iter().map(|chat| (chat.id, chat.state())).collect()
    }
}

impl Shared {
    fn chat(&self, id: ChatId) -> Result<Arc<Chat>, StoreError> {
        if let Some(chat) = self.assistant.get(id) {
            return Ok(chat);
        }
        let (record, status) = match self.store.chat(id)? {
            Some(mut record) => {
                let entries = self.store.chat_entries(id)?;
                let fixes = reply::recover(&entries, now_ms());
                if !fixes.is_empty() {
                    self.store.put_entries(id, &fixes)?;
                }
                let entry_count = entries
                    .iter()
                    .chain(fixes.iter())
                    .map(|(seq, _, _)| seq + 1)
                    .max()
                    .unwrap_or(0)
                    .max(record.entry_count);
                if entry_count != record.entry_count {
                    record.entry_count = entry_count;
                    self.store.put_chat(id, &record)?;
                }
                (record, ChatStatus::Idle)
            }
            None => (self.draft_record(), ChatStatus::Draft),
        };
        let chat = self
            .assistant
            .chats
            .write()
            .unwrap()
            .entry(id)
            .or_insert_with(|| Arc::new(Chat::new(id, record, status)))
            .clone();
        Ok(chat)
    }

    fn draft_record(&self) -> ChatRecord {
        let now = now_ms();
        ChatRecord {
            title: String::new(),
            created_at_ms: now,
            updated_at_ms: now,
            options: self.default_options(),
            session: None,
            entry_count: 0,
            access: None,
            recap_floor: 0,
        }
    }

    fn default_options(&self) -> ChatOptions {
        let settings = self.settings();
        let cfg = settings.providers.iter().find(|c| c.enabled);
        let kind = cfg.map_or(ProviderKind::ALL[0], |c| c.provider);
        let p = provider(kind);
        let (asked_model, asked_effort) = cfg.map(|c| p.launch_choice(c)).unwrap_or_default();
        let models = self.providers.models(kind);
        let newest = self
            .store
            .chats(1)
            .ok()
            .and_then(|chats| chats.into_iter().next())
            .map(|(_, record)| record.options)
            .filter(|o| o.provider == kind);
        if asked_model.is_none()
            && asked_effort.is_none()
            && let (Some(prev), Some(models)) = (newest, models.as_deref())
            && let Some(model) = models.iter().find(|m| m.id == prev.model)
            && prev.effort.as_ref().is_none_or(|e| model.efforts.iter().any(|x| x.id == *e))
        {
            return ChatOptions { permission: Permission::AskFirst, ..prev };
        }
        let (model, effort) = match models.and_then(|models| models.into_iter().find(|m| m.recommended)) {
            Some(model) => {
                let listed = |id: &str| model.efforts.iter().find(|e| e.id.0 == id);
                let effort = asked_effort
                    .as_ref()
                    .and_then(|a| listed(&a.0))
                    .or_else(|| listed("high"))
                    .or(model.efforts.last())
                    .map(|e| e.id.clone());
                (model.id, effort)
            }
            None => p.fallback_model(),
        };
        ChatOptions { provider: kind, model, effort, permission: Permission::AskFirst }
    }

    fn validate_options(&self, options: &ChatOptions) -> Result<(), ChatError> {
        let Some(models) = self.providers.models(options.provider) else { return Ok(()) };
        let model = models
            .iter()
            .find(|m| m.id == options.model)
            .ok_or_else(|| ChatError::UnknownModel { model: options.model.clone() })?;
        if let Some(effort) = &options.effort
            && !model.efforts.iter().any(|e| e.id == *effort)
        {
            return Err(ChatError::UnsupportedEffort { effort: effort.clone() });
        }
        Ok(())
    }

    pub(crate) fn assistant_open(&self, id: ChatId) -> Result<ChatView, ChatError> {
        let store_error = |e: StoreError| ChatError::Store { message: e.to_string() };
        let chat = self.chat(id).map_err(store_error)?;
        let inner = chat.inner.lock().unwrap();
        let stored = if inner.stored() { self.store.chat_entries(id).map_err(store_error)? } else { Vec::new() };
        let mut entries: Vec<EntrySnapshot> = stored
            .into_iter()
            .map(|(seq, turn, entry)| EntrySnapshot { chat_id: id, seq, rev: inner.rev, turn, entry })
            .collect();
        for (seq, live) in &inner.live {
            match entries.iter_mut().find(|e| e.seq == *seq) {
                Some(slot) => *slot = live.clone(),
                None => entries.push(live.clone()),
            }
        }
        entries.sort_by_key(|e| e.seq);
        Ok(ChatView { state: Chat::state_of(id, &inner), entries })
    }

    pub(crate) fn assistant_configure(&self, id: ChatId, options: ChatOptions) -> Result<ChatState, ChatError> {
        let chat = self.chat(id).map_err(|e| ChatError::Store { message: e.to_string() })?;
        self.validate_options(&options)?;
        chat.inner.lock().unwrap().record.options = options;
        chat.store_record(self);
        Ok(chat.publish_state(self))
    }

    pub(crate) async fn assistant_send(
        self: &Arc<Self>,
        id: ChatId,
        mut request: SendRequest,
    ) -> Result<TurnAccepted, SendError> {
        let text = request.text.trim().to_owned();
        if text.is_empty() {
            return Err(SendError::EmptyMessage);
        }
        let chat = self.chat(id).map_err(|e| SendError::Store { message: e.to_string() })?;
        let access_guard = self.access.lock();
        let access = *access_guard;
        if access.mode == AssistantAccess::SchemaOnly
            && let Some(tab) = &mut request.tab
        {
            tab.attach = None;
            tab.text.clear();
            tab.title.clear();
            tab.target = RunTarget::All;
        }
        let options = chat.inner.lock().unwrap().record.options.clone();
        let enabled = self.settings().providers.iter().any(|c| c.provider == options.provider && c.enabled);
        if !enabled {
            return Err(SendError::ProviderOff { provider: options.provider });
        }
        if let Some(models) = self.providers.models(options.provider)
            && !models.iter().any(|m| m.id == options.model)
        {
            return Err(SendError::UnknownModel { model: options.model });
        }
        let attachment = match request.tab.as_ref().and_then(|t| t.attach.map(|r| (r, &t.title))) {
            Some((result, title)) => {
                Some(context::attachment(&*self.result_set(result).ok_or(SendError::AttachmentGone)?, title))
            }
            None => None,
        };
        let connection_name = request
            .tab
            .as_ref()
            .and_then(|t| t.db.as_ref())
            .and_then(|db| self.config(db.connection_id))
            .map(|c| c.name);
        let (seq, control_rx, closing) = {
            let Some(mut inner) = chat.unless_deleted() else { return Err(SendError::Busy) };
            if inner.turn.is_some() {
                return Err(SendError::Busy);
            }
            if inner.record.access != Some(access)
                || inner.record.session.as_ref().is_some_and(|s| s.access != Some(access))
            {
                inner.record.session = None;
                inner.record.recap_floor = inner.record.entry_count;
                inner.record.access = Some(access);
                inner.last_context = None;
            }
            if inner.status == ChatStatus::Draft {
                inner.record.title = text.lines().next().unwrap_or_default().chars().take(TITLE_CHARS).collect();
                inner.record.created_at_ms = now_ms();
            }
            let seq = inner.next_seq();
            let (control, control_rx) = mpsc::channel(4);
            inner.turn = Some(ActiveTurn {
                db: request.tab.as_ref().and_then(|t| t.db.clone()),
                control,
                supervisor: None,
                abort: None,
            });
            inner.status = ChatStatus::Running { turn: seq, started_at_ms: now_ms(), phase: TurnPhase::Starting };
            inner.record.updated_at_ms = now_ms();
            (seq, control_rx, inner.closing.clone())
        };
        chat.store_record(self);
        let context = request.tab.as_ref().map(|t| SentContext {
            database_kind: t.db.as_ref().and_then(|db| self.config(db.connection_id)).map(|c| c.database_kind),
            db: t.db.clone(),
            connection_name,
            tab_title: t.title.clone(),
        });
        let entry = Entry::User { at_ms: now_ms(), text: text.clone(), context, attachment };
        chat.store_entry(self, seq, seq, &entry);
        chat.publish_entry(self, seq, seq, entry);
        chat.publish_state(self);
        let plan =
            TurnPlan { chat: chat.clone(), turn: seq, options, user_text: text, tab: request.tab, closing, access };
        let turn = self.spawn(turn::run_turn(self.clone(), plan, control_rx));
        let abort = turn.abort_handle();
        let supervisor = self.spawn(supervise(self.clone(), chat.clone(), turn));
        if let Some(active) = chat.inner.lock().unwrap().turn.as_mut() {
            active.supervisor = Some(supervisor);
            active.abort = Some(abort);
        }
        Ok(TurnAccepted { turn: seq })
    }

    pub(crate) fn revoke_assistant_access(&self) {
        for chat in self.assistant.all() {
            chat.deny_pending(chat::Refusal::Stopped);
            {
                let mut inner = chat.inner.lock().unwrap();
                inner.record.session = None;
                inner.record.access = None;
                inner.record.recap_floor = inner.record.entry_count;
                inner.last_context = None;
                if let Some(active) = &inner.turn {
                    let _ = active.control.try_send(TurnControl::Stop { grace: Duration::ZERO });
                }
            }
            chat.store_record(self);
        }
    }

    pub(crate) fn assistant_stop(&self, id: ChatId) {
        if let Some(chat) = self.assistant.get(id)
            && let Some(active) = chat.inner.lock().unwrap().turn.as_ref()
        {
            let _ = active.control.try_send(TurnControl::Stop { grace: STOP_GRACE });
        }
    }

    pub(crate) fn assistant_approve(
        &self,
        id: ChatId,
        approval: ApprovalId,
        decision: Decision,
    ) -> Result<(), ApproveError> {
        self.assistant.get(id).ok_or(ApproveError::NotPending)?.answer(approval, decision)
    }

    pub(crate) async fn assistant_delete(&self, id: ChatId) {
        let loaded = self.assistant.get(id);
        let (record, running) = match &loaded {
            Some(chat) => {
                let mut inner = chat.inner.lock().unwrap();
                inner.status = ChatStatus::Deleted;
                let running = inner.turn.as_mut().map(|t| {
                    let _ = t.control.try_send(TurnControl::Stop { grace: QUIT_GRACE });
                    (t.supervisor.take(), t.abort.take())
                });
                (Some(inner.record.clone()), running)
            }
            None => (self.store.chat(id).ok().flatten(), None),
        };
        if let Some((Some(supervisor), abort)) = running
            && tokio::time::timeout(DELETE_WAIT, supervisor).await.is_err()
        {
            eprintln!("chat {} did not stop within {} s; its turn is aborted", id.0, DELETE_WAIT.as_secs());
            if let Some(abort) = abort {
                abort.abort();
            }
        }
        if let Some(closing) = loaded.as_ref().and_then(|chat| chat.inner.lock().unwrap().closing.clone()) {
            closing.wait().await;
        }
        if let Err(e) = self.store.delete_chat(id) {
            eprintln!("chat {} delete failed: {e}", id.0);
        }
        self.assistant.chats.write().unwrap().remove(&id);
        let record = record.unwrap_or_else(|| self.draft_record());
        self.emit(AppEvent::ChatChanged(ChatState {
            id,
            rev: self.next_chat_rev(),
            title: record.title,
            updated_at_ms: record.updated_at_ms,
            options: record.options,
            status: ChatStatus::Deleted,
            entry_count: record.entry_count,
        }));
    }

    pub(crate) async fn shutdown(&self) {
        let mut tasks: FuturesUnordered<JoinHandle<()>> = FuturesUnordered::new();
        let mut aborts = Vec::new();
        for chat in self.assistant.all() {
            if let Some(active) = chat.inner.lock().unwrap().turn.as_mut() {
                let _ = active.control.try_send(TurnControl::Stop { grace: QUIT_GRACE });
                tasks.extend(active.supervisor.take());
                aborts.extend(active.abort.take());
            }
        }
        let deadline = tokio::time::Instant::now() + SHUTDOWN_WAIT;
        let _ = tokio::time::timeout_at(deadline, drain(&mut tasks)).await;
        let closing: Vec<Closing> =
            self.assistant.all().iter().filter_map(|chat| chat.inner.lock().unwrap().closing.clone()).collect();
        let _ = tokio::time::timeout_at(deadline, join_all(closing.into_iter().map(Closing::wait))).await;
        for abort in aborts {
            abort.abort();
        }
        self.process_groups.kill_all();
        let _ = tokio::time::timeout(ABORT_WAIT, drain(&mut tasks)).await;
    }
}

async fn drain(tasks: &mut FuturesUnordered<JoinHandle<()>>) {
    while tasks.next().await.is_some() {}
}

#[cfg(test)]
mod tests {
    const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/assistant");

    fn sources() -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = std::fs::read_dir(DIR)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.is_file())
            .map(|p| {
                let name = p.file_name().unwrap().to_string_lossy().into_owned();
                let text = std::fs::read_to_string(&p).unwrap();
                let text = if name == "mod.rs" { text.split("#[cfg(test)]").next().unwrap().to_owned() } else { text };
                (name, text)
            })
            .collect();
        out.sort();
        assert!(out.iter().any(|(n, _)| n == "turn.rs"), "read no sources from {DIR}");
        out
    }

    #[test]
    fn no_file_under_assistant_names_a_provider_or_its_wire_format() {
        for (name, text) in sources() {
            let lower = text.to_lowercase();
            for word in ["claude", "mcp__", "stream-json", "--resume"] {
                assert!(
                    !lower.contains(word),
                    "{name} contains {word:?}; provider-specific text belongs in provider/<name>.rs behind the Codec and Provider traits"
                );
            }
        }
    }

    #[test]
    fn only_tools_rs_runs_model_sql() {
        for (name, text) in sources() {
            if name == "tools.rs" {
                assert!(text.contains("session.query("), "tools.rs is where queries run");
                continue;
            }
            assert!(
                !text.contains("session.query("),
                "{name} reaches a session's query stream; database access belongs in tools.rs, on its own read-only session"
            );
        }
    }
}
