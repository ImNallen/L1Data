pub(crate) mod agent;
pub(crate) mod claude;
pub(crate) mod limits;

use parking_lot::Mutex;
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::{Arc, Weak};
use std::time::Duration;

use serde_json::Value;

use crate::Shared;
use crate::console::now_ms;
use crate::model::{
    AgentError, AppEvent, Decision, EffortId, ModelId, ModelOption, ProbeOutcome, ProviderConfig, ProviderInfo,
    ProviderKind, ProviderState, ToolCallId, TurnOutcome,
};
use crate::shell_path::{self, AgentPath};

pub(crate) use crate::BoxFuture;

const HEALTH_TICK: Duration = Duration::from_secs(5);

pub(crate) trait Provider: Send + Sync {
    fn info(&self) -> ProviderInfo;
    fn fallback_model(&self) -> (ModelId, Option<EffortId>);
    fn launch_choice(&self, cfg: &ProviderConfig) -> (Option<ModelId>, Option<EffortId>);
    fn ignored_flags(&self, cfg: &ProviderConfig) -> Vec<String>;
    fn session_lost(&self, error: &AgentError) -> bool;
    fn quick_probe<'a>(
        &'a self,
        cfg: &'a ProviderConfig,
        host: &'a HostEnv,
    ) -> BoxFuture<'a, Result<Quick, ProbeOutcome>>;
    fn launch(&self, cfg: &ProviderConfig, host: &HostEnv, start: &SessionStart) -> Result<LaunchSpec, AgentError>;
    fn codec(&self, cfg: &ProviderConfig, open: Open) -> Box<dyn Codec>;
}

pub(crate) fn provider(kind: ProviderKind) -> &'static dyn Provider {
    match kind {
        ProviderKind::Claude => &claude::Claude,
    }
}

pub(crate) trait Codec: Send {
    fn open(&mut self) -> Vec<String>;
    fn user_turn(&mut self, text: &str) -> Vec<String>;
    fn interrupt(&mut self) -> Vec<String>;
    fn permission(&mut self, reply: &ReplyTo, decision: Decision, input: &Value, message: &str) -> Vec<String>;
    fn tool_result(&mut self, reply: &ReplyTo, result: &ModelResult) -> Vec<String>;
    fn read(&mut self, line: &str) -> Step;
}

#[derive(Debug, Default, PartialEq)]
pub(crate) struct Step {
    pub replies: Vec<String>,
    pub events: Vec<AgentEvent>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ReplyTo(Value);

impl ReplyTo {
    pub(crate) fn new(address: Value) -> Self {
        Self(address)
    }

    pub(crate) fn field(&self, key: &str) -> &Value {
        &self.0[key]
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelResult {
    pub text: String,
    pub is_error: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum AgentEvent {
    Ready(Handshake),
    TurnStarted { session_id: String },
    Text { block: BlockKey, delta: String },
    TextDone { block: BlockKey },
    ToolUse { call: ToolCallId, name: String, input: Value },
    Permission { reply: ReplyTo, call: ToolCallId, name: String, input: Value },
    ToolCall { reply: ReplyTo, call: ToolCallId, name: String, input: Value },
    ToolCancelled { call: ToolCallId },
    TurnEnded(TurnOutcome),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Handshake {
    pub version: Option<String>,
    pub plan: Option<String>,
    pub models: Vec<ModelOption>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct BlockKey {
    pub message: u32,
    pub index: u32,
}

#[derive(Clone, Copy)]
pub(crate) struct Open {
    pub system_prompt: &'static str,
    pub tools: &'static [ToolSpec],
}

impl Open {
    pub(crate) const PROBE: Open = Open { system_prompt: "", tools: &[] };
}

pub(crate) struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: fn() -> Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ProbeDepth {
    Quick,
    Full,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Quick {
    pub version: String,
    pub signed_in: bool,
    pub plan: Option<String>,
}

pub(crate) struct SessionStart {
    pub model: Option<ModelId>,
    pub effort: Option<EffortId>,
    pub resume: Option<String>,
}

pub(crate) struct LaunchSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env_set: Vec<(OsString, OsString)>,
    pub env_remove: Vec<OsString>,
    pub cwd: PathBuf,
}

pub(crate) struct HostEnv {
    pub path: OsString,
    pub home: PathBuf,
    /// `<data_dir>/assistant`. Fixed, so a resumed session finds the CLI's transcript.
    pub cwd: PathBuf,
}

impl HostEnv {
    pub(crate) async fn capture(agent_path: &AgentPath, cwd: PathBuf) -> Self {
        let home = shell_path::home();
        let path = shell_path::search_path(agent_path, &home).await;
        Self { path, home, cwd }
    }

    pub(crate) fn dirs(&self) -> Vec<PathBuf> {
        shell_path::dirs(&self.path)
    }

    pub(crate) fn expand_home(&self, path: &str) -> PathBuf {
        match path.strip_prefix('~') {
            Some("") => self.home.clone(),
            Some(rest) if rest.starts_with(std::path::is_separator) => self.home.join(&rest[1..]),
            _ => PathBuf::from(path),
        }
    }

    pub(crate) fn resolve(&self, configured: &str, default: &str) -> Result<PathBuf, String> {
        let name = configured.trim();
        let name = if name.is_empty() { default } else { name };
        if name.contains(std::path::is_separator) || name.starts_with('~') {
            let path = self.expand_home(name);
            let path = if path.is_absolute() {
                path
            } else {
                std::env::current_dir().map(|cwd| cwd.join(&path)).unwrap_or(path)
            };
            return shell_path::executable(&path).ok_or_else(|| path.display().to_string());
        }
        shell_path::find(&self.dirs(), name).ok_or_else(|| format!("{name} on PATH ({})", self.path.to_string_lossy()))
    }
}

pub(crate) fn capitalize(word: &str) -> String {
    let mut chars = word.chars();
    chars.next().map(|c| c.to_uppercase().collect::<String>() + chars.as_str()).unwrap_or_default()
}

pub(crate) struct Providers {
    entries: Mutex<HashMap<ProviderKind, ProviderEntry>>,
    agent_path: AgentPath,
    host: tokio::sync::OnceCell<HostEnv>,
}

#[derive(Default)]
struct ProviderEntry {
    rev: u64,
    checking: bool,
    checked_at_ms: Option<i64>,
    outcome: Option<ProbeOutcome>,
    running: bool,
    owed: Option<ProbeDepth>,
}

impl ProviderEntry {
    fn state(&self, kind: ProviderKind, cfg: Option<&ProviderConfig>) -> ProviderState {
        let p = provider(kind);
        ProviderState {
            provider: kind,
            rev: self.rev,
            info: p.info(),
            ignored_flags: cfg.map(|c| p.ignored_flags(c)).unwrap_or_default(),
            checking: self.checking,
            checked_at_ms: self.checked_at_ms,
            outcome: self.outcome.clone(),
        }
    }

    fn version(&self) -> Option<&str> {
        match self.outcome.as_ref()? {
            ProbeOutcome::Ready { version, .. } | ProbeOutcome::SignedOut { version } => Some(version),
            ProbeOutcome::Broken { version, .. } => version.as_deref(),
            ProbeOutcome::NotFound { .. } => None,
        }
    }

    fn models(&self) -> Option<&[ModelOption]> {
        match self.outcome.as_ref()? {
            ProbeOutcome::Ready { models, .. } => Some(models),
            _ => None,
        }
    }
}

impl Providers {
    pub(crate) fn new(agent_path: AgentPath) -> Self {
        Self { entries: Mutex::new(HashMap::new()), agent_path, host: tokio::sync::OnceCell::new() }
    }

    pub(crate) fn models(&self, kind: ProviderKind) -> Option<Vec<ModelOption>> {
        self.entries.lock().get(&kind)?.models().map(<[ModelOption]>::to_vec)
    }

    fn due(&self, kind: ProviderKind, every_secs: u32) -> bool {
        let entries = self.entries.lock();
        let Some(entry) = entries.get(&kind) else { return false };
        !entry.checking && entry.checked_at_ms.is_some_and(|at| now_ms() - at >= i64::from(every_secs) * 1000)
    }
}

impl Shared {
    pub(crate) async fn host_env(&self) -> &HostEnv {
        self.providers.host.get_or_init(|| HostEnv::capture(&self.providers.agent_path, self.assistant_dir())).await
    }

    pub(crate) fn assistant_dir(&self) -> PathBuf {
        self.data_dir.join("assistant")
    }

    fn provider_config(&self, kind: ProviderKind) -> Option<ProviderConfig> {
        self.settings().providers.into_iter().find(|c| c.provider == kind)
    }

    pub(crate) fn provider_states(&self) -> Vec<ProviderState> {
        let settings = self.settings();
        let entries = self.providers.entries.lock();
        ProviderKind::ALL
            .iter()
            .map(|&kind| {
                let cfg = settings.providers.iter().find(|c| c.provider == kind);
                entries.get(&kind).map_or_else(|| ProviderEntry::default().state(kind, cfg), |e| e.state(kind, cfg))
            })
            .collect()
    }

    pub(crate) fn probe_all(self: &Arc<Self>, depth: ProbeDepth) {
        for cfg in self.settings().providers {
            if cfg.enabled {
                self.probe(cfg.provider, depth);
            }
        }
    }

    pub(crate) fn probe(self: &Arc<Self>, kind: ProviderKind, depth: ProbeDepth) {
        {
            let mut entries = self.providers.entries.lock();
            let entry = entries.entry(kind).or_default();
            if entry.running {
                entry.owed = Some(entry.owed.map_or(depth, |owed| owed.max(depth)));
                return;
            }
            entry.running = true;
        }
        self.publish_provider(kind, |e| e.checking = true);
        let shared = self.clone();
        self.spawn(async move {
            let mut depth = depth;
            loop {
                let outcome = shared.run_probe(kind, depth).await;
                shared.publish_provider(kind, |e| {
                    e.checking = false;
                    e.checked_at_ms = Some(now_ms());
                    e.outcome = Some(outcome);
                });
                let owed = {
                    let mut entries = shared.providers.entries.lock();
                    let entry = entries.entry(kind).or_default();
                    let owed = entry.owed.take();
                    entry.running = owed.is_some();
                    owed
                };
                let Some(next) = owed else { return };
                depth = next;
                shared.publish_provider(kind, |e| e.checking = true);
            }
        });
    }

    fn publish_provider(&self, kind: ProviderKind, edit: impl FnOnce(&mut ProviderEntry)) {
        let cfg = self.provider_config(kind);
        let state = {
            let mut entries = self.providers.entries.lock();
            let entry = entries.entry(kind).or_default();
            edit(entry);
            entry.rev += 1;
            entry.state(kind, cfg.as_ref())
        };
        self.emit(AppEvent::ProviderChanged(state));
    }

    async fn run_probe(self: &Arc<Self>, kind: ProviderKind, depth: ProbeDepth) -> ProbeOutcome {
        let Some(cfg) = self.provider_config(kind) else { return ProbeOutcome::NotFound { binary: String::new() } };
        let host = self.host_env().await;
        let p = provider(kind);
        let quick = match p.quick_probe(&cfg, host).await {
            Ok(quick) => quick,
            Err(outcome) => return outcome,
        };
        if !quick.signed_in {
            return ProbeOutcome::SignedOut { version: quick.version };
        }
        let kept = {
            let entries = self.providers.entries.lock();
            let entry = entries.get(&kind);
            let same_version = entry.and_then(ProviderEntry::version) == Some(quick.version.as_str());
            if depth == ProbeDepth::Quick && same_version {
                entry.and_then(ProviderEntry::models).map(<[ModelOption]>::to_vec)
            } else {
                None
            }
        };
        if let Some(models) = kept {
            return ProbeOutcome::Ready { version: quick.version, plan: quick.plan, models };
        }
        let start = SessionStart { model: None, effort: None, resume: None };
        let handshake = async {
            let spec = p.launch(&cfg, host, &start).map_err(|e| match e {
                AgentError::NotFound { binary } => format!("{binary} is not an executable file"),
                AgentError::SignedOut => "signed out".to_owned(),
                AgentError::Exited { detail, .. } | AgentError::Provider { message: detail } => detail,
            })?;
            let (agent, handshake) =
                agent::Agent::start(self, spec, p.codec(&cfg, Open::PROBE)).await.map_err(|e| e.to_string())?;
            agent.close().await;
            Ok::<_, String>(handshake)
        };
        match handshake.await {
            Ok(h) => ProbeOutcome::Ready {
                version: h.version.unwrap_or(quick.version),
                plan: h.plan.or(quick.plan),
                models: h.models,
            },
            Err(message) => ProbeOutcome::Broken { version: Some(quick.version), message },
        }
    }

    pub(crate) fn reprobe_changed(self: &Arc<Self>, before: &[ProviderConfig], after: &[ProviderConfig]) {
        for cfg in after {
            let was = before.iter().find(|c| c.provider == cfg.provider);
            let changed = was.is_none_or(|w| {
                w.enabled != cfg.enabled
                    || w.binary_path != cfg.binary_path
                    || w.config_dir != cfg.config_dir
                    || w.launch_args != cfg.launch_args
            });
            if !changed {
                continue;
            }
            if cfg.enabled {
                self.probe(cfg.provider, ProbeDepth::Full);
            } else {
                self.publish_provider(cfg.provider, |_| {});
            }
        }
    }
}

pub(crate) async fn health_loop(shared: Weak<Shared>) {
    let mut tick = tokio::time::interval(HEALTH_TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let Some(shared) = shared.upgrade() else { return };
        let settings = shared.settings();
        if settings.provider_health_check_secs == 0 {
            continue;
        }
        for cfg in &settings.providers {
            if cfg.enabled && shared.providers.due(cfg.provider, settings.provider_health_check_secs) {
                shared.probe(cfg.provider, ProbeDepth::Quick);
            }
        }
    }
}
