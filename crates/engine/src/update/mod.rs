mod machine;
mod scripted;

use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::console::now_ms;
use crate::model::{
    AppEvent, ConsoleWork, InstallReply, RestartImpact, UpdateError, UpdateFailure, UpdateState, UpdateTrack,
    UpdatesOff,
};
use crate::store::Store;
use crate::{BoxFuture, Shared};

pub use scripted::ScriptedTransport;

const KV_UPDATES: &str = "updates";
const FIRST_CHECK: Duration = Duration::from_secs(15);
const TICK: Duration = Duration::from_secs(60);
const CHECK_TIMEOUT: Duration = Duration::from_secs(30);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(300);

/// The shell's half of self-update. It makes no decisions: it reports what a feed offers, whatever
/// the version, and the engine decides whether to take it.
pub trait UpdateTransport: Send + Sync + 'static {
    /// The running build. Read once, at open.
    fn installed(&self) -> Installed;
    /// What `feed` offers this platform. `Ok(None)` when the feed or this platform's entry in it
    /// does not exist yet, which counts as nothing new.
    fn fetch<'a>(&'a self, feed: &'a str) -> BoxFuture<'a, Result<Option<Box<dyn Release>>, UpdateFailure>>;
    /// Ends the process through the shell's normal exit, which runs `Engine::shutdown`, and starts
    /// the installed build.
    fn relaunch(&self);
}

pub struct Installed {
    pub version: String,
    pub support: Result<(), UpdatesOff>,
}

pub trait Release: Send + 'static {
    fn version(&self) -> &str;
    /// The feed's whole JSON, for the fields the plugin does not read.
    fn feed(&self) -> &serde_json::Value;
    /// Downloads, then verifies the signature against the key built into the app.
    fn download(self: Box<Self>, progress: Progress) -> BoxFuture<'static, Result<Box<dyn Verified>, UpdateFailure>>;
}

/// Called with the bytes received so far and the total length when the server sent one.
pub type Progress = Box<dyn FnMut(u64, Option<u64>) + Send>;

pub trait Verified: Send + 'static {
    /// Blocking, and may wait on an OS password prompt. On Windows the process exits inside it.
    fn install(self: Box<Self>) -> Result<(), UpdateFailure>;
}

#[derive(Serialize, Deserialize)]
struct UpdatePrefs {
    track: UpdateTrack,
}

/// Lock order: machine, then store.
pub(crate) struct Updates {
    transport: Arc<dyn UpdateTransport>,
    machine: Mutex<machine::Machine<Box<dyn Release>, Box<dyn Verified>>>,
}

impl Updates {
    /// With no saved track, saves the one the build implies, so a nightly user whom the nightly
    /// feed moves onto a stable build stays on Nightly. A build that cannot update saves nothing,
    /// because a debug build shares the installed app's data directory.
    pub(crate) fn open(store: &Store, transport: Arc<dyn UpdateTransport>) -> Self {
        let installed = transport.installed();
        let implied = machine::implied_track(&installed.version);
        let track = match store.kv_get::<UpdatePrefs>(KV_UPDATES) {
            Ok(Some(prefs)) => prefs.track,
            Ok(None) => {
                if machine::updatable(&installed).is_ok()
                    && let Err(e) = store.kv_set(KV_UPDATES, &UpdatePrefs { track: implied })
                {
                    eprintln!("update track save failed: {e}");
                }
                implied
            }
            Err(e) => {
                eprintln!("update track load failed, so it is left as saved: {e}");
                implied
            }
        };
        Self { transport, machine: Mutex::new(machine::Machine::new(installed, track)) }
    }

    pub(crate) fn state(&self) -> UpdateState {
        self.machine.lock().unwrap().state()
    }
}

impl Shared {
    fn edit_update<T>(&self, f: impl FnOnce(&mut machine::Machine<Box<dyn Release>, Box<dyn Verified>>) -> T) -> T {
        let (out, changed) = self.updates.machine.lock().unwrap().apply(f);
        if let Some(state) = changed {
            self.emit(AppEvent::UpdateChanged(state));
        }
        out
    }

    pub(crate) fn update_check(self: &Arc<Self>) {
        if let Some(check) = self.edit_update(|m| m.begin_check()) {
            self.run_check(check);
        }
    }

    fn run_check(self: &Arc<Self>, check: machine::Check) {
        let shared = self.clone();
        self.spawn(async move {
            let fetched = tokio::time::timeout(CHECK_TIMEOUT, shared.updates.transport.fetch(check.feed))
                .await
                .unwrap_or(Err(UpdateFailure::Offline));
            let fetched = fetched.map(|found| {
                found.map(|release| machine::Found {
                    version: release.version().to_owned(),
                    changes: machine::feed_changes(release.feed()),
                    release,
                })
            });
            shared.edit_update(|m| m.finish_check(check.ticket, fetched, now_ms()));
        });
    }

    pub(crate) fn update_download(self: &Arc<Self>, version: &str) -> Result<(), UpdateError> {
        let Some(release) = self.edit_update(|m| m.begin_download(version))? else { return Ok(()) };
        let shared = self.clone();
        let progress: Progress =
            Box::new(move |received, total| shared.edit_update(|m| m.progress(machine::percent(received, total))));
        let shared = self.clone();
        self.spawn(async move {
            let verified = tokio::time::timeout(DOWNLOAD_TIMEOUT, release.download(progress))
                .await
                .unwrap_or(Err(UpdateFailure::Offline));
            shared.edit_update(|m| m.finish_download(verified, now_ms()));
        });
        Ok(())
    }

    pub(crate) async fn update_install(
        self: &Arc<Self>,
        version: &str,
        accepted: Option<RestartImpact>,
    ) -> Result<InstallReply, UpdateError> {
        if self.updates.machine.lock().unwrap().installing(version)? {
            return Ok(InstallReply::Restarting);
        }
        let impact = self.restart_impact().await;
        if !accepted.is_some_and(|a| machine::covers(&a, &impact)) {
            return Ok(InstallReply::Confirm { impact });
        }
        let Some(verified) = self.edit_update(|m| m.begin_install(version))? else {
            return Ok(InstallReply::Restarting);
        };
        let shared = self.clone();
        self.spawn(async move {
            // Before install, because Windows exits inside it without running the shell's exit
            // handler, which would leave an agent process behind.
            shared.shutdown().await;
            // The macOS elevation prompt waits on its caller, so this runs off the async workers.
            let installed = shared
                .runtime
                .spawn_blocking(move || verified.install())
                .await
                .unwrap_or_else(|e| Err(UpdateFailure::Other { message: format!("the installer panicked: {e}") }));
            match installed {
                Ok(()) => shared.updates.transport.relaunch(),
                Err(failure) => shared.edit_update(|m| m.install_failed(failure, now_ms())),
            }
        });
        Ok(InstallReply::Restarting)
    }

    pub(crate) fn update_set_track(self: &Arc<Self>, track: UpdateTrack) -> Result<(), UpdateError> {
        // Saved under the machine lock, so two switches reach the store in the order they took effect.
        let check = self.edit_update(|m| {
            let before = m.track();
            let check = m.set_track(track)?;
            if m.track() != before
                && let Err(e) = self.store.kv_set(KV_UPDATES, &UpdatePrefs { track })
            {
                eprintln!("update track save failed: {e}");
            }
            Ok::<_, UpdateError>(check)
        })?;
        if let Some(check) = check {
            self.run_check(check);
        }
        Ok(())
    }

    /// Work and turns are read after the probes, so what started while they ran is in the list. A
    /// console busy with `Probe` is the engine's own `has_writes` probe, not user work.
    async fn restart_impact(self: &Arc<Self>) -> RestartImpact {
        let mut transactions = self.open_transactions(&self.all_consoles()).await;
        transactions.sort_by_key(|t| t.tab_id.0);
        let mut running: Vec<_> = self
            .all_consoles()
            .iter()
            .filter(|c| c.latest.lock().unwrap().work.is_some_and(|w| w != ConsoleWork::Probe))
            .map(|c| c.tab)
            .collect();
        running.sort_by_key(|t| t.0);
        let mut chats = self.assistant.running_chats();
        chats.sort_by_key(|c| c.0);
        RestartImpact { running, transactions, chats }
    }
}

/// On a tick, so time the machine spends asleep counts toward the wall-clock interval.
pub(crate) async fn schedule(shared: Weak<Shared>) {
    tokio::time::sleep(FIRST_CHECK).await;
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let Some(shared) = shared.upgrade() else { return };
        if shared.updates.machine.lock().unwrap().due(now_ms()) {
            shared.update_check();
        }
    }
}

#[cfg(test)]
mod tests;
