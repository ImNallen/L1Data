use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::{Value, json};

use super::machine::feed;
use super::{Installed, Progress, Release, UpdateTransport, Verified};
use crate::BoxFuture;
use crate::model::{UpdateFailure, UpdateTrack, UpdatesOff};

const CHECK_DELAY: Duration = Duration::from_millis(600);
const DOWNLOAD_STEPS: u64 = 50;
const DOWNLOAD_STEP: Duration = Duration::from_millis(100);
const DOWNLOAD_BYTES: u64 = 24 * 1024 * 1024;

/// Serves scripted feeds, so the engine's real machine and policy run in the dev-bridge and the
/// tests.
pub struct ScriptedTransport {
    script: Script,
    relaunched: AtomicBool,
}

struct Script {
    installed: &'static str,
    support: Result<(), UpdatesOff>,
    stable: Option<&'static str>,
    nightly: Option<&'static str>,
    signature: Result<(), UpdateFailure>,
    install: Result<(), UpdateFailure>,
}

const AVAILABLE: Script = Script {
    installed: "0.1.0",
    support: Ok(()),
    stable: Some("0.2.0"),
    nightly: Some("0.2.1-nightly.20261023.7"),
    signature: Ok(()),
    install: Ok(()),
};

type Item = (&'static str, Option<&'static str>, Option<u32>);

/// Each track's releases, newest first. A feed lists its own release and the ones before it.
const STABLE_HISTORY: &[(&str, &[Item])] = &[
    (
        "0.2.0",
        &[
            (
                "feat(app): name the run target, mark Production, and fix tree keyboard access",
                Some("ImNallen"),
                Some(50),
            ),
            ("feat(app): save queries and reopen them from the sidebar", Some("ImNallen"), Some(49)),
            ("fix(ui): cancel a grid cell edit on Escape", Some("ImNallen"), Some(51)),
        ],
    ),
    (
        "0.1.1",
        &[
            ("fix(engine): keep a numeric's exact digits in the grid", Some("ImNallen"), Some(44)),
            ("chore(ui): tighten the sidebar spacing", None, None),
        ],
    ),
    ("0.1.0", &[("feat(app): the first release", Some("ImNallen"), Some(1))]),
];

const NIGHTLY_HISTORY: &[(&str, &[Item])] = &[
    ("0.2.1-nightly.20261023.7", &[("feat(app): ask before applying edits on Production", Some("ImNallen"), Some(53))]),
    (
        "0.1.1-nightly.20261024.6",
        &[("chore(ui): lint and format the frontend with oxlint and oxfmt", Some("ImNallen"), Some(52))],
    ),
    (
        "0.1.1-nightly.20261023.7",
        &[
            ("fix(ui): cancel a grid cell edit on Escape", Some("ImNallen"), Some(51)),
            ("docs(readme): list the shortcuts", None, None),
        ],
    ),
    (
        "0.1.1-nightly.20261023.5",
        &[("feat(app): save queries and reopen them from the sidebar", Some("ImNallen"), Some(49))],
    ),
];

fn feed_json(version: &str, history: &[(&str, &[Item])]) -> Value {
    let changes: Vec<_> = history
        .iter()
        .skip_while(|(v, _)| *v != version)
        .map(|(v, items)| json!({ "version": v, "items": items.iter().map(|(title, author, pr)| json!({ "title": title, "author": author, "pr": pr })).collect::<Vec<_>>() }))
        .collect();
    json!({ "version": version, "notes": "", "changes": changes })
}

impl ScriptedTransport {
    pub const SCENARIOS: [&str; 7] =
        ["current", "available", "nightly-build", "offline", "bad-signature", "install-fails", "off"];

    pub fn named(name: &str) -> Result<Self, String> {
        let script = match name {
            "current" => Script { stable: Some("0.1.0"), nightly: Some("0.1.1-nightly.20261023.7"), ..AVAILABLE },
            "available" => AVAILABLE,
            "nightly-build" => Script {
                installed: "0.1.1-nightly.20261023.5",
                stable: Some("0.1.0"),
                nightly: Some("0.1.1-nightly.20261024.6"),
                ..AVAILABLE
            },
            "offline" => Script { stable: None, nightly: None, ..AVAILABLE },
            "bad-signature" => Script { signature: Err(UpdateFailure::BadSignature), ..AVAILABLE },
            "install-fails" => Script { install: Err(UpdateFailure::ReadOnlyLocation), ..AVAILABLE },
            "off" => Script { support: Err(UpdatesOff::DevBuild), ..AVAILABLE },
            _ => {
                return Err(format!(
                    "unknown update scenario {name:?}; expected one of {}",
                    Self::SCENARIOS.join(", ")
                ));
            }
        };
        Ok(Self { script, relaunched: AtomicBool::new(false) })
    }

    pub fn relaunched(&self) -> bool {
        self.relaunched.load(Ordering::SeqCst)
    }
}

impl UpdateTransport for ScriptedTransport {
    fn installed(&self) -> Installed {
        Installed { version: self.script.installed.into(), support: self.script.support }
    }

    fn fetch<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<Option<Box<dyn Release>>, UpdateFailure>> {
        Box::pin(async move {
            tokio::time::sleep(CHECK_DELAY).await;
            let (offered, history) = if url == feed(UpdateTrack::Stable) {
                (self.script.stable, STABLE_HISTORY)
            } else {
                (self.script.nightly, NIGHTLY_HISTORY)
            };
            let version = offered.ok_or(UpdateFailure::Offline)?;
            let feed = feed_json(version, history);
            let release = ScriptedRelease {
                version,
                feed,
                signature: self.script.signature.clone(),
                install: self.script.install.clone(),
            };
            Ok(Some(Box::new(release) as Box<dyn Release>))
        })
    }

    fn relaunch(&self) {
        self.relaunched.store(true, Ordering::SeqCst);
        eprintln!("dev-bridge: the app would restart into the installed build now");
    }
}

struct ScriptedRelease {
    version: &'static str,
    feed: Value,
    signature: Result<(), UpdateFailure>,
    install: Result<(), UpdateFailure>,
}

impl Release for ScriptedRelease {
    fn version(&self) -> &str {
        self.version
    }

    fn feed(&self) -> &Value {
        &self.feed
    }

    /// Like the plugin, it verifies the signature after the last chunk.
    fn download(
        self: Box<Self>,
        mut progress: Progress,
    ) -> BoxFuture<'static, Result<Box<dyn Verified>, UpdateFailure>> {
        Box::pin(async move {
            for step in 1..=DOWNLOAD_STEPS {
                tokio::time::sleep(DOWNLOAD_STEP).await;
                progress(DOWNLOAD_BYTES * step / DOWNLOAD_STEPS, Some(DOWNLOAD_BYTES));
            }
            self.signature?;
            Ok(Box::new(ScriptedInstall(self.install)) as Box<dyn Verified>)
        })
    }
}

struct ScriptedInstall(Result<(), UpdateFailure>);

impl Verified for ScriptedInstall {
    fn install(self: Box<Self>) -> Result<(), UpdateFailure> {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_listed_scenario_opens_and_an_unknown_one_lists_them() {
        for name in ScriptedTransport::SCENARIOS {
            assert!(ScriptedTransport::named(name).is_ok(), "{name}");
        }
        let error = ScriptedTransport::named("nope").err().unwrap();
        assert!(ScriptedTransport::SCENARIOS.iter().all(|name| error.contains(name)), "{error}");
    }
}
