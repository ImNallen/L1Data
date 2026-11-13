#![cfg(feature = "claude-smoke")]

use std::sync::Arc;
use std::time::{Duration, Instant};

use engine::model::*;
use engine::{AgentPath, Engine, EngineConfig, MemoryKeyStore, draft_from_url};
use tokio::sync::broadcast;

const DEFAULT_URL: &str = "postgres://l1data:l1data@localhost:5433/shop";
const WAIT: Duration = Duration::from_secs(180);
const APPROVAL_DELAY: Duration = Duration::from_secs(75);

struct Smoke {
    engine: Arc<Engine>,
    events: broadcast::Receiver<AppEvent>,
}

impl Smoke {
    async fn wait_entry(&mut self, chat: ChatId, what: &str, pick: impl Fn(&Entry) -> bool) -> Entry {
        let started = Instant::now();
        loop {
            let remaining =
                WAIT.checked_sub(started.elapsed()).unwrap_or_else(|| panic!("timed out waiting for {what}"));
            match tokio::time::timeout(remaining, self.events.recv()).await {
                Ok(Ok(AppEvent::ChatEntryChanged(s))) if s.chat_id == chat && pick(&s.entry) => return s.entry,
                Ok(Ok(_)) | Ok(Err(broadcast::error::RecvError::Lagged(_))) => {}
                Ok(Err(e)) => panic!("{e}"),
                Err(_) => panic!("timed out waiting for {what}"),
            }
        }
    }

    async fn end(&mut self, chat: ChatId) -> TurnOutcome {
        match self.wait_entry(chat, "the End entry", |e| matches!(e, Entry::End { .. })).await {
            Entry::End { outcome, .. } => outcome,
            _ => unreachable!(),
        }
    }

    async fn reply_text(&self, chat: ChatId, turn: u32) -> String {
        let view = self.engine.assistant_open(chat).await.unwrap();
        view.entries
            .iter()
            .filter(|s| s.turn == turn)
            .filter_map(|s| match &s.entry {
                Entry::Text { blocks, .. } => Some(
                    blocks
                        .iter()
                        .map(|b| match b {
                            ReplyBlock::Markdown { text } => text.clone(),
                            ReplyBlock::Sql { sql, .. } => sql.clone(),
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// The CLI keeps a transcript directory named after the cwd, so a temp data dir leaves one behind.
struct TranscriptDirs(String);

impl Drop for TranscriptDirs {
    fn drop(&mut self) {
        remove_transcript_dirs(&self.0);
    }
}

fn remove_transcript_dirs(marker: &str) {
    let Some(home) = std::env::var_os("HOME") else { return };
    let projects = std::path::PathBuf::from(home).join(".claude/projects");
    let Ok(entries) = std::fs::read_dir(&projects) else { return };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().contains(marker) {
            println!("removing {}", entry.path().display());
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

#[tokio::test]
async fn haiku_answers_resumes_and_waits_for_an_approval() {
    let dir = tempfile::Builder::new().prefix("l1-smoke-").tempdir().unwrap();
    let marker = dir.path().file_name().unwrap().to_string_lossy().into_owned();
    let _transcripts = TranscriptDirs(marker);
    let engine = Engine::open(EngineConfig {
        data_dir: dir.path().join("data"),
        keys: Arc::new(MemoryKeyStore::default()),
        agent_path: AgentPath::LoginShell,
        az_binary: None,
        user_known_hosts: None,
        updates: Arc::new(engine::ScriptedTransport::named("off").unwrap()),
    })
    .await
    .unwrap();
    let mut smoke = Smoke { events: engine.subscribe(), engine };

    let started = Instant::now();
    let provider = loop {
        let state = smoke.engine.boot().providers.into_iter().next().unwrap();
        if !state.checking && state.outcome.is_some() {
            break state;
        }
        assert!(started.elapsed() < WAIT, "no probe outcome");
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let outcome = provider.outcome.unwrap();
    match &outcome {
        ProbeOutcome::Ready { version, plan, models } => println!(
            "probe: Ready version={version} plan={plan:?} models={} recommended={:?} ({:.1} s)",
            models.len(),
            models.iter().find(|m| m.recommended).map(|m| m.id.0.as_str()),
            started.elapsed().as_secs_f32()
        ),
        other => panic!("probe: {other:?}"),
    }

    let chat = ChatId(uuid::Uuid::new_v4());
    let options = |permission| ChatOptions {
        provider: ProviderKind::Claude,
        model: ModelId("haiku".into()),
        effort: Some(EffortId("low".into())),
        permission,
    };
    smoke.engine.assistant_configure(chat, options(Permission::Auto)).unwrap();

    let t = Instant::now();
    let first = smoke
        .engine
        .assistant_send(
            chat,
            SendRequest { text: "What is 17 times 23? Reply with only the number.".into(), tab: None },
        )
        .await
        .unwrap();
    let outcome = smoke.end(chat).await;
    let text = smoke.reply_text(chat, first.turn).await;
    println!("turn 1: {outcome:?} in {:.1} s, reply={text:?}", t.elapsed().as_secs_f32());
    assert_eq!(outcome, TurnOutcome::Completed);
    assert!(text.contains("391"), "{text}");

    let t = Instant::now();
    let second = smoke
        .engine
        .assistant_send(
            chat,
            SendRequest { text: "What number did you just tell me? Reply with only the number.".into(), tab: None },
        )
        .await
        .unwrap();
    let outcome = smoke.end(chat).await;
    let text = smoke.reply_text(chat, second.turn).await;
    let view = smoke.engine.assistant_open(chat).await.unwrap();
    let notices = view.entries.iter().filter(|s| matches!(s.entry, Entry::Notice { .. })).count();
    println!("turn 2 (resumed): {outcome:?} in {:.1} s, reply={text:?}, notices={notices}", t.elapsed().as_secs_f32());
    assert_eq!(outcome, TurnOutcome::Completed);
    assert!(text.contains("391"), "resume lost the earlier turn: {text}");
    assert_eq!(notices, 0, "the session resumed without a recap");

    let url = std::env::var("L1_DATA_TEST_PG").unwrap_or_else(|_| DEFAULT_URL.into());
    let config = smoke.engine.save_connection(draft_from_url(&url, "dev").unwrap()).await.unwrap();
    smoke.engine.connect(config.id).await.unwrap();
    smoke.engine.assistant_configure(chat, options(Permission::AskFirst)).unwrap();
    let tab = TabContext {
        tab_id: TabId(uuid::Uuid::new_v4()),
        title: "orders.sql".into(),
        db: Some(DatabaseRef { connection_id: config.id, database: config.database.clone() }),
        text: "select count(*) from public.orders".into(),
        target: RunTarget::All,
        attach: None,
    };
    let t = Instant::now();
    let third = smoke.engine.assistant_send(chat, SendRequest { text: "How many rows are in public.orders? Use the run_query tool to count them, then reply with only the number.".into(), tab: Some(tab) }).await.unwrap();
    let asked = smoke
        .wait_entry(chat, "an approval or the End", |e| {
            matches!(e, Entry::Tool { status: ToolStatus::AwaitingApproval { .. }, .. } | Entry::End { .. })
        })
        .await;
    let Entry::Tool { status: ToolStatus::AwaitingApproval { approval, reason }, tool, .. } = asked else {
        panic!("the turn ended before asking: {asked:?}")
    };
    println!(
        "turn 3: approval {approval:?} ({reason:?}) for {tool:?} after {:.1} s; waiting {} s before answering",
        t.elapsed().as_secs_f32(),
        APPROVAL_DELAY.as_secs()
    );
    tokio::time::sleep(APPROVAL_DELAY).await;
    let answered = smoke.engine.assistant_approve(chat, approval, Decision::Run);
    println!("turn 3: approve after {} s -> {answered:?}", APPROVAL_DELAY.as_secs());
    let outcome = smoke.end(chat).await;
    let view = smoke.engine.assistant_open(chat).await.unwrap();
    let tools: Vec<&Entry> = view
        .entries
        .iter()
        .filter(|s| s.turn == third.turn && matches!(s.entry, Entry::Tool { .. }))
        .map(|s| &s.entry)
        .collect();
    let text = smoke.reply_text(chat, third.turn).await;
    println!("turn 3: {outcome:?} in {:.1} s, tools={tools:?}, reply={text:?}", t.elapsed().as_secs_f32());
    smoke.engine.shutdown().await;
    assert_eq!(
        answered,
        Ok(()),
        "the CLI did not wait {} s for the approval; see \"The CLI waits on `can_use_tool`\" in docs/architecture.md",
        APPROVAL_DELAY.as_secs()
    );
    assert_eq!(outcome, TurnOutcome::Completed);
    assert!(
        tools
            .iter()
            .any(|e| matches!(e, Entry::Tool { status: ToolStatus::Done { result: ToolResult::Rows { .. } }, .. })),
        "the approved query ran: {tools:?}"
    );
}
