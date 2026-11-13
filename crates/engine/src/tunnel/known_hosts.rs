use std::collections::VecDeque;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use russh::keys::known_hosts::{known_host_keys_path, learn_known_hosts_path};
use russh::keys::{HashAlg, PublicKey, PublicKeyBase64};

use crate::model::{HostKeyId, SshError};

const PENDING_TTL: Duration = Duration::from_secs(600);
const PENDING_MAX: usize = 32;

/// Host keys the engine trusts: the user's OpenSSH file, read and never written, and the app's
/// own file, which only `trust` appends to. A key the user is asked about waits in `pending`, so
/// the webview can trust only a key the engine saw.
pub(crate) struct HostKeys {
    user: Option<PathBuf>,
    app: PathBuf,
    pending: Mutex<Pending>,
}

#[derive(Default)]
struct Pending {
    next: u64,
    keys: VecDeque<Observed>,
}

struct Observed {
    id: HostKeyId,
    at: Instant,
    host: String,
    port: u16,
    key: PublicKey,
}

impl Pending {
    fn prune(&mut self) {
        self.keys.retain(|o| o.at.elapsed() < PENDING_TTL);
    }
}

enum Verdict {
    Trusted,
    Unknown,
    Changed(String),
    Revoked(String),
}

impl HostKeys {
    pub fn new(user: Option<PathBuf>, app: PathBuf) -> Self {
        Self { user, app, pending: Mutex::new(Pending::default()) }
    }

    pub fn check(&self, host: &str, port: u16, key: &PublicKey) -> Result<(), SshError> {
        let (host, algorithm, fingerprint) =
            (host.to_owned(), key.algorithm().as_str().to_owned(), key.fingerprint(HashAlg::Sha256).to_string());
        match self.verdict(&host, port, key) {
            Ok(Verdict::Trusted) => Ok(()),
            Ok(Verdict::Unknown) => {
                let trust = self.hold(&host, port, key);
                Err(SshError::HostKeyUnknown { host, port, algorithm, fingerprint, trust })
            }
            Ok(Verdict::Changed(known_at)) => {
                Err(SshError::HostKeyChanged { host, port, algorithm, fingerprint, known_at })
            }
            Ok(Verdict::Revoked(revoked_at)) => {
                Err(SshError::HostKeyRevoked { host, port, algorithm, fingerprint, revoked_at })
            }
            Err(message) => Err(SshError::KnownHosts { message }),
        }
    }

    /// Revoked when a file revokes the key, else Trusted when a file holds it, as OpenSSH does for
    /// a host name shared by several servers. Changed when a file holds only other keys of its
    /// algorithm for the server. A file that exists but cannot be read fails the check.
    fn verdict(&self, host: &str, port: u16, key: &PublicKey) -> Result<Verdict, String> {
        let blob = key.public_key_base64();
        let (mut trusted, mut changed) = (false, None);
        for path in self.user.iter().chain([&self.app]) {
            let unreadable = |e: &dyn std::fmt::Display| format!("cannot read {}: {e}", path.display());
            let text = match std::fs::read_to_string(path) {
                Ok(text) => text,
                Err(e) if e.kind() == ErrorKind::NotFound => continue,
                Err(e) => return Err(unreadable(&e)),
            };
            let at = |line: usize| format!("{}:{line}", path.display());
            if let Some(line) = text.lines().position(|l| revokes(l, &blob)) {
                return Ok(Verdict::Revoked(at(line + 1)));
            }
            for (counted, known) in known_host_keys_path(host, port, path).map_err(|e| unreadable(&e))? {
                if known.algorithm() != key.algorithm() {
                    continue;
                }
                if known.key_data() == key.key_data() {
                    trusted = true;
                } else {
                    changed.get_or_insert_with(|| at(file_line(&text, counted)));
                }
            }
        }
        Ok(match (trusted, changed) {
            (true, _) => Verdict::Trusted,
            (false, Some(known_at)) => Verdict::Changed(known_at),
            (false, None) => Verdict::Unknown,
        })
    }

    fn hold(&self, host: &str, port: u16, key: &PublicKey) -> HostKeyId {
        let mut pending = self.pending.lock().unwrap();
        pending.prune();
        while pending.keys.len() >= PENDING_MAX {
            pending.keys.pop_front();
        }
        pending.next += 1;
        let id = HostKeyId(pending.next);
        pending.keys.push_back(Observed { id, at: Instant::now(), host: host.to_owned(), port, key: key.clone() });
        id
    }

    /// Appends the held key to the app's known_hosts. Sync and std-only, so the IPC command can
    /// run on the main thread.
    pub fn trust(&self, id: HostKeyId) -> Result<(), String> {
        let mut pending = self.pending.lock().unwrap();
        pending.prune();
        let index = pending
            .keys
            .iter()
            .position(|o| o.id == id)
            .ok_or("That host key is no longer waiting to be trusted. Connect again to see it.")?;
        let observed = &pending.keys[index];
        learn_known_hosts_path(&observed.host, observed.port, &observed.key, &self.app)
            .map_err(|e| format!("cannot write {}: {e}", self.app.display()))?;
        pending.keys.remove(index);
        Ok(())
    }
}

/// OpenSSH's `@revoked` marker. The host pattern is ignored, so a revoked key fails for every host.
fn revokes(line: &str, blob: &str) -> bool {
    let mut fields = line.split_whitespace();
    fields.next() == Some("@revoked") && fields.nth(2) == Some(blob)
}

/// russh numbers a match among the lines that do not start with `#`. This is its line in the file.
fn file_line(text: &str, counted: usize) -> usize {
    text.lines()
        .enumerate()
        .filter(|(_, l)| !l.starts_with('#'))
        .nth(counted.saturating_sub(1))
        .map_or(counted, |(i, _)| i + 1)
}
