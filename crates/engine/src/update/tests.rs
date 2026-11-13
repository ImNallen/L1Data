use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::broadcast;

use super::ScriptedTransport;
use crate::model::*;
use crate::{AgentPath, Engine, EngineConfig, MemoryKeyStore};

async fn open(dir: &Path, transport: &Arc<ScriptedTransport>) -> Arc<Engine> {
    let updates = transport.clone();
    Engine::open(EngineConfig {
        data_dir: dir.into(),
        keys: Arc::new(MemoryKeyStore::default()),
        agent_path: AgentPath::Fixed("".into()),
        az_binary: None,
        user_known_hosts: None,
        updates,
    })
    .await
    .unwrap()
}

async fn engine_with(scenario: &str) -> (Arc<Engine>, Arc<ScriptedTransport>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let transport = Arc::new(ScriptedTransport::named(scenario).unwrap());
    (open(dir.path(), &transport).await, transport, dir)
}

fn saved_track(engine: &Engine) -> Option<Value> {
    engine.shared.store.kv_get::<Value>(super::KV_UPDATES).unwrap()
}

async fn next_update(events: &mut broadcast::Receiver<AppEvent>, until: impl Fn(&UpdateStatus) -> bool) -> UpdateState {
    let found = async {
        loop {
            if let Ok(AppEvent::UpdateChanged(state)) = events.recv().await
                && until(&state.status)
            {
                return state;
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(600), found).await.expect("no such update state within ten minutes")
}

async fn until_relaunched(transport: &ScriptedTransport) {
    while !transport.relaunched() {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn available(version: &str, downgrade: bool) -> UpdateStatus {
    UpdateStatus::Available { version: version.into(), downgrade, changes: vec![] }
}

/// The offer with its change list left out, for the tests about which release is offered.
fn offer(status: UpdateStatus) -> UpdateStatus {
    match status {
        UpdateStatus::Available { version, downgrade, .. } => available(&version, downgrade),
        other => other,
    }
}

fn listed(status: &UpdateStatus) -> Vec<&str> {
    match status {
        UpdateStatus::Available { changes, .. }
        | UpdateStatus::Downloading { changes, .. }
        | UpdateStatus::Downloaded { changes, .. } => changes.iter().map(|c| c.version.as_str()).collect(),
        _ => vec![],
    }
}

async fn downloaded(engine: &Engine, events: &mut broadcast::Receiver<AppEvent>, version: &str) -> UpdateState {
    engine.update_check();
    next_update(events, |s| matches!(s, UpdateStatus::Available { .. })).await;
    engine.update_download(version.into()).unwrap();
    next_update(events, |s| matches!(s, UpdateStatus::Downloaded { .. } | UpdateStatus::Failed { .. })).await
}

#[tokio::test(start_paused = true)]
async fn the_first_check_runs_fifteen_seconds_after_open() {
    let (engine, _transport, _dir) = engine_with("available").await;
    let mut events = engine.subscribe();
    let opened = tokio::time::Instant::now();
    assert_eq!(engine.boot().updates.status, UpdateStatus::Idle);
    next_update(&mut events, |s| *s == UpdateStatus::Checking).await;
    let waited = opened.elapsed();
    assert!((Duration::from_secs(15)..Duration::from_secs(16)).contains(&waited), "{waited:?}");
    let state = next_update(&mut events, |s| *s != UpdateStatus::Checking).await;
    assert_eq!(
        (offer(state.status), state.track, state.current_version.as_str()),
        (available("0.2.0", false), UpdateTrack::Stable, "0.1.0")
    );
    assert!(state.checked_at_ms.is_some());
}

#[tokio::test(start_paused = true)]
async fn a_nightly_build_switched_to_stable_is_offered_the_older_stable_and_restarts_into_it() {
    let (engine, transport, _dir) = engine_with("nightly-build").await;
    let mut events = engine.subscribe();
    assert_eq!(engine.boot().updates.track, UpdateTrack::Nightly);
    engine.update_set_track(UpdateTrack::Stable).unwrap();
    let state = next_update(&mut events, |s| *s != UpdateStatus::Checking).await;
    assert_eq!((state.status, state.track), (available("0.1.0", true), UpdateTrack::Stable));
    engine.update_set_track(UpdateTrack::Nightly).unwrap();
    let state = next_update(&mut events, |s| *s != UpdateStatus::Checking).await;
    assert_eq!(
        listed(&state.status),
        ["0.1.1-nightly.20261024.6", "0.1.1-nightly.20261023.7"],
        "the running nightly is not listed"
    );
    assert_eq!(
        offer(state.status),
        available("0.1.1-nightly.20261024.6", false),
        "switching back offers the newer nightly"
    );
    engine.update_set_track(UpdateTrack::Stable).unwrap();
    assert_eq!(next_update(&mut events, |s| *s != UpdateStatus::Checking).await.status, available("0.1.0", true));
    assert_eq!(saved_track(&engine), Some(json!({ "track": "Stable" })));

    engine.update_download("0.1.0".into()).unwrap();
    let state = next_update(&mut events, |s| matches!(s, UpdateStatus::Downloaded { .. })).await;
    assert_eq!(state.status, UpdateStatus::Downloaded { version: "0.1.0".into(), downgrade: true, changes: vec![] });
    let Ok(InstallReply::Confirm { impact }) = engine.update_install("0.1.0".into(), None).await else {
        panic!("a restart always asks first")
    };
    assert_eq!(impact, RestartImpact::default());
    assert!(matches!(engine.update_install("0.1.0".into(), Some(impact)).await, Ok(InstallReply::Restarting)));
    assert_eq!(engine.boot().updates.status, UpdateStatus::Installing { version: "0.1.0".into() });
    until_relaunched(&transport).await;
}

#[tokio::test(start_paused = true)]
async fn the_first_launch_saves_the_implied_track_and_a_later_stable_build_keeps_it() {
    let dir = tempfile::tempdir().unwrap();
    let nightly = open(dir.path(), &Arc::new(ScriptedTransport::named("nightly-build").unwrap())).await;
    assert_eq!(saved_track(&nightly), Some(json!({ "track": "Nightly" })));
    nightly.shutdown().await;
    drop(nightly);
    let stable = open(dir.path(), &Arc::new(ScriptedTransport::named("available").unwrap())).await;
    assert_eq!(stable.boot().updates.track, UpdateTrack::Nightly);
}

#[tokio::test(start_paused = true)]
async fn a_build_that_cannot_update_saves_no_track_until_one_is_chosen() {
    let (engine, _transport, _dir) = engine_with("off").await;
    let mut events = engine.subscribe();
    assert_eq!(saved_track(&engine), None);
    tokio::time::sleep(Duration::from_secs(3600)).await;
    assert_eq!(engine.boot().updates.status, UpdateStatus::Disabled { reason: UpdatesOff::DevBuild });
    engine.update_set_track(UpdateTrack::Nightly).unwrap();
    let state = next_update(&mut events, |_| true).await;
    assert_eq!(
        (state.track, state.status),
        (UpdateTrack::Nightly, UpdateStatus::Disabled { reason: UpdatesOff::DevBuild })
    );
    assert_eq!(saved_track(&engine), Some(json!({ "track": "Nightly" })));
}

#[tokio::test(start_paused = true)]
async fn a_track_this_build_cannot_read_is_left_as_saved() {
    let dir = tempfile::tempdir().unwrap();
    let first = open(dir.path(), &Arc::new(ScriptedTransport::named("off").unwrap())).await;
    first.shared.store.kv_set(super::KV_UPDATES, &json!({ "track": "Beta" })).unwrap();
    first.shutdown().await;
    drop(first);
    let engine = open(dir.path(), &Arc::new(ScriptedTransport::named("available").unwrap())).await;
    assert_eq!(engine.boot().updates.track, UpdateTrack::Stable);
    assert_eq!(saved_track(&engine), Some(json!({ "track": "Beta" })));
}

#[tokio::test(start_paused = true)]
async fn a_failed_install_publishes_the_failure_and_does_not_relaunch() {
    let (engine, transport, _dir) = engine_with("install-fails").await;
    let mut events = engine.subscribe();
    downloaded(&engine, &mut events, "0.2.0").await;
    let Ok(InstallReply::Confirm { impact }) = engine.update_install("0.2.0".into(), None).await else {
        panic!("a restart always asks first")
    };
    assert!(matches!(engine.update_install("0.2.0".into(), Some(impact)).await, Ok(InstallReply::Restarting)));
    let state = next_update(&mut events, |s| matches!(s, UpdateStatus::Failed { .. })).await;
    assert_eq!(state.status, UpdateStatus::Failed { failure: UpdateFailure::ReadOnlyLocation });
    assert!(!transport.relaunched());
}

#[tokio::test(start_paused = true)]
async fn a_bad_signature_fails_the_download_and_the_next_step_is_a_check() {
    let (engine, _transport, _dir) = engine_with("bad-signature").await;
    let mut events = engine.subscribe();
    let state = downloaded(&engine, &mut events, "0.2.0").await;
    assert_eq!(state.status, UpdateStatus::Failed { failure: UpdateFailure::BadSignature });
    assert!(matches!(engine.update_install("0.2.0".into(), None).await, Err(UpdateError::Stale)));
    engine.update_check();
    assert_eq!(
        offer(next_update(&mut events, |s| *s != UpdateStatus::Checking).await.status),
        available("0.2.0", false)
    );
}

#[tokio::test(start_paused = true)]
async fn an_unreachable_feed_fails_the_check() {
    let (engine, _transport, _dir) = engine_with("offline").await;
    let mut events = engine.subscribe();
    engine.update_check();
    let state = next_update(&mut events, |s| *s != UpdateStatus::Checking).await;
    assert_eq!((state.status, state.checked_at_ms), (UpdateStatus::Failed { failure: UpdateFailure::Offline }, None));
}

#[tokio::test(start_paused = true)]
async fn switching_tracks_during_a_download_is_busy_and_saves_nothing() {
    let (engine, _transport, _dir) = engine_with("available").await;
    let mut events = engine.subscribe();
    engine.update_check();
    next_update(&mut events, |s| matches!(s, UpdateStatus::Available { .. })).await;
    engine.update_download("0.2.0".into()).unwrap();
    next_update(&mut events, |s| matches!(s, UpdateStatus::Downloading { percent: Some(p), .. } if *p > 0)).await;
    assert!(matches!(engine.update_set_track(UpdateTrack::Nightly), Err(UpdateError::Busy)));
    assert_eq!(engine.boot().updates.track, UpdateTrack::Stable);
    assert_eq!(saved_track(&engine), Some(json!({ "track": "Stable" })));
    assert!(engine.update_download("0.2.0".into()).is_ok(), "a repeated download is a no-op");
    assert!(matches!(engine.update_download("0.1.9".into()), Err(UpdateError::Stale)));
    next_update(&mut events, |s| matches!(s, UpdateStatus::Downloaded { .. })).await;
}

#[tokio::test(start_paused = true)]
async fn update_check_runs_from_a_thread_with_no_runtime() {
    let (engine, _transport, _dir) = engine_with("available").await;
    let mut events = engine.subscribe();
    let caller = engine.clone();
    std::thread::spawn(move || caller.update_check()).join().unwrap();
    assert_eq!(
        offer(next_update(&mut events, |s| *s != UpdateStatus::Checking).await.status),
        available("0.2.0", false)
    );
}

#[tokio::test(start_paused = true)]
async fn update_download_runs_from_a_thread_with_no_runtime() {
    let (engine, _transport, _dir) = engine_with("available").await;
    let mut events = engine.subscribe();
    engine.update_check();
    next_update(&mut events, |s| matches!(s, UpdateStatus::Available { .. })).await;
    let caller = engine.clone();
    std::thread::spawn(move || caller.update_download("0.2.0".into())).join().unwrap().unwrap();
    next_update(&mut events, |s| matches!(s, UpdateStatus::Downloaded { .. })).await;
}

#[tokio::test(start_paused = true)]
async fn update_set_track_runs_from_a_thread_with_no_runtime() {
    let (engine, _transport, _dir) = engine_with("available").await;
    let mut events = engine.subscribe();
    let caller = engine.clone();
    std::thread::spawn(move || caller.update_set_track(UpdateTrack::Nightly)).join().unwrap().unwrap();
    let state = next_update(&mut events, |s| *s != UpdateStatus::Checking).await;
    assert_eq!(
        (state.track, offer(state.status)),
        (UpdateTrack::Nightly, available("0.2.1-nightly.20261023.7", false))
    );
}

#[tokio::test(start_paused = true)]
async fn the_offer_lists_what_changed_since_the_running_build_through_the_download() {
    let (engine, _transport, _dir) = engine_with("available").await;
    let mut events = engine.subscribe();
    engine.update_check();
    let state = next_update(&mut events, |s| matches!(s, UpdateStatus::Available { .. })).await;
    assert_eq!(listed(&state.status), ["0.2.0", "0.1.1"]);
    engine.update_download("0.2.0".into()).unwrap();
    let state = next_update(&mut events, |s| matches!(s, UpdateStatus::Downloading { .. })).await;
    assert_eq!(listed(&state.status), ["0.2.0", "0.1.1"]);
    let state = next_update(&mut events, |s| matches!(s, UpdateStatus::Downloaded { .. })).await;
    assert_eq!(listed(&state.status), ["0.2.0", "0.1.1"]);
}
