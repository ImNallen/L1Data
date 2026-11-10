use std::sync::Arc;
use std::time::{Duration, Instant};

use engine::model::*;
use engine::{AgentPath, Engine, EngineConfig, MemoryKeyStore};
use tokio::sync::broadcast;

const FAKE: &str = env!("CARGO_BIN_EXE_fake-claude");
const WAIT: Duration = Duration::from_secs(30);

struct Harness {
    engine: Arc<Engine>,
    events: broadcast::Receiver<AppEvent>,
    dir: tempfile::TempDir,
}

impl Harness {
    async fn open() -> Self {
        let h = Self::open_in(tempfile::tempdir().unwrap()).await;
        h.access(AssistantAccess::Full);
        h
    }

    async fn open_in(dir: tempfile::TempDir) -> Self {
        Self::open_with(dir, AgentPath::Fixed("".into())).await
    }

    async fn open_with(dir: tempfile::TempDir, agent_path: AgentPath) -> Self {
        let engine = Engine::open(EngineConfig { data_dir: dir.path().join("data"), keys: Arc::new(MemoryKeyStore::default()), agent_path, az_binary: None, user_known_hosts: None, updates: Arc::new(engine::ScriptedTransport::named("off").unwrap()) }).await.unwrap();
        let events = engine.subscribe();
        Self { engine, events, dir }
    }

    fn access(&self, access: AssistantAccess) {
        self.engine.set_settings(Settings { assistant_access: access, ..self.engine.settings() });
    }

    fn log_path(&self) -> std::path::PathBuf {
        self.dir.path().join("fake.log")
    }

    fn use_fake(&self, scenario: &str) {
        self.use_fake_with(scenario, "");
    }

    fn use_fake_with(&self, scenario: &str, extra: &str) {
        self.configure_provider(|p| {
            p.binary_path = FAKE.into();
            p.launch_args = format!("--fake-scenario {scenario} --fake-log {} {extra}", self.log_path().display());
        });
    }

    fn configure_provider(&self, edit: impl FnOnce(&mut ProviderConfig)) {
        let mut settings = self.engine.settings();
        edit(&mut settings.providers[0]);
        self.engine.set_settings(settings);
    }

    async fn wait_event(&mut self, what: &str, mut pick: impl FnMut(&AppEvent) -> bool) -> AppEvent {
        let started = Instant::now();
        loop {
            let remaining = WAIT.checked_sub(started.elapsed()).unwrap_or_else(|| panic!("timed out waiting for {what}"));
            match tokio::time::timeout(remaining, self.events.recv()).await {
                Ok(Ok(event)) if pick(&event) => return event,
                Ok(Ok(_)) => {}
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => {}
                Ok(Err(e)) => panic!("{e}"),
                Err(_) => panic!("timed out waiting for {what}"),
            }
        }
    }

    async fn wait_provider(&mut self, what: &str, done: impl Fn(&ProviderState) -> bool) -> ProviderState {
        match self.wait_event(what, |e| matches!(e, AppEvent::ProviderChanged(s) if done(s))).await {
            AppEvent::ProviderChanged(state) => state,
            _ => unreachable!(),
        }
    }

    async fn wait_outcome(&mut self, done: impl Fn(&ProbeOutcome) -> bool) -> ProbeOutcome {
        self.wait_provider("a probe outcome", |s| !s.checking && s.outcome.as_ref().is_some_and(&done)).await;
        self.settle().await.outcome.unwrap()
    }

    async fn settle(&mut self) -> ProviderState {
        loop {
            let state = self.engine.boot().providers.into_iter().next().unwrap();
            if !state.checking {
                return state;
            }
            self.wait_provider("the owed probe", |s| !s.checking).await;
        }
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.log_path()).unwrap_or_default()
    }

    fn handshakes(&self) -> usize {
        self.log().lines().filter(|l| l.starts_with("argv\t")).count()
    }

    fn turns(&self) -> Vec<(u32, Vec<String>)> {
        self.log()
            .lines()
            .filter_map(|l| l.strip_prefix("argv\t")?.split_once('\t'))
            .map(|(pid, argv)| (pid.parse().unwrap(), serde_json::from_str::<Vec<String>>(argv).unwrap()))
            .filter(|(_, argv)| argv.iter().any(|x| x == "--model"))
            .collect()
    }

    fn turn_argvs(&self) -> Vec<Vec<String>> {
        self.turns().into_iter().map(|(_, argv)| argv).collect()
    }

    fn turn_pids(&self) -> Vec<u32> {
        self.turns().into_iter().map(|(pid, _)| pid).collect()
    }

    fn stdin_lines(&self) -> Vec<String> {
        self.log().lines().filter_map(|l| l.strip_prefix("stdin\t")).map(str::to_owned).collect()
    }

    fn lifecycle(&self) -> Vec<(String, u32)> {
        self.log()
            .lines()
            .filter_map(|l| {
                let (kind, rest) = l.split_once('\t')?;
                if kind != "argv" && kind != "exit" {
                    return None;
                }
                Some((kind.to_owned(), rest.split('\t').next()?.parse().ok()?))
            })
            .collect()
    }

    fn chat(&self, permission: Permission) -> ChatId {
        let id = ChatId(uuid::Uuid::new_v4());
        let options = ChatOptions { provider: ProviderKind::Claude, model: ModelId("opus".into()), effort: Some(EffortId("high".into())), permission };
        self.engine.assistant_configure(id, options).unwrap();
        id
    }

    async fn send(&self, chat: ChatId, text: &str) -> TurnAccepted {
        self.engine.assistant_send(chat, SendRequest { text: text.into(), tab: None }).await.unwrap()
    }

    async fn wait_entry(&mut self, chat: ChatId, what: &str, pick: impl Fn(&EntrySnapshot) -> bool) -> EntrySnapshot {
        match self.wait_event(what, |e| matches!(e, AppEvent::ChatEntryChanged(s) if s.chat_id == chat && pick(s))).await {
            AppEvent::ChatEntryChanged(snapshot) => snapshot,
            _ => unreachable!(),
        }
    }

    async fn wait_end(&mut self, chat: ChatId) -> (TurnOutcome, Vec<String>) {
        let snapshot = self.wait_entry(chat, "the End entry", |s| matches!(s.entry, Entry::End { .. })).await;
        let Entry::End { outcome, suggestions, .. } = snapshot.entry else { unreachable!() };
        (outcome, suggestions)
    }

    async fn wait_approval(&mut self, chat: ChatId) -> (ApprovalId, ApprovalReason) {
        let snapshot = self.wait_entry(chat, "an approval", |s| matches!(s.entry, Entry::Tool { status: ToolStatus::AwaitingApproval { .. }, .. })).await;
        let Entry::Tool { status: ToolStatus::AwaitingApproval { approval, reason }, .. } = snapshot.entry else { unreachable!() };
        (approval, reason)
    }

    async fn wait_chat(&mut self, chat: ChatId, what: &str, pick: impl Fn(&ChatState) -> bool) -> ChatState {
        match self.wait_event(what, |e| matches!(e, AppEvent::ChatChanged(s) if s.id == chat && pick(s))).await {
            AppEvent::ChatChanged(state) => state,
            _ => unreachable!(),
        }
    }

    async fn wait_idle(&mut self, chat: ChatId) -> ChatState {
        self.wait_chat(chat, "the idle state", |s| s.status == ChatStatus::Idle).await
    }

    async fn events_above(&mut self, chat: ChatId, rev: u64, window: Duration) -> Vec<AppEvent> {
        let deadline = Instant::now() + window;
        let mut later = Vec::new();
        while let Ok(Ok(event)) = tokio::time::timeout(deadline.saturating_duration_since(Instant::now()), self.events.recv()).await {
            match &event {
                AppEvent::ChatChanged(s) if s.id == chat && s.rev > rev => later.push(event),
                AppEvent::ChatEntryChanged(s) if s.chat_id == chat && s.rev > rev => later.push(event),
                _ => {}
            }
        }
        later
    }

    async fn entries(&self, chat: ChatId) -> Vec<Entry> {
        self.engine.assistant_open(chat).await.unwrap().entries.into_iter().map(|s| s.entry).collect()
    }
}

fn ready(outcome: &ProbeOutcome) -> bool {
    matches!(outcome, ProbeOutcome::Ready { .. })
}

fn alive(pid: u32) -> bool {
    let out = std::process::Command::new("ps").args(["-o", "stat=", "-p", &pid.to_string()]).output().unwrap();
    let stat = String::from_utf8_lossy(&out.stdout);
    let stat = stat.trim();
    !stat.is_empty() && !stat.starts_with('Z')
}

async fn wait_dead(pid: u32) {
    dead_within(pid, Duration::from_secs(10)).await;
}

async fn dead_within(pid: u32, limit: Duration) {
    let started = Instant::now();
    while alive(pid) {
        assert!(started.elapsed() < limit, "process {pid} is still alive after {limit:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn text_of(entry: &Entry) -> String {
    let Entry::Text { blocks, .. } = entry else { return String::new() };
    blocks
        .iter()
        .map(|b| match b {
            ReplyBlock::Markdown { text } => text.clone(),
            ReplyBlock::Sql { sql, .. } => sql.clone(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn the_probe_reads_version_plan_and_models_and_boot_carries_them() {
    let mut h = Harness::open().await;
    h.use_fake("none");
    let outcome = h.wait_outcome(ready).await;
    let ProbeOutcome::Ready { version, plan, models } = outcome else { panic!("{outcome:?}") };
    assert_eq!((version.as_str(), plan.as_deref()), ("2.1.301", Some("Claude Max")));
    assert_eq!(models.iter().map(|m| m.id.0.as_str()).collect::<Vec<_>>(), ["opus", "fable", "sonnet", "haiku", "fake"], "the default alias is hidden");
    assert_eq!(models.iter().filter(|m| m.recommended).map(|m| m.label.as_str()).collect::<Vec<_>>(), ["Opus 5.5"]);
    assert_eq!(models[0].efforts.iter().map(|e| e.label.as_str()).collect::<Vec<_>>(), ["Low", "Medium", "High", "Extra high", "Max"]);
    assert!(models[4].efforts.is_empty());
    let booted = h.engine.boot().providers;
    assert_eq!(booted.len(), 1);
    assert_eq!((booted[0].provider, booted[0].info.default_binary.as_str(), booted[0].checking), (ProviderKind::Claude, "claude", false));
    assert!(matches!(&booted[0].outcome, Some(ProbeOutcome::Ready { version, .. }) if version == "2.1.301"));
    assert!(booted[0].checked_at_ms.is_some());
    let log = h.log();
    let argv = log.lines().find(|l| l.starts_with("argv\t")).unwrap();
    assert!(argv.contains("--setting-sources=") && argv.contains("--permission-prompt-tool=stdio") && !argv.contains("--resume"), "{argv}");
}

#[tokio::test]
async fn usage_limits_ask_claude_without_an_initialize() {
    let mut h = Harness::open().await;
    h.use_fake("none");
    h.wait_outcome(ready).await;
    std::fs::write(h.log_path(), "").unwrap();
    let report = h.engine.usage_limits().await;
    let [claude] = report.sources.as_slice() else { panic!("{report:?}") };
    let LimitsOutcome::Ready { plan, windows } = &claude.outcome else { panic!("{claude:?}") };
    assert_eq!((claude.source, claude.name.as_str(), plan.as_deref()), (ProviderKind::Claude, "Claude", Some("Max")));
    let shown: Vec<_> = windows.iter().map(|w| (w.id.as_str(), w.label.as_str(), w.left_percent, w.level)).collect();
    assert_eq!(shown, [("session", "Session", 10, LimitLevel::Low), ("weekly_all", "Weekly", 40, LimitLevel::Normal), ("weekly_scoped:Opus", "Weekly · Opus", 38, LimitLevel::Normal)]);
    let until_reset = windows[0].resets_at_ms.unwrap() - report.checked_at_ms;
    assert!((85 * 60_000..=86 * 60_000).contains(&until_reset), "the session resets 86 minutes after the read, not {until_reset} ms");
    let stdin: Vec<serde_json::Value> = h.stdin_lines().iter().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(stdin, [serde_json::json!({ "type": "control_request", "request_id": "l1-usage", "request": { "subtype": "get_usage", "skip_behaviors": true } })], "one get_usage and no initialize");

    h.configure_provider(|p| p.enabled = false);
    let off = h.engine.usage_limits().await;
    assert!(matches!(&off.sources[0].outcome, LimitsOutcome::Ready { windows, .. } if windows.len() == 3), "a disabled assistant still shows the account's limits: {off:?}");

    let signed_out = h.dir.path().join("signed-out");
    std::fs::create_dir_all(&signed_out).unwrap();
    h.configure_provider(|p| p.config_dir = signed_out.display().to_string());
    let out = h.engine.usage_limits().await;
    assert_eq!(out.sources[0].outcome, LimitsOutcome::SignedOut { sign_in_command: "claude auth login".into() }, "a disabled provider is never probed, so the read asks auth status itself");
}

#[tokio::test]
async fn an_empty_agent_path_finds_no_claude() {
    let mut h = Harness::open().await;
    assert_eq!(h.wait_outcome(|_| true).await, ProbeOutcome::NotFound { binary: "claude on PATH ()".into() });
}

#[tokio::test]
async fn claude_installed_on_the_agent_path_is_found_without_a_binary_path() {
    let bin = tempfile::tempdir().unwrap();
    std::fs::copy(FAKE, bin.path().join(format!("claude{}", std::env::consts::EXE_SUFFIX))).unwrap();
    let mut h = Harness::open_with(tempfile::tempdir().unwrap(), AgentPath::Fixed(bin.path().into())).await;
    let outcome = h.wait_outcome(|_| true).await;
    assert!(ready(&outcome), "the native installer's claude{} is found: {outcome:?}", std::env::consts::EXE_SUFFIX);
}

#[cfg(windows)]
#[tokio::test]
async fn an_npm_claude_cmd_on_the_agent_path_is_found_and_runs() {
    let bin = tempfile::tempdir().unwrap();
    std::fs::write(bin.path().join("claude"), "#!/bin/sh\nexec node cli.js \"$@\"\n").unwrap();
    std::fs::write(bin.path().join("claude.cmd"), format!("@\"{FAKE}\" %*\r\n")).unwrap();
    let mut h = Harness::open_with(tempfile::tempdir().unwrap(), AgentPath::Fixed(bin.path().into())).await;
    let outcome = h.wait_outcome(|_| true).await;
    assert!(ready(&outcome), "npm's claude.cmd runs through cmd.exe, and its extensionless shell shim does not: {outcome:?}");
}

#[tokio::test]
async fn a_missing_binary_probes_not_found_and_a_signed_out_config_dir_probes_signed_out() {
    let mut h = Harness::open().await;
    h.configure_provider(|p| p.binary_path = "/nope".into());
    assert_eq!(h.wait_outcome(|o| matches!(o, ProbeOutcome::NotFound { .. })).await, ProbeOutcome::NotFound { binary: "/nope".into() });
    let signed_out = h.dir.path().join("signed-out");
    std::fs::create_dir_all(&signed_out).unwrap();
    h.configure_provider(|p| {
        p.binary_path = FAKE.into();
        p.config_dir = signed_out.display().to_string();
    });
    assert_eq!(h.wait_outcome(|o| matches!(o, ProbeOutcome::SignedOut { .. })).await, ProbeOutcome::SignedOut { version: "2.1.301".into() });
}

#[tokio::test]
async fn provider_refresh_works_from_a_thread_without_a_runtime_and_reprobes() {
    let mut h = Harness::open().await;
    h.use_fake("none");
    h.wait_outcome(ready).await;
    let (before, handshakes) = (h.settle().await, h.handshakes());
    let caller = h.engine.clone();
    std::thread::spawn(move || caller.provider_refresh()).join().expect("provider_refresh panicked off the runtime");
    let after = h.wait_provider("the refresh's outcome", |s| !s.checking && s.checked_at_ms > before.checked_at_ms).await;
    assert_eq!(after.outcome, before.outcome);
    assert_eq!(h.handshakes(), handshakes + 1, "a refresh is a full probe, so it ran the handshake once more");
}

#[tokio::test]
async fn an_unchanged_setting_does_not_reprobe_but_the_health_loop_does() {
    let mut h = Harness::open().await;
    h.use_fake("none");
    h.wait_outcome(ready).await;
    let (before, handshakes) = (h.settle().await, h.handshakes());
    let mut settings = h.engine.settings();
    settings.provider_health_check_secs = 1;
    h.engine.set_settings(settings);
    let after = h.wait_provider("the health loop's probe", |s| !s.checking && s.checked_at_ms > before.checked_at_ms).await;
    assert_eq!(after.outcome, before.outcome, "a quick probe keeps the models of the last full one");
    assert_eq!(h.handshakes(), handshakes, "the interval change alone re-probed nothing, and the quick probe ran no handshake");
}

#[tokio::test]
async fn a_turn_streams_the_reply_into_blocks_and_ends_with_suggestions() {
    let mut h = Harness::open().await;
    h.use_fake("card");
    let chat = h.chat(Permission::Auto);
    let accepted = h.send(chat, "Which regions grew the most compared with the 90 days before?").await;
    assert_eq!(accepted.turn, 0);
    let running = h.wait_chat(chat, "the running state", |s| matches!(s.status, ChatStatus::Running { .. })).await;
    assert_eq!(running.title, "Which regions grew the most compared with the 90 days before", "the first line, cut at 60 chars");
    let streaming = h.wait_entry(chat, "a streaming text", |s| matches!(&s.entry, Entry::Text { done: false, blocks } if !blocks.is_empty())).await;
    assert_eq!(streaming.turn, 0);
    let (outcome, suggestions) = h.wait_end(chat).await;
    assert_eq!(outcome, TurnOutcome::Completed);
    assert_eq!(suggestions, ["Show growth as a percentage", "Only EU regions"]);
    let entries = h.entries(chat).await;
    assert_eq!(entries.len(), 3, "{entries:?}");
    assert!(matches!(&entries[0], Entry::User { text, context: None, attachment: None, .. } if text.starts_with("Which regions")));
    let Entry::Text { blocks, done: true } = &entries[1] else { panic!("{:?}", entries[1]) };
    assert_eq!(blocks.len(), 3, "{blocks:?}");
    assert_eq!(blocks[0], ReplyBlock::Markdown { text: "This compares paid revenue in the last 90 days with the 90 days before that, per region:".into() });
    let ReplyBlock::Sql { sql, relations, vetted, complete } = &blocks[1] else { panic!("{:?}", blocks[1]) };
    assert!(sql.starts_with("SELECT c.region") && sql.ends_with("ORDER BY growth DESC;"), "{sql}");
    assert_eq!((relations.clone(), *vetted, *complete), (vec!["orders".to_owned(), "customers".to_owned()], true, true));
    assert_eq!(blocks[2], ReplyBlock::Markdown { text: "`FILTER` keeps both periods in one scan of `orders`, which already has an index on `created_at`.".into() });
    let idle = h.wait_chat(chat, "the idle state", |s| s.status == ChatStatus::Idle).await;
    assert_eq!(idle.entry_count, 3);
    let listed = h.engine.boot().chats;
    assert_eq!(listed.iter().map(|c| c.id).collect::<Vec<_>>(), [chat]);
    assert!(h.stdin_lines().iter().any(|l| l.contains("Which regions grew") && l.contains("<l1_context>")), "the prompt carries the context block");
}

#[tokio::test]
async fn a_second_send_during_a_turn_is_busy() {
    let mut h = Harness::open().await;
    h.use_fake("slow");
    let chat = h.chat(Permission::Auto);
    h.send(chat, "one").await;
    h.wait_entry(chat, "streaming text", |s| matches!(s.entry, Entry::Text { .. })).await;
    assert_eq!(h.engine.assistant_send(chat, SendRequest { text: "two".into(), tab: None }).await.unwrap_err(), SendError::Busy);
    h.engine.assistant_stop(chat);
    assert_eq!(h.wait_end(chat).await.0, TurnOutcome::Stopped);
    assert_eq!(h.send(chat, "two").await.turn, 3, "a new turn is accepted as soon as the old one ended");
    h.wait_entry(chat, "the second turn's text", |s| s.turn == 3 && matches!(s.entry, Entry::Text { .. })).await;
    h.engine.assistant_stop(chat);
    assert_eq!(h.wait_end(chat).await.0, TurnOutcome::Stopped);
}

#[tokio::test]
async fn ask_first_waits_for_the_user_then_runs_once_and_a_second_answer_is_not_pending() {
    let mut h = Harness::open().await;
    h.use_fake("query");
    let chat = h.chat(Permission::AskFirst);
    h.send(chat, "How many orders?").await;
    let (approval, reason) = h.wait_approval(chat).await;
    assert_eq!(reason, ApprovalReason::AskFirst);
    let waiting = h.wait_chat(chat, "the waiting phase", |s| matches!(s.status, ChatStatus::Running { phase: TurnPhase::AwaitingApproval, .. })).await;
    assert_eq!(waiting.entry_count, 2);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!h.stdin_lines().iter().any(|l| l.contains("\"behavior\"")), "nothing was answered before the user did");
    assert_eq!(h.engine.assistant_approve(chat, approval, Decision::Run), Ok(()));
    assert_eq!(h.engine.assistant_approve(chat, approval, Decision::Run), Err(ApproveError::NotPending));
    assert_eq!(h.wait_end(chat).await.0, TurnOutcome::Completed);
    let entries = h.entries(chat).await;
    let Entry::Tool { tool, status, .. } = &entries[1] else { panic!("{:?}", entries[1]) };
    assert_eq!(tool, &ToolUse::RunQuery { sql: "select count(*) from orders".into(), purpose: "Count the orders".into(), relations: vec!["orders".into()] });
    assert_eq!(status, &ToolStatus::Failed { message: "No database is selected. Ask the user to open a tab on a connection.".into() }, "the call ran: without a database it fails there, not at the gate");
    assert!(text_of(&entries[2]).contains("The tool failed: No database is selected"));
    let allows: Vec<String> = h.stdin_lines().into_iter().filter(|l| l.contains("\"behavior\":\"allow\"")).collect();
    assert_eq!(allows.len(), 1);
    assert!(allows[0].contains("\"updatedInput\":{\"purpose\":\"Count the orders\",\"sql\":\"select count(*) from orders\"}"), "{}", allows[0]);
    assert_eq!(h.engine.assistant_approve(chat, approval, Decision::Deny), Err(ApproveError::NotPending), "answered turns stay answered");
}

#[tokio::test]
async fn deny_reaches_the_model_as_an_error_result() {
    let mut h = Harness::open().await;
    h.use_fake("query");
    let chat = h.chat(Permission::AskFirst);
    h.send(chat, "How many orders?").await;
    let (approval, _) = h.wait_approval(chat).await;
    assert_eq!(h.engine.assistant_approve(chat, approval, Decision::Deny), Ok(()));
    assert_eq!(h.wait_end(chat).await.0, TurnOutcome::Completed);
    let entries = h.entries(chat).await;
    assert!(matches!(&entries[1], Entry::Tool { status: ToolStatus::Denied { stopped: false }, .. }), "{:?}", entries[1]);
    assert!(text_of(&entries[2]).contains("The call was declined: The user declined this tool call."), "{}", text_of(&entries[2]));
    assert!(h.stdin_lines().iter().any(|l| l.contains("\"behavior\":\"deny\"") && l.contains("The user declined this tool call.")));
    assert!(!h.stdin_lines().iter().any(|l| l.contains("tools/call") || l.contains("mcp_response\":{\"id\":2")), "no call ran");
}

#[tokio::test]
async fn a_tools_call_whose_arguments_differ_from_the_approved_input_is_refused() {
    let mut h = Harness::open().await;
    h.use_fake("query-tampered");
    let chat = h.chat(Permission::Auto);
    h.send(chat, "How many orders?").await;
    assert_eq!(h.wait_end(chat).await.0, TurnOutcome::Completed);
    let entries = h.entries(chat).await;
    assert!(matches!(&entries[1], Entry::Tool { status: ToolStatus::Failed { message }, .. } if message == "This call was not approved."), "{:?}", entries[1]);
    assert!(text_of(&entries[2]).contains("The tool failed: This call was not approved."));
    assert!(h.stdin_lines().iter().any(|l| l.contains("\"isError\":true") && l.contains("This call was not approved.")));
}

#[tokio::test]
async fn a_call_with_other_arguments_while_its_ask_waits_is_refused_and_the_ask_stays_answerable() {
    let mut h = Harness::open().await;
    h.use_fake("call-before-answer");
    let chat = h.chat(Permission::AskFirst);
    h.send(chat, "How many orders?").await;
    let (approval, _) = h.wait_approval(chat).await;
    let refused = |lines: &[String]| lines.iter().position(|l| l.contains("\"isError\":true") && l.contains("This call was not approved."));
    let started = Instant::now();
    while refused(&h.stdin_lines()).is_none() {
        assert!(started.elapsed() < Duration::from_secs(10), "the call with other arguments was not refused while the ask waited: {:?}", h.stdin_lines());
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let waiting = h.entries(chat).await;
    assert!(matches!(&waiting[1], Entry::Tool { tool: ToolUse::RunQuery { sql, .. }, status: ToolStatus::AwaitingApproval { .. }, .. } if sql == "select count(*) from orders"), "the card still asks about the asked input: {:?}", waiting[1]);
    h.engine.assistant_approve(chat, approval, Decision::Run).unwrap();
    assert_eq!(h.wait_end(chat).await.0, TurnOutcome::Completed);
    let entries = h.entries(chat).await;
    assert!(matches!(&entries[1], Entry::Tool { tool: ToolUse::RunQuery { sql, .. }, status: ToolStatus::Expired, .. } if sql == "select count(*) from orders"), "approved for the asked input and never called with it: {:?}", entries[1]);
    assert!(text_of(&entries[2]).contains("The tool failed: This call was not approved.; the ask was allowed"), "{}", text_of(&entries[2]));
    let lines = h.stdin_lines();
    let allowed = lines.iter().position(|l| l.contains("\"behavior\":\"allow\"")).unwrap();
    assert!(refused(&lines).unwrap() < allowed, "the refusal waited for the user");
    assert!(lines[allowed].contains("\"updatedInput\":{\"purpose\":\"Count the orders\",\"sql\":\"select count(*) from orders\"}"), "{}", lines[allowed]);
    assert!(!lines.iter().any(|l| l.contains("\"mcp_response\"") && l.contains("No database is selected")), "a query ran: {lines:?}");
}

#[tokio::test]
async fn the_card_and_the_allow_reply_carry_the_input_the_provider_asked_about() {
    let mut h = Harness::open().await;
    h.use_fake("ask-differs");
    let chat = h.chat(Permission::AskFirst);
    h.send(chat, "How many customers?").await;
    let (approval, _) = h.wait_approval(chat).await;
    let waiting = h.entries(chat).await;
    assert!(matches!(&waiting[1], Entry::Tool { tool: ToolUse::RunQuery { sql, .. }, status: ToolStatus::AwaitingApproval { .. }, .. } if sql == "select count(*) from customers"), "the card shows the ask, not the tool_use block: {:?}", waiting[1]);
    h.engine.assistant_approve(chat, approval, Decision::Run).unwrap();
    assert_eq!(h.wait_end(chat).await.0, TurnOutcome::Completed);
    let entries = h.entries(chat).await;
    assert!(matches!(&entries[1], Entry::Tool { tool: ToolUse::RunQuery { sql, .. }, status: ToolStatus::Failed { message }, .. } if sql == "select count(*) from customers" && message.starts_with("No database is selected")), "the approved call ran: {:?}", entries[1]);
    let allow = h.stdin_lines().into_iter().find(|l| l.contains("\"behavior\":\"allow\"")).unwrap();
    assert!(allow.contains("\"updatedInput\":{\"purpose\":\"Count the orders\",\"sql\":\"select count(*) from customers\"}"), "{allow}");
}

#[tokio::test]
async fn stop_denies_a_pending_approval() {
    let mut h = Harness::open().await;
    h.use_fake("query");
    let chat = h.chat(Permission::AskFirst);
    h.send(chat, "How many orders?").await;
    let (approval, _) = h.wait_approval(chat).await;
    h.engine.assistant_stop(chat);
    assert_eq!(h.wait_end(chat).await.0, TurnOutcome::Stopped);
    assert_eq!(h.engine.assistant_approve(chat, approval, Decision::Run), Err(ApproveError::NotPending));
    let entries = h.entries(chat).await;
    assert!(matches!(&entries[1], Entry::Tool { status: ToolStatus::Denied { stopped: true }, .. }), "{:?}", entries[1]);
    assert!(h.stdin_lines().iter().any(|l| l.contains("\"behavior\":\"deny\"") && l.contains("Stopped by the user.")));
}

#[tokio::test]
async fn after_stop_an_auto_query_is_denied_at_the_ask_and_at_the_call() {
    let mut h = Harness::open().await;
    h.use_fake("stop-then-query");
    let chat = h.chat(Permission::Auto);
    h.send(chat, "go").await;
    h.wait_entry(chat, "streaming text", |s| matches!(&s.entry, Entry::Text { done: false, blocks } if !blocks.is_empty())).await;
    h.engine.assistant_stop(chat);
    assert_eq!(h.wait_end(chat).await.0, TurnOutcome::Stopped);
    let lines = h.stdin_lines();
    assert!(lines.iter().any(|l| l.contains("\"behavior\":\"deny\"") && l.contains("Stopped by the user.")), "the ask was denied at once");
    assert!(lines.iter().any(|l| l.contains("\"isError\":true") && l.contains("Stopped by the user.")), "the call was refused at once");
    let entries = h.entries(chat).await;
    assert!(entries.iter().any(|e| matches!(e, Entry::Tool { status: ToolStatus::Denied { stopped: true }, .. })), "{entries:?}");
}

#[tokio::test]
async fn auto_runs_a_read_query_without_asking() {
    let mut h = Harness::open().await;
    h.use_fake("query");
    let chat = h.chat(Permission::Auto);
    h.send(chat, "How many orders?").await;
    assert_eq!(h.wait_end(chat).await.0, TurnOutcome::Completed);
    let entries = h.entries(chat).await;
    assert!(matches!(&entries[1], Entry::Tool { status: ToolStatus::Failed { message }, .. } if message.starts_with("No database is selected")), "{:?}", entries[1]);
    assert!(h.stdin_lines().iter().any(|l| l.contains("\"behavior\":\"allow\"")));
    assert!(!entries.iter().any(|e| matches!(e, Entry::Tool { status: ToolStatus::AwaitingApproval { .. }, .. })));
}

#[tokio::test]
async fn stop_ends_the_turn_stopped_and_the_process_is_gone() {
    let mut h = Harness::open().await;
    h.use_fake("slow");
    let chat = h.chat(Permission::Auto);
    h.send(chat, "go").await;
    h.wait_entry(chat, "streaming text", |s| matches!(&s.entry, Entry::Text { done: false, blocks } if !blocks.is_empty())).await;
    let pid = h.turn_pids()[0];
    assert!(alive(pid));
    let caller = h.engine.clone();
    std::thread::spawn(move || caller.assistant_stop(chat)).join().expect("assistant_stop panicked off the runtime");
    let (outcome, suggestions) = h.wait_end(chat).await;
    assert_eq!((outcome, suggestions), (TurnOutcome::Stopped, Vec::new()));
    wait_dead(pid).await;
    let entries = h.entries(chat).await;
    assert!(matches!(&entries[1], Entry::Text { done: true, blocks } if text_of(&entries[1]).starts_with("tick 1 ") && !blocks.is_empty()), "{:?}", entries[1]);
    assert!(h.stdin_lines().iter().any(|l| l.contains("\"subtype\":\"interrupt\"")));
    h.engine.assistant_stop(chat);
}

#[tokio::test]
async fn a_crash_ends_the_turn_failed_with_the_stderr_tail() {
    let mut h = Harness::open().await;
    h.use_fake("crash");
    let chat = h.chat(Permission::Auto);
    h.send(chat, "go").await;
    let (outcome, _) = h.wait_end(chat).await;
    let TurnOutcome::Failed { error: AgentError::Exited { code, detail } } = outcome else { panic!("{outcome:?}") };
    assert_eq!(code, Some(1));
    assert!(detail.contains("fake-claude: simulated crash"), "{detail}");
    let entries = h.entries(chat).await;
    assert_eq!(text_of(&entries[1]), "Working on it");
    assert_eq!(h.wait_chat(chat, "idle", |s| s.status == ChatStatus::Idle).await.entry_count, 3);
}

#[tokio::test]
async fn a_lost_session_falls_back_to_a_new_one_with_a_notice_and_a_recap() {
    let mut h = Harness::open().await;
    h.use_fake("resume-lost");
    let chat = h.chat(Permission::Auto);
    h.send(chat, "first").await;
    assert_eq!(h.wait_end(chat).await.0, TurnOutcome::Completed);
    let first_session = format!("fake-session-{}", h.turn_pids()[0]);
    h.send(chat, "second").await;
    assert_eq!(h.wait_end(chat).await.0, TurnOutcome::Completed);
    let argvs = h.turn_argvs();
    assert_eq!(argvs.len(), 3, "first turn, the failed resume, the retry: {argvs:?}");
    assert_eq!(argvs[1].iter().position(|a| a == "--resume").map(|i| argvs[1][i + 1].as_str()), Some(first_session.as_str()));
    assert!(!argvs[2].iter().any(|a| a == "--resume"));
    let entries = h.entries(chat).await;
    let notice = entries.iter().find_map(|e| match e {
        Entry::Notice { text } => Some(text.clone()),
        _ => None,
    });
    assert_eq!(notice.as_deref(), Some("Claude could not resume this chat, so it was restarted with a summary of the conversation."));
    assert!(h.stdin_lines().iter().any(|l| l.contains("Earlier in this conversation, restored after the session was lost") && l.contains("User: first")), "the retry's prompt carries the recap");
    h.send(chat, "third").await;
    assert_eq!(h.wait_end(chat).await.0, TurnOutcome::Completed);
    let argvs = h.turn_argvs();
    assert_eq!(argvs[3].iter().position(|a| a == "--resume").map(|i| argvs[3][i + 1].clone()), Some(format!("fake-session-{}", h.turn_pids()[2])), "the next turn resumes the replacement session");
}

#[tokio::test]
async fn a_session_lost_at_the_first_result_is_restarted_with_a_recap_that_leaves_out_this_turn() {
    let mut h = Harness::open().await;
    h.use_fake("resume-lost-late");
    let chat = h.chat(Permission::Auto);
    h.send(chat, "first").await;
    assert_eq!(h.wait_end(chat).await.0, TurnOutcome::Completed);
    let first_session = format!("fake-session-{}", h.turn_pids()[0]);
    h.send(chat, "second").await;
    assert_eq!(h.wait_end(chat).await.0, TurnOutcome::Completed);
    let argvs = h.turn_argvs();
    assert_eq!(argvs.len(), 3, "first turn, the failed resume, the retry: {argvs:?}");
    assert_eq!(argvs[1].iter().position(|a| a == "--resume").map(|i| argvs[1][i + 1].as_str()), Some(first_session.as_str()));
    assert!(!argvs[2].iter().any(|a| a == "--resume"));
    let entries = h.entries(chat).await;
    assert!(matches!(&entries[4], Entry::Notice { .. }), "the notice follows the second turn's user entry: {entries:?}");
    let recaps: Vec<String> = h.stdin_lines().into_iter().filter(|l| l.contains("Earlier in this conversation, restored after the session was lost")).collect();
    assert_eq!(recaps.len(), 1);
    assert!(recaps[0].contains("User: first") && !recaps[0].contains("User: second"), "the recap stops before the current turn: {}", recaps[0]);
}

#[tokio::test]
async fn a_resume_that_fails_for_another_reason_keeps_the_session_and_fails_the_turn() {
    let mut h = Harness::open().await;
    h.use_fake("card");
    let chat = h.chat(Permission::Auto);
    h.send(chat, "first").await;
    h.wait_end(chat).await;
    let session = format!("fake-session-{}", h.turn_pids()[0]);
    h.use_fake("resume-crash");
    h.send(chat, "second").await;
    let (outcome, _) = h.wait_end(chat).await;
    assert!(matches!(&outcome, TurnOutcome::Failed { error: AgentError::Exited { code: Some(1), detail } } if detail.contains("fake-claude: boom")), "{outcome:?}");
    assert_eq!(h.turn_argvs().len(), 2, "no retry");
    assert!(!h.entries(chat).await.iter().any(|e| matches!(e, Entry::Notice { .. })));
    h.use_fake("card");
    h.send(chat, "third").await;
    h.wait_end(chat).await;
    let third = h.turn_argvs()[2].clone();
    assert_eq!(third.iter().position(|a| a == "--resume").map(|i| third[i + 1].clone()), Some(session), "the session was kept");
}

#[tokio::test]
async fn the_notice_appears_only_once_the_replacement_session_has_started() {
    let mut h = Harness::open().await;
    h.use_fake("card");
    let chat = h.chat(Permission::Auto);
    h.send(chat, "first").await;
    h.wait_end(chat).await;
    h.use_fake("resume-lost-twice");
    h.send(chat, "second").await;
    let (outcome, _) = h.wait_end(chat).await;
    assert!(matches!(&outcome, TurnOutcome::Failed { error: AgentError::Exited { code: Some(1), detail } } if detail.contains("fake-claude: no session store")), "{outcome:?}");
    assert_eq!(h.turn_argvs().len(), 3, "the resume, then one fresh start");
    assert!(!h.entries(chat).await.iter().any(|e| matches!(e, Entry::Notice { .. })), "no notice for a restart that never started");
    h.use_fake("card");
    h.send(chat, "third").await;
    h.wait_end(chat).await;
    assert!(!h.turn_argvs()[3].iter().any(|a| a == "--resume"), "the lost session is forgotten");
}

#[tokio::test]
async fn a_sign_in_failure_at_start_ends_the_turn_signed_out() {
    let mut h = Harness::open().await;
    h.use_fake("signed-out-start");
    let chat = h.chat(Permission::Auto);
    h.send(chat, "go").await;
    assert_eq!(h.wait_end(chat).await.0, TurnOutcome::Failed { error: AgentError::SignedOut });
}

#[tokio::test]
async fn a_missing_assistant_directory_is_reported_as_such() {
    let mut h = Harness::open().await;
    h.use_fake("card");
    h.wait_outcome(ready).await;
    let dir = h.engine.data_dir().join("assistant");
    std::fs::remove_dir_all(&dir).unwrap();
    let chat = h.chat(Permission::Auto);
    h.send(chat, "go").await;
    assert_eq!(h.wait_end(chat).await.0, TurnOutcome::Failed { error: AgentError::Exited { code: None, detail: format!("the assistant's working directory {} is missing", dir.display()) } });
}

async fn draft_options(h: &Harness) -> ChatOptions {
    h.engine.assistant_open(ChatId(uuid::Uuid::new_v4())).await.unwrap().state.options
}

#[tokio::test]
async fn a_new_chat_starts_in_ask_first_and_inherits_a_model_only_while_the_probe_lists_it() {
    let mut h = Harness::open().await;
    h.use_fake("card");
    h.wait_outcome(ready).await;
    let options = |model: &str, effort: &str, permission: Permission| ChatOptions { provider: ProviderKind::Claude, model: ModelId(model.into()), effort: Some(EffortId(effort.into())), permission };
    let chat = ChatId(uuid::Uuid::new_v4());
    h.engine.assistant_configure(chat, options("haiku", "low", Permission::Auto)).unwrap();
    h.send(chat, "store me").await;
    h.wait_end(chat).await;
    h.wait_idle(chat).await;
    assert_eq!(draft_options(&h).await, options("haiku", "low", Permission::AskFirst), "the newest chat's model and effort, never its permission");
    h.use_fake_with("card", "--model sonnet");
    h.wait_outcome(ready).await;
    assert_eq!(draft_options(&h).await, options("sonnet", "high", Permission::AskFirst), "launch arguments that name a model win");
    h.configure_provider(|p| p.binary_path = "/nope".into());
    h.wait_outcome(|o| matches!(o, ProbeOutcome::NotFound { .. })).await;
    assert_eq!(draft_options(&h).await, options("opus", "high", Permission::AskFirst), "with no Ready probe, the fallback rather than an unverified stored choice");
}

#[tokio::test]
async fn a_changed_config_dir_restarts_the_session_with_a_recap() {
    let mut h = Harness::open().await;
    h.use_fake("card");
    let chat = h.chat(Permission::Auto);
    h.send(chat, "first").await;
    assert_eq!(h.wait_end(chat).await.0, TurnOutcome::Completed);
    let other = h.dir.path().join("other-config");
    std::fs::create_dir_all(&other).unwrap();
    h.configure_provider(|p| p.config_dir = other.display().to_string());
    h.send(chat, "second").await;
    assert_eq!(h.wait_end(chat).await.0, TurnOutcome::Completed);
    let argvs = h.turn_argvs();
    assert_eq!(argvs.len(), 2, "no resume was attempted: {argvs:?}");
    assert!(!argvs[1].iter().any(|a| a == "--resume"));
    let entries = h.entries(chat).await;
    assert!(entries.iter().any(|e| matches!(e, Entry::Notice { text } if text.starts_with("Claude could not resume this chat"))), "{entries:?}");
    assert!(h.stdin_lines().iter().any(|l| l.contains("Earlier in this conversation, restored after the session was lost") && l.contains("User: first")));
    h.send(chat, "third").await;
    assert_eq!(h.wait_end(chat).await.0, TurnOutcome::Completed);
    let argvs = h.turn_argvs();
    assert_eq!(argvs[2].iter().position(|a| a == "--resume").map(|i| argvs[2][i + 1].clone()), Some(format!("fake-session-{}", h.turn_pids()[1])), "the replacement session resumes under the new config dir");
}

#[tokio::test]
async fn the_second_turn_resumes_the_session_and_a_changed_model_reaches_argv() {
    let mut h = Harness::open().await;
    h.use_fake("card");
    let chat = h.chat(Permission::Auto);
    h.send(chat, "one").await;
    assert_eq!(h.wait_end(chat).await.0, TurnOutcome::Completed);
    let first = h.turn_argvs()[0].clone();
    assert!(!first.iter().any(|a| a == "--resume"));
    assert_eq!(first.iter().position(|a| a == "--model").map(|i| first[i + 1].as_str()), Some("opus"));
    assert_eq!(first.iter().position(|a| a == "--effort").map(|i| first[i + 1].as_str()), Some("high"));
    let state = h.engine.assistant_configure(chat, ChatOptions { provider: ProviderKind::Claude, model: ModelId("sonnet".into()), effort: Some(EffortId("low".into())), permission: Permission::Auto }).unwrap();
    assert_eq!(state.options.model, ModelId("sonnet".into()));
    h.send(chat, "two").await;
    assert_eq!(h.wait_end(chat).await.0, TurnOutcome::Completed);
    let second = h.turn_argvs()[1].clone();
    assert_eq!(second.iter().position(|a| a == "--resume").map(|i| second[i + 1].clone()), Some(format!("fake-session-{}", h.turn_pids()[0])));
    assert_eq!(second.iter().position(|a| a == "--model").map(|i| second[i + 1].as_str()), Some("sonnet"));
    assert_eq!(second.iter().position(|a| a == "--effort").map(|i| second[i + 1].as_str()), Some("low"));
    let entries = h.entries(chat).await;
    assert_eq!(entries.len(), 6);
    assert!(matches!(&entries[3], Entry::User { text, .. } if text == "two"));
}

#[tokio::test]
async fn chats_persist_across_open_and_a_mid_turn_row_recovers_as_lost() {
    let dir = tempfile::tempdir().unwrap();
    let chat = ChatId(uuid::Uuid::new_v4());
    {
        let mut h = Harness::open_in(dir).await;
        h.use_fake("card");
        h.engine.assistant_configure(chat, ChatOptions { provider: ProviderKind::Claude, model: ModelId("opus".into()), effort: None, permission: Permission::Auto }).unwrap();
        h.send(chat, "persist me").await;
        assert_eq!(h.wait_end(chat).await.0, TurnOutcome::Completed);
        h.engine.shutdown().await;
        let h2 = Harness::open_in(h.dir).await;
        let listed = h2.engine.boot().chats;
        assert_eq!(listed.iter().map(|c| (c.id, c.title.as_str(), c.entry_count)).collect::<Vec<_>>(), [(chat, "persist me", 3)]);
        let entries = h2.entries(chat).await;
        assert!(matches!(&entries[0], Entry::User { text, .. } if text == "persist me"));
        assert!(matches!(&entries[1], Entry::Text { done: true, .. }));
        assert!(matches!(&entries[2], Entry::End { outcome: TurnOutcome::Completed, .. }));
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_owned();
    let lost = ChatId(uuid::Uuid::new_v4());
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let mut h = Harness::open_in(tempfile::TempDir::new_in(&path).unwrap()).await;
            h.use_fake("slow");
            h.engine.assistant_configure(lost, ChatOptions { provider: ProviderKind::Claude, model: ModelId("opus".into()), effort: None, permission: Permission::Auto }).unwrap();
            h.send(lost, "lose me").await;
            h.wait_entry(lost, "streaming text", |s| matches!(s.entry, Entry::Text { .. })).await;
            std::mem::forget(h.dir);
            std::fs::write(path.join("data-dir"), h.engine.data_dir().display().to_string()).unwrap();
        });
        rt.shutdown_background();
    })
    .join()
    .unwrap();
    let data_dir = std::fs::read_to_string(dir.path().join("data-dir")).unwrap();
    let engine = Engine::open(EngineConfig { data_dir: data_dir.into(), keys: Arc::new(MemoryKeyStore::default()), agent_path: AgentPath::Fixed("".into()), az_binary: None, user_known_hosts: None, updates: Arc::new(engine::ScriptedTransport::named("off").unwrap()) }).await.unwrap();
    let listed = engine.boot().chats;
    assert_eq!(listed.iter().map(|c| (c.id, c.status.clone())).collect::<Vec<_>>(), [(lost, ChatStatus::Idle)]);
    let view = engine.assistant_open(lost).await.unwrap();
    let entries: Vec<Entry> = view.entries.into_iter().map(|s| s.entry).collect();
    assert_eq!(entries.len(), 2, "{entries:?}");
    assert!(matches!(&entries[0], Entry::User { text, .. } if text == "lose me"));
    assert!(matches!(&entries[1], Entry::End { outcome: TurnOutcome::Lost, .. }), "{:?}", entries[1]);
    assert_eq!(view.state.entry_count, 2);
}

#[tokio::test]
async fn shutdown_leaves_no_child_pid_alive_and_ends_the_turn_stopped() {
    let mut h = Harness::open().await;
    h.use_fake("slow");
    let chat = h.chat(Permission::Auto);
    h.send(chat, "go").await;
    h.wait_entry(chat, "streaming text", |s| matches!(s.entry, Entry::Text { .. })).await;
    let pid = h.turn_pids()[0];
    let started = Instant::now();
    h.engine.shutdown().await;
    assert!(started.elapsed() < Duration::from_secs(4), "shutdown is bounded");
    wait_dead(pid).await;
    let entries = h.entries(chat).await;
    assert!(matches!(entries.last(), Some(Entry::End { outcome: TurnOutcome::Stopped, .. })), "{:?}", entries.last());
}

#[tokio::test]
async fn stop_kills_a_process_that_ignores_interrupt_and_eof_before_the_next_turn_starts() {
    let mut h = Harness::open().await;
    h.use_fake("wedged");
    let chat = h.chat(Permission::Auto);
    h.send(chat, "go").await;
    h.wait_entry(chat, "streaming text", |s| matches!(&s.entry, Entry::Text { done: false, blocks } if !blocks.is_empty())).await;
    let pid = h.turn_pids()[0];
    let started = Instant::now();
    h.engine.assistant_stop(chat);
    assert_eq!(h.wait_end(chat).await.0, TurnOutcome::Stopped);
    assert_eq!(h.send(chat, "again").await.turn, 3, "the send right after Stop is accepted");
    h.wait_entry(chat, "the second turn's text", |s| s.turn == 3 && matches!(s.entry, Entry::Text { .. })).await;
    assert!(!alive(pid), "the second process started while the first lived");
    assert_eq!(h.turn_pids().len(), 2);
    assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
    h.engine.shutdown().await;
}

#[tokio::test]
async fn shutdown_kills_a_wedged_process_within_its_bound() {
    let mut h = Harness::open().await;
    h.use_fake("wedged");
    let chat = h.chat(Permission::Auto);
    h.send(chat, "go").await;
    h.wait_entry(chat, "streaming text", |s| matches!(&s.entry, Entry::Text { done: false, blocks } if !blocks.is_empty())).await;
    let pid = h.turn_pids()[0];
    let started = Instant::now();
    h.engine.shutdown().await;
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
    dead_within(pid, Duration::from_secs(1)).await;
    let entries = h.entries(chat).await;
    assert!(matches!(entries.last(), Some(Entry::End { outcome: TurnOutcome::Stopped | TurnOutcome::Lost, .. })), "{:?}", entries.last());
    assert_eq!(h.engine.boot().chats[0].status, ChatStatus::Idle);
}

#[tokio::test]
async fn a_quick_follow_up_is_accepted_and_starts_only_after_the_last_process_exited() {
    let mut h = Harness::open().await;
    h.use_fake_with("card", "--fake-linger-ms 500");
    let chat = h.chat(Permission::Auto);
    h.send(chat, "one").await;
    h.wait_end(chat).await;
    assert_eq!(h.send(chat, "two").await.turn, 3, "the follow-up sent right after the End is accepted");
    h.wait_end(chat).await;
    let life = h.lifecycle();
    let [first, second] = h.turn_pids()[..] else { panic!("{life:?}") };
    let at = |kind: &str, pid: u32| life.iter().position(|(k, p)| k == kind && *p == pid).unwrap_or_else(|| panic!("no {kind} for {pid} in {life:?}"));
    assert!(at("exit", first) < at("argv", second), "the second process started before the first exited: {life:?}");
}

#[tokio::test]
async fn delete_waits_for_the_last_process_to_exit() {
    let mut h = Harness::open().await;
    h.use_fake_with("card", "--fake-linger-ms 500");
    let chat = h.chat(Permission::Auto);
    h.send(chat, "one").await;
    h.wait_end(chat).await;
    let pid = h.turn_pids()[0];
    h.engine.assistant_delete(chat).await;
    assert!(!alive(pid), "delete returned while the last process lived");
    assert!(h.lifecycle().contains(&("exit".to_owned(), pid)), "the process was killed instead of exiting: {:?}", h.lifecycle());
    h.wait_chat(chat, "the tombstone", |s| s.status == ChatStatus::Deleted).await;
}

#[tokio::test]
async fn shutdown_waits_for_the_last_process_to_exit_before_the_sweep() {
    let mut h = Harness::open().await;
    h.use_fake_with("card", "--fake-linger-ms 500");
    let chat = h.chat(Permission::Auto);
    h.send(chat, "one").await;
    h.wait_end(chat).await;
    let pid = h.turn_pids()[0];
    h.engine.shutdown().await;
    assert!(!alive(pid), "shutdown returned while the last process lived");
    assert!(h.lifecycle().contains(&("exit".to_owned(), pid)), "the process was killed instead of exiting: {:?}", h.lifecycle());
}

#[tokio::test]
async fn approve_works_from_a_thread_without_a_runtime() {
    let mut h = Harness::open().await;
    h.use_fake("query");
    let chat = h.chat(Permission::AskFirst);
    h.send(chat, "How many orders?").await;
    let (approval, _) = h.wait_approval(chat).await;
    let caller = h.engine.clone();
    let other = ChatId(uuid::Uuid::new_v4());
    let answered = std::thread::spawn(move || {
        caller.assistant_stop(other);
        assert_eq!(caller.assistant_approve(other, approval, Decision::Run), Err(ApproveError::NotPending), "another chat's id");
        caller.assistant_approve(chat, approval, Decision::Run)
    })
    .join()
    .expect("panicked off the runtime");
    assert_eq!(answered, Ok(()));
    assert_eq!(h.wait_end(chat).await.0, TurnOutcome::Completed);
}

#[tokio::test]
async fn an_empty_message_a_disabled_provider_and_a_gone_attachment_are_refused() {
    let h = Harness::open().await;
    h.use_fake("card");
    let chat = h.chat(Permission::Auto);
    assert_eq!(h.engine.assistant_send(chat, SendRequest { text: "  \n".into(), tab: None }).await.unwrap_err(), SendError::EmptyMessage);
    let gone = TabContext {
        tab_id: TabId(uuid::Uuid::new_v4()),
        title: "t.sql".into(),
        db: None,
        text: "select 1".into(),
        target: RunTarget::All,
        attach: Some(ResultRef { tab_id: TabId(uuid::Uuid::new_v4()), run_id: RunId(1), index: 0 }),
    };
    assert_eq!(h.engine.assistant_send(chat, SendRequest { text: "x".into(), tab: Some(gone) }).await.unwrap_err(), SendError::AttachmentGone);
    h.configure_provider(|p| p.enabled = false);
    assert_eq!(h.engine.assistant_send(chat, SendRequest { text: "x".into(), tab: None }).await.unwrap_err(), SendError::ProviderOff { provider: ProviderKind::Claude });
    assert!(h.engine.boot().chats.is_empty(), "a refused send stores nothing");
}

#[tokio::test]
async fn a_draft_opens_with_default_options_and_configure_validates_against_the_probe() {
    let mut h = Harness::open().await;
    h.use_fake("none");
    h.wait_outcome(ready).await;
    let chat = ChatId(uuid::Uuid::new_v4());
    let view = h.engine.assistant_open(chat).await.unwrap();
    assert_eq!((view.state.status, view.state.title.as_str(), view.state.entry_count, view.entries.len()), (ChatStatus::Draft, "", 0, 0));
    assert_eq!(view.state.options, ChatOptions { provider: ProviderKind::Claude, model: ModelId("opus".into()), effort: Some(EffortId("high".into())), permission: Permission::AskFirst });
    let options = |model: &str, effort: Option<&str>| ChatOptions { provider: ProviderKind::Claude, model: ModelId(model.into()), effort: effort.map(|e| EffortId(e.into())), permission: Permission::Auto };
    assert_eq!(h.engine.assistant_configure(chat, options("nope", None)).unwrap_err(), ChatError::UnknownModel { model: ModelId("nope".into()) });
    assert_eq!(h.engine.assistant_configure(chat, options("opus", Some("turbo"))).unwrap_err(), ChatError::UnsupportedEffort { effort: EffortId("turbo".into()) });
    assert_eq!(h.engine.assistant_configure(chat, options("fake", Some("low"))).unwrap_err(), ChatError::UnsupportedEffort { effort: EffortId("low".into()) });
    let state = h.engine.assistant_configure(chat, options("haiku", Some("max"))).unwrap();
    assert_eq!((state.status, state.options), (ChatStatus::Draft, options("haiku", Some("max"))));
    assert!(h.engine.boot().chats.is_empty(), "a draft is not listed");
    assert_eq!(h.engine.assistant_open(chat).await.unwrap().state.options, options("haiku", Some("max")));
}

async fn draft_with(launch_args: &str) -> (String, Option<String>) {
    let mut h = Harness::open().await;
    h.configure_provider(|p| {
        p.binary_path = FAKE.into();
        p.launch_args = format!("--fake-scenario none {launch_args}");
    });
    h.wait_outcome(ready).await;
    let options = h.engine.assistant_open(ChatId(uuid::Uuid::new_v4())).await.unwrap().state.options;
    (options.model.0, options.effort.map(|e| e.0))
}

#[tokio::test]
async fn launch_arguments_choose_a_new_chats_model_and_effort() {
    assert_eq!(draft_with("--model claude-sonnet-5-5 --effort low").await, ("sonnet".into(), Some("low".into())));
    assert_eq!(draft_with("--model haiku --effort turbo").await, ("haiku".into(), Some("high".into())), "an effort the model does not list falls back to high");
    assert_eq!(draft_with("--model fake --effort low").await, ("fake".into(), None), "a model without effort levels starts with none");
}

#[tokio::test]
async fn delete_during_a_wedged_turn_leaves_no_row_and_nothing_after_the_tombstone() {
    let mut h = Harness::open().await;
    h.use_fake("wedged");
    let chat = h.chat(Permission::Auto);
    h.send(chat, "go").await;
    h.wait_entry(chat, "streaming text", |s| matches!(&s.entry, Entry::Text { done: false, blocks } if !blocks.is_empty())).await;
    let pid = h.turn_pids()[0];
    h.engine.assistant_delete(chat).await;
    let tombstone = h.wait_chat(chat, "the tombstone", |s| s.status == ChatStatus::Deleted).await;
    assert!(!alive(pid));
    assert_eq!(h.events_above(chat, tombstone.rev, Duration::from_millis(500)).await.len(), 0, "the dying turn published after the tombstone");
    assert!(h.engine.boot().chats.is_empty(), "a row came back");
    let reopened = h.engine.assistant_open(chat).await.unwrap();
    assert_eq!((reopened.state.status, reopened.entries.len()), (ChatStatus::Draft, 0));
}

#[tokio::test]
async fn deleting_a_stored_chat_this_session_never_loaded_publishes_a_tombstone() {
    let mut h = Harness::open().await;
    h.use_fake("card");
    let chat = h.chat(Permission::Auto);
    h.send(chat, "keep me").await;
    h.wait_end(chat).await;
    h.wait_idle(chat).await;
    h.engine.shutdown().await;
    let mut h2 = Harness::open_in(h.dir).await;
    assert_eq!(h2.engine.boot().chats.iter().map(|c| (c.id, c.rev)).collect::<Vec<_>>(), [(chat, 0)], "stored, not loaded");
    h2.engine.assistant_delete(chat).await;
    let tombstone = h2.wait_chat(chat, "the tombstone", |s| s.status == ChatStatus::Deleted).await;
    assert!(tombstone.rev > 0 && tombstone.title == "keep me", "{tombstone:?}");
    assert!(h2.engine.boot().chats.is_empty());
}

#[tokio::test]
async fn opening_a_chat_twice_returns_the_same_rev_and_no_newer_entries() {
    let mut h = Harness::open().await;
    h.use_fake("card");
    let chat = h.chat(Permission::Auto);
    h.send(chat, "one").await;
    h.wait_end(chat).await;
    let idle = h.wait_idle(chat).await;
    let first = h.engine.assistant_open(chat).await.unwrap();
    let second = h.engine.assistant_open(chat).await.unwrap();
    assert_eq!((first.state.rev, second.state.rev), (idle.rev, idle.rev), "open mints no rev");
    let revs = |view: &ChatView| view.entries.iter().map(|e| e.rev).collect::<Vec<_>>();
    assert_eq!(revs(&first), revs(&second));
    assert!(revs(&first).iter().all(|r| *r <= idle.rev), "{:?} above the idle rev {}", revs(&first), idle.rev);
}

#[tokio::test]
async fn delete_stops_the_turn_and_publishes_a_tombstone() {
    let mut h = Harness::open().await;
    h.use_fake("slow");
    let chat = h.chat(Permission::Auto);
    h.send(chat, "go").await;
    h.wait_entry(chat, "streaming text", |s| matches!(s.entry, Entry::Text { .. })).await;
    let pid = h.turn_pids()[0];
    h.engine.assistant_delete(chat).await;
    h.wait_chat(chat, "the tombstone", |s| s.status == ChatStatus::Deleted).await;
    wait_dead(pid).await;
    assert!(h.engine.boot().chats.is_empty());
    assert_eq!(h.engine.assistant_open(chat).await.unwrap().state.status, ChatStatus::Draft, "the id opens a fresh draft");
    h.engine.assistant_delete(chat).await;
}

#[tokio::test]
async fn a_chat_being_deleted_stays_deleted_when_its_turn_ends_and_refuses_sends() {
    let mut h = Harness::open().await;
    h.use_fake_with("slow", "--fake-linger-ms 500");
    let chat = h.chat(Permission::Auto);
    h.send(chat, "go").await;
    h.wait_entry(chat, "streaming text", |s| matches!(s.entry, Entry::Text { .. })).await;
    let sends = async {
        loop {
            match h.engine.assistant_send(chat, SendRequest { text: "again".into(), tab: None }).await {
                Err(SendError::Busy) => tokio::time::sleep(Duration::from_millis(2)).await,
                other => return other,
            }
        }
    };
    tokio::select! {
        () = h.engine.assistant_delete(chat) => {}
        other = sends => panic!("a send to the chat being deleted returned {other:?}"),
    }
    let next = h.wait_chat(chat, "an idle or deleted state", |s| matches!(s.status, ChatStatus::Idle | ChatStatus::Deleted)).await;
    assert_eq!(next.status, ChatStatus::Deleted, "the stopped turn published Idle after the delete began");
    assert!(h.engine.boot().chats.is_empty(), "a row came back");
}

#[cfg(feature = "pg-tests")]
mod pg {
    use super::*;
    use engine::draft_from_url;

    const DEFAULT_URL: &str = "postgres://l1data:l1data@localhost:5433/shop";

    async fn connected(h: &Harness) -> ConnectionConfig {
        let url = std::env::var("L1_DATA_TEST_PG").unwrap_or_else(|_| DEFAULT_URL.into());
        let config = h.engine.save_connection(draft_from_url(&url, "dev").unwrap()).await.unwrap();
        h.engine.connect(config.id).await.unwrap();
        config
    }

    fn tab(config: &ConnectionConfig, text: &str) -> TabContext {
        TabContext { tab_id: TabId(uuid::Uuid::new_v4()), title: "orders.sql".into(), db: Some(DatabaseRef { connection_id: config.id, database: config.database.clone() }), text: text.into(), target: RunTarget::All, attach: None }
    }

    async fn send_on(h: &Harness, chat: ChatId, text: &str, tab: TabContext) {
        h.engine.assistant_send(chat, SendRequest { text: text.into(), tab: Some(tab) }).await.unwrap();
    }

    async fn scalar(h: &Harness, config: &ConnectionConfig, sql: &str) -> String {
        let tab_id = TabId(uuid::Uuid::new_v4());
        h.engine.console_configure(tab_id, ConsoleConfig { connection_id: config.id, database: config.database.clone(), schema: None, mode: TxnMode::AutoCommit }).await.unwrap();
        let accepted = h.engine.console_run(tab_id, RunRequest::Script { text: sql.into(), target: RunTarget::All, mode: RunMode::Run, params: Vec::new(), cells: Vec::new() }, None).await.unwrap();
        let started = Instant::now();
        loop {
            let state = h.engine.boot().consoles.into_iter().find(|c| c.tab_id == tab_id).unwrap();
            if let RunState::Finished { run_id, .. } = state.run
                && run_id == accepted.run_id
            {
                break;
            }
            assert!(started.elapsed() < Duration::from_secs(10));
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let page = h.engine.result_page(ResultRef { tab_id, run_id: accepted.run_id, index: 0 }, 0, 1).unwrap();
        h.engine.console_close(tab_id, true).await.unwrap();
        page.rows[0][0].clone().unwrap()
    }

    #[tokio::test]
    async fn a_manual_tab_with_an_uncommitted_insert_is_untouched_and_the_approved_count_does_not_see_it() {
        let mut h = Harness::open().await;
        h.use_fake("query");
        let config = connected(&h).await;
        let before = scalar(&h, &config, "select count(*) from public.orders").await;
        let manual = TabId(uuid::Uuid::new_v4());
        h.engine.console_configure(manual, ConsoleConfig { connection_id: config.id, database: config.database.clone(), schema: None, mode: TxnMode::Manual }).await.unwrap();
        h.engine.console_run(manual, RunRequest::Script { text: "insert into public.orders (number, customer_id, total) values ('ASSIST-TXN', 1, 1)".into(), target: RunTarget::All, mode: RunMode::Run, params: Vec::new(), cells: Vec::new() }, None).await.unwrap();
        let started = Instant::now();
        loop {
            let state = h.engine.boot().consoles.into_iter().find(|c| c.tab_id == manual).unwrap();
            if matches!(state.run, RunState::Finished { .. }) {
                assert_eq!(state.txn, TxnState::Active, "{state:?}");
                break;
            }
            assert!(started.elapsed() < Duration::from_secs(10));
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let chat = h.chat(Permission::AskFirst);
        send_on(&h, chat, "How many orders?", tab(&config, "select 1")).await;
        let (approval, _) = h.wait_approval(chat).await;
        h.engine.assistant_approve(chat, approval, Decision::Run).unwrap();
        assert_eq!(h.wait_end(chat).await.0, TurnOutcome::Completed);
        let entries = h.entries(chat).await;
        let Entry::Tool { status: ToolStatus::Done { result: ToolResult::Rows { columns, rows, more, .. } }, .. } = &entries[1] else { panic!("{:?}", entries[1]) };
        assert_eq!((columns.clone(), rows.clone(), *more), (vec!["count".to_owned()], vec![vec![Some(before.clone())]], false));
        assert!(text_of(&entries[2]).contains(&format!("count\n{before}\n1 rows")));
        let tab_state = h.engine.boot().consoles.into_iter().find(|c| c.tab_id == manual).unwrap();
        assert_eq!(tab_state.txn, TxnState::Active, "the tab's transaction is untouched");
        h.engine.console_rollback(manual).await.unwrap();
        assert_eq!(scalar(&h, &config, "select count(*) from public.orders").await, before);
    }

    #[tokio::test]
    async fn auto_asks_before_an_escaped_call_and_a_deny_runs_nothing() {
        let mut h = Harness::open().await;
        h.use_fake_with("query", r#"--fake-sql 'select U&"pg\005fcancel_backend"(0)'"#);
        let config = connected(&h).await;
        let chat = h.chat(Permission::Auto);
        send_on(&h, chat, "Cancel it", tab(&config, "")).await;
        let (approval, reason) = h.wait_approval(chat).await;
        assert_eq!(reason, ApprovalReason::Unvetted { functions: vec!["U& escape".into(), r"pg\005fcancel_backend".into()] });
        h.engine.assistant_approve(chat, approval, Decision::Deny).unwrap();
        assert_eq!(h.wait_end(chat).await.0, TurnOutcome::Completed);
        let entries = h.entries(chat).await;
        assert!(matches!(&entries[1], Entry::Tool { tool: ToolUse::RunQuery { sql, .. }, status: ToolStatus::Denied { stopped: false }, .. } if sql == r#"select U&"pg\005fcancel_backend"(0)"#), "{:?}", entries[1]);
        assert!(!h.stdin_lines().iter().any(|l| l.contains("tools/call")), "the call never ran");
    }

    #[tokio::test]
    async fn a_wide_row_stays_within_the_tool_budget_in_the_stored_result_and_the_model_text() {
        let mut h = Harness::open().await;
        let columns = (1..=60).map(|i| format!("repeat(chr(120), 300) as c{i}")).collect::<Vec<_>>().join(", ");
        h.use_fake_with("query", &format!("--fake-sql 'select {columns}'"));
        let config = connected(&h).await;
        let chat = h.chat(Permission::Auto);
        send_on(&h, chat, "Show me a wide row", tab(&config, "")).await;
        assert_eq!(h.wait_end(chat).await.0, TurnOutcome::Completed);
        let entries = h.entries(chat).await;
        let Entry::Tool { status: ToolStatus::Done { result: ToolResult::Rows { columns, rows, more, .. } }, .. } = &entries[1] else { panic!("{:?}", entries[1]) };
        assert_eq!((columns.len(), rows.len(), *more), (60, 1, false));
        let stored: usize = rows.iter().flatten().map(|c| c.as_ref().map_or(4, String::len) + 1).sum();
        assert!(stored <= 8 << 10, "the stored row holds {stored} bytes");
        let sent = h.stdin_lines().into_iter().find(|l| l.contains("\"mcp_response\"") && l.contains("\"content\"")).unwrap();
        let text = serde_json::from_str::<serde_json::Value>(&sent).unwrap()["response"]["response"]["mcp_response"]["result"]["content"][0]["text"].as_str().unwrap().len();
        assert!(text <= 8 << 10, "the model got {text} bytes");
    }

    #[tokio::test]
    async fn describe_tables_lists_the_foreign_key_and_an_index() {
        let mut h = Harness::open().await;
        h.use_fake("describe");
        let config = connected(&h).await;
        let chat = h.chat(Permission::AskFirst);
        send_on(&h, chat, "Describe orders", tab(&config, "")).await;
        assert_eq!(h.wait_end(chat).await.0, TurnOutcome::Completed);
        let entries = h.entries(chat).await;
        assert!(matches!(&entries[1], Entry::Tool { status: ToolStatus::Done { result: ToolResult::Described { found, missing } }, .. } if found == &["public.orders".to_owned()] && missing.is_empty()), "{:?}", entries[1]);
        let text = text_of(&entries[2]);
        assert!(text.contains("public.orders (table"), "{text}");
        assert!(text.contains("customer_id bigint NOT NULL FK → public.customers(id)"), "{text}");
        assert!(text.contains("orders_created_at_idx (created_at DESC)"), "{text}");
        assert!(text.contains("orders_open_idx (status) WHERE ") && text.contains("status <> 'paid'::text"), "{text}");
        assert!(!h.stdin_lines().iter().any(|l| l.contains("AwaitingApproval")) && !entries.iter().any(|e| matches!(e, Entry::Tool { status: ToolStatus::AwaitingApproval { .. }, .. })), "a catalog tool never asks");
    }

    #[tokio::test]
    async fn default_schema_tools_return_structure_without_estimates_defaults_or_index_expressions() {
        for scenario in ["describe", "search", "echo"] {
            let mut h = Harness::open_in(tempfile::tempdir().unwrap()).await;
            h.use_fake(scenario);
            let config = connected(&h).await;
            let chat = h.chat(Permission::AskFirst);
            send_on(&h, chat, "Help with the orders schema", tab(&config, "select 'EDITOR_CANARY'")).await;
            assert_eq!(h.wait_end(chat).await.0, TurnOutcome::Completed);
            let entries = h.entries(chat).await;
            let sent = h.stdin_lines().join("\n");
            assert!(!sent.contains("EDITOR_CANARY"));
            assert!(!sent.contains("~") && !sent.contains("WHERE") && !sent.contains("'paid'"), "{sent}");
            if scenario == "describe" {
                let text = text_of(&entries[2]);
                assert!(text.contains("customer_id bigint NOT NULL FK → public.customers(id)"), "{text}");
                assert!(text.contains("orders_open_idx"));
                assert!(!text.contains("(status)") && !text.contains("created_at DESC"));
            } else if scenario == "search" {
                assert!(text_of(&entries[2]).contains("table public.orders"));
            } else {
                assert!(text_of(&entries[1]).contains("public.orders: id bigint PK"));
            }
            assert!(!entries.iter().any(|e| matches!(e, Entry::Tool { status: ToolStatus::AwaitingApproval { .. }, .. })));
        }
    }

    #[tokio::test]
    async fn downgrade_cancels_a_running_query_without_delivering_its_rows() {
        let mut h = Harness::open().await;
        h.use_fake_with("query", "--fake-sql \"select 'ROW_CANARY', pg_sleep(10)\"");
        let config = connected(&h).await;
        let chat = h.chat(Permission::AskFirst);
        send_on(&h, chat, "Run the slow query", tab(&config, "")).await;
        let (approval, _) = h.wait_approval(chat).await;
        h.engine.assistant_approve(chat, approval, Decision::Run).unwrap();
        let deadline = Instant::now() + WAIT;
        loop {
            let running = scalar(&h, &config, "select count(*) from pg_stat_activity where query like 'select ''ROW_CANARY''%' and state = 'active'").await;
            if running != "0" { break; }
            assert!(Instant::now() < deadline, "the assistant query never ran");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        h.access(AssistantAccess::SchemaOnly);
        h.access(AssistantAccess::Full);
        assert_eq!(h.wait_end(chat).await.0, TurnOutcome::Stopped);
        assert!(!h.stdin_lines().iter().any(|l| l.contains("mcp_response") && l.contains("ROW_CANARY")));
        assert!(!h.entries(chat).await.iter().any(|e| matches!(e, Entry::Tool { status: ToolStatus::Done { result: ToolResult::Rows { .. } }, .. })));
    }

    #[tokio::test]
    async fn full_access_still_attaches_rows_and_schema_only_drops_the_same_reference() {
        let mut h = Harness::open().await;
        h.use_fake("echo");
        let config = connected(&h).await;
        let mut context = tab(&config, "select 'ATTACHMENT_CANARY'");
        let id = context.tab_id;
        h.engine.console_configure(id, ConsoleConfig { connection_id: config.id, database: config.database.clone(), schema: None, mode: TxnMode::AutoCommit }).await.unwrap();
        let accepted = h.engine.console_run(id, RunRequest::Script { text: context.text.clone(), target: RunTarget::All, mode: RunMode::Run, params: Vec::new(), cells: Vec::new() }, None).await.unwrap();
        let deadline = Instant::now() + WAIT;
        loop {
            if h.engine.boot().consoles.iter().any(|c| c.tab_id == id && matches!(c.run, RunState::Finished { .. })) { break; }
            assert!(Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        context.attach = Some(ResultRef { tab_id: id, run_id: accepted.run_id, index: 0 });
        let chat = h.chat(Permission::Auto);
        send_on(&h, chat, "Full attachment", context.clone()).await;
        h.wait_end(chat).await;
        h.wait_idle(chat).await;
        assert!(matches!(&h.entries(chat).await[0], Entry::User { attachment: Some(a), .. } if a.rows[0][0].as_deref() == Some("ATTACHMENT_CANARY")));
        assert!(h.stdin_lines().iter().any(|l| l.contains("Attached result") && l.contains("ATTACHMENT_CANARY")));
        h.access(AssistantAccess::SchemaOnly);
        let before = h.stdin_lines().len();
        send_on(&h, chat, "Schema attachment is discarded", context).await;
        h.wait_end(chat).await;
        assert!(matches!(&h.entries(chat).await[3], Entry::User { attachment: None, .. }));
        assert!(!h.stdin_lines().into_iter().skip(before).any(|l| l.contains("ATTACHMENT_CANARY")));
    }

    #[tokio::test]
    async fn the_context_sends_the_outline_once_per_session_and_the_tab_sql_when_it_changes() {
        let mut h = Harness::open().await;
        h.use_fake("echo");
        let config = connected(&h).await;
        let chat = h.chat(Permission::Auto);
        send_on(&h, chat, "first", tab(&config, "select * from orders")).await;
        assert_eq!(h.wait_end(chat).await.0, TurnOutcome::Completed);
        let first = text_of(&h.entries(chat).await[1]);
        assert!(first.contains("Connection: dev (PostgreSQL ") && first.contains(", database shop, connected."), "{first}");
        assert!(first.contains("Default schema: public. Shown schemas: lab, public.") && first.contains("Schema outline:\nlab.type_zoo, ~") && first.contains("public.orders, ~"), "{first}");
        assert!(first.contains("Tab SQL:\nselect * from orders\nStatement at the cursor:\nselect * from orders\n"), "{first}");
        send_on(&h, chat, "second", tab(&config, "select * from orders")).await;
        assert_eq!(h.wait_end(chat).await.0, TurnOutcome::Completed);
        let second = text_of(&h.entries(chat).await[4]);
        assert!(second.contains("Tab SQL: unchanged since your last message.") && second.contains("Schema outline: unchanged since earlier in this chat."), "{second}");
        send_on(&h, chat, "third", tab(&config, "select * from customers")).await;
        assert_eq!(h.wait_end(chat).await.0, TurnOutcome::Completed);
        let third = text_of(&h.entries(chat).await[7]);
        assert!(third.contains("Tab SQL:\nselect * from customers\n") && third.contains("Schema outline: unchanged since earlier in this chat."), "{third}");
        let user = &h.entries(chat).await[0];
        assert!(matches!(user, Entry::User { context: Some(SentContext { connection_name: Some(name), tab_title, .. }), .. } if name == "dev" && tab_title == "orders.sql"), "{user:?}");
    }
}

#[tokio::test]
async fn sql_server_model_queries_are_refused_without_an_approval_even_without_a_catalog() {
    for permission in [Permission::AskFirst, Permission::Auto] {
        let mut h = Harness::open().await;
        h.use_fake_with("query", "--fake-sql 'SELECT TOP (1) [id] FROM [dbo].[orders]'");
        let config = h.engine.save_connection(engine::draft_from_url("mssql://sa@127.0.0.1:1/shop?sslmode=require", "offline SQL Server").unwrap()).await.unwrap();
        let chat = h.chat(permission);
        let tab = TabContext { tab_id: TabId(uuid::Uuid::new_v4()), title: "orders.sql".into(), db: Some(DatabaseRef { connection_id: config.id, database: "shop".into() }), text: "DECLARE @x int=1; SELECT @x".into(), target: RunTarget::All, attach: None };
        h.engine.assistant_send(chat, SendRequest { text: "Count orders".into(), tab: Some(tab) }).await.unwrap();
        assert_eq!(h.wait_end(chat).await.0, TurnOutcome::Completed);
        let entries = h.entries(chat).await;
        assert!(entries.iter().any(|e| matches!(e, Entry::User { context: Some(SentContext { database_kind: Some(DatabaseKind::SqlServer), .. }), .. })));
        assert!(entries.iter().any(|e| matches!(e, Entry::Tool { tool: ToolUse::RunQuery { .. }, status: ToolStatus::Failed { message }, .. } if message.contains("SQL Server model queries are disabled"))), "{entries:?}");
        assert!(!h.stdin_lines().iter().any(|l| l.contains("\"behavior\":\"allow\"")));
        assert!(h.stdin_lines().iter().any(|l| l.contains("SQL Server / T-SQL")));
        let protocol = h.stdin_lines().join("\n");
        assert!(protocol.contains("Microsoft SQL Server") && protocol.contains("TOP or OFFSET/FETCH"), "{protocol}");
        assert!(!protocol.contains("Prefer aggregates and LIMIT"));
    }
}

#[tokio::test]
async fn schema_only_is_default_and_drops_stale_attachments_before_loading_or_storing_rows() {
    let mut h = Harness::open_in(tempfile::tempdir().unwrap()).await;
    assert_eq!(h.engine.settings().assistant_access, AssistantAccess::SchemaOnly);
    h.use_fake("echo");
    let chat = h.chat(Permission::Auto);
    let tab = TabContext { tab_id: TabId(uuid::Uuid::new_v4()), title: "TITLE_CANARY".into(), db: None, text: "select 'EDITOR_CANARY'".into(), target: RunTarget::All, attach: Some(ResultRef { tab_id: TabId(uuid::Uuid::new_v4()), run_id: RunId(999), index: 0 }) };
    h.engine.assistant_send(chat, SendRequest { text: "USER_TEXT_ALLOWED".into(), tab: Some(tab) }).await.unwrap();
    assert_eq!(h.wait_end(chat).await.0, TurnOutcome::Completed);
    h.wait_idle(chat).await;
    let entries = h.entries(chat).await;
    assert!(matches!(&entries[0], Entry::User { attachment: None, .. }));
    assert!(!h.stdin_lines().join("\n").contains("CANARY"));
    assert!(text_of(&entries[1]).contains("USER_TEXT_ALLOWED"));
    let tools = h.stdin_lines().into_iter().find(|l| l.contains("mcp_response") && l.contains("describe_tables")).unwrap();
    assert!(!tools.contains("run_query"), "{tools}");
    h.engine.shutdown().await;
    let reopened = Harness::open_in(h.dir).await;
    assert_eq!(reopened.engine.settings().assistant_access, AssistantAccess::SchemaOnly);
}

#[tokio::test]
async fn schema_only_refuses_both_permission_requests_and_forged_direct_queries() {
    for scenario in ["query", "query-forged"] {
        let mut h = Harness::open_in(tempfile::tempdir().unwrap()).await;
        h.use_fake(scenario);
        let chat = h.chat(Permission::Auto);
        h.send(chat, "Help me write SQL").await;
        assert_eq!(h.wait_end(chat).await.0, TurnOutcome::Completed);
        let entries = h.entries(chat).await;
        assert!(entries.iter().any(|e| matches!(e, Entry::Tool { status: ToolStatus::Failed { message }, .. } if message.contains("Schema only"))), "{entries:?}");
        assert!(!entries.iter().any(|e| matches!(e, Entry::Tool { status: ToolStatus::AwaitingApproval { .. } | ToolStatus::Done { result: ToolResult::Rows { .. } }, .. })));
    }
}

#[tokio::test]
async fn an_access_switch_cancels_pending_and_already_approved_calls_even_if_full_is_restored() {
    for approved in [false, true] {
        let mut h = Harness::open().await;
        h.use_fake(if approved { "query-delayed" } else { "query" });
        let chat = h.chat(Permission::AskFirst);
        h.send(chat, "Count rows").await;
        let (approval, _) = h.wait_approval(chat).await;
        if approved {
            h.engine.assistant_approve(chat, approval, Decision::Run).unwrap();
            let deadline = Instant::now() + WAIT;
            while !h.log().contains("approved\t") {
                assert!(Instant::now() < deadline);
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
        let before = h.stdin_lines().len();
        let engine = h.engine.clone();
        std::thread::spawn(move || {
            engine.set_settings(Settings { assistant_access: AssistantAccess::SchemaOnly, ..engine.settings() });
            engine.set_settings(Settings { assistant_access: AssistantAccess::Full, ..engine.settings() });
        }).join().expect("settings_set works outside a runtime");
        assert_eq!(h.engine.assistant_approve(chat, approval, Decision::Run), Err(ApproveError::NotPending));
        assert_eq!(h.wait_end(chat).await.0, TurnOutcome::Stopped);
        h.wait_idle(chat).await;
        let later = h.stdin_lines().into_iter().skip(before).collect::<Vec<_>>();
        assert!(!later.iter().any(|l| l.contains("\"behavior\"") || l.contains("\"isError\"")), "a pending permission or tool result escaped: {later:?}");
        h.use_fake("echo");
        h.send(chat, "Fresh context").await;
        assert_eq!(h.wait_end(chat).await.0, TurnOutcome::Completed);
        assert!(!h.turn_argvs().last().unwrap().iter().any(|s| s == "--resume"));
        assert!(!h.stdin_lines().last().unwrap().contains("Count rows"));
    }
}

#[tokio::test]
async fn same_access_resumes_across_restart_but_legacy_provenance_and_old_recaps_fail_closed() {
    for legacy in [false, true] {
        let mut h = Harness::open_in(tempfile::tempdir().unwrap()).await;
        if legacy { h.access(AssistantAccess::Full); }
        h.use_fake("echo");
        let chat = h.chat(Permission::Auto);
        h.send(chat, "OLD_CONTEXT_CANARY").await;
        h.wait_end(chat).await;
        h.wait_idle(chat).await;
        h.engine.shutdown().await;
        if legacy {
            let conn = rusqlite::Connection::open(h.engine.data_dir().join("l1-data.sqlite3")).unwrap();
            let json: String = conn.query_row("SELECT json FROM chats WHERE id = ?1", [chat.0.to_string()], |r| r.get(0)).unwrap();
            let mut record: serde_json::Value = serde_json::from_str(&json).unwrap();
            record.as_object_mut().unwrap().remove("access");
            record.as_object_mut().unwrap().remove("recapFloor");
            record["session"].as_object_mut().unwrap().remove("access");
            conn.execute("UPDATE chats SET json = ?1 WHERE id = ?2", [record.to_string(), chat.0.to_string()]).unwrap();
            let settings: String = conn.query_row("SELECT json FROM kv WHERE key = 'settings'", [], |r| r.get(0)).unwrap();
            let mut settings: serde_json::Value = serde_json::from_str(&settings).unwrap();
            settings.as_object_mut().unwrap().remove("assistantAccess");
            conn.execute("UPDATE kv SET json = ?1 WHERE key = 'settings'", [settings.to_string()]).unwrap();
            conn.execute("DELETE FROM kv WHERE key = 'assistant_access'", []).unwrap();
        }
        let mut h = Harness::open_in(h.dir).await;
        assert_eq!(h.engine.settings().assistant_access, AssistantAccess::SchemaOnly);
        h.send(chat, "NEW_CONTEXT").await;
        h.wait_end(chat).await;
        h.wait_idle(chat).await;
        assert_eq!(h.turn_argvs().last().unwrap().iter().any(|s| s == "--resume"), !legacy);
        if legacy {
            assert!(!h.stdin_lines().last().unwrap().contains("OLD_CONTEXT_CANARY"));
        }
        h.use_fake("resume-lost");
        h.send(chat, "Recap safe context").await;
        assert_eq!(h.wait_end(chat).await.0, TurnOutcome::Completed);
        let prompt = h.stdin_lines().into_iter().filter(|l| l.contains("Recap safe context")).last().unwrap();
        assert_eq!(prompt.contains("OLD_CONTEXT_CANARY"), !legacy, "{prompt}");
        assert!(prompt.contains("NEW_CONTEXT"));
        assert!(h.entries(chat).await.iter().any(|e| matches!(e, Entry::User { text, .. } if text == "OLD_CONTEXT_CANARY")), "history stays local");
    }
}

#[tokio::test]
async fn switching_access_while_a_chat_is_unloaded_excludes_its_old_context_and_recap() {
    let mut h = Harness::open().await;
    h.use_fake("echo");
    let chat = h.chat(Permission::Auto);
    h.send(chat, "FULL_HISTORY_CANARY").await;
    h.wait_end(chat).await;
    h.wait_idle(chat).await;
    h.engine.shutdown().await;
    let mut h = Harness::open_in(h.dir).await;
    h.access(AssistantAccess::SchemaOnly);
    h.access(AssistantAccess::Full);
    h.send(chat, "SAFE_NEW_HISTORY").await;
    h.wait_end(chat).await;
    h.wait_idle(chat).await;
    assert!(!h.turn_argvs().last().unwrap().iter().any(|s| s == "--resume"));
    h.use_fake("resume-lost");
    h.send(chat, "Summarize").await;
    h.wait_end(chat).await;
    let prompt = h.stdin_lines().into_iter().filter(|l| l.contains("Summarize")).last().unwrap();
    assert!(!prompt.contains("FULL_HISTORY_CANARY"), "{prompt}");
    assert!(prompt.contains("SAFE_NEW_HISTORY"));
}
