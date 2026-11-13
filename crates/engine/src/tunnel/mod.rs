mod auth;
#[cfg(all(test, feature = "ssh-tests"))]
mod docker_tests;
mod known_hosts;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::task::{Context, Poll};
use std::time::Duration;

use russh::ChannelStream;
use russh::client::{self, Handle};
use russh::keys::PublicKeyOrCertificate;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::time::timeout;
use zeroize::Zeroizing;

use crate::model::{ConnectError, ConnectionId, HostKeyId, SshAuth, SshError};
use known_hosts::HostKeys;

const DIAL_TIMEOUT: Duration = Duration::from_secs(10);
/// Long enough for a hardware key that waits for a touch.
const AUTH_TIMEOUT: Duration = Duration::from_secs(60);
const CHANNEL_TIMEOUT: Duration = Duration::from_secs(10);
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);
/// russh grants a channel more window as data arrives, then waits for room in the channel's
/// buffer inside the connection's one loop. A stream nobody reads stalls every session on the
/// connection once this many packets queue for it.
const CHANNEL_BUFFER: usize = 256;

/// The user's OpenSSH known_hosts, which the engine reads and never writes.
pub fn openssh_known_hosts() -> Option<PathBuf> {
    std::env::home_dir().map(|home| home.join(".ssh").join("known_hosts"))
}

/// Everything that decides which SSH connection carries a session. Sessions share one only when
/// they belong to the same connection and every field matches, so an edited config dials anew.
#[derive(PartialEq, Eq, Hash)]
pub(crate) struct TunnelSpec {
    host: String,
    port: u16,
    user: String,
    auth: SshAuth,
    secret: Option<Secret>,
}

#[derive(PartialEq, Eq)]
struct Secret(Zeroizing<String>);

impl Hash for Secret {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.as_str().hash(state);
    }
}

impl TunnelSpec {
    pub fn new(host: &str, port: u16, user: &str, auth: &SshAuth, secret: Option<String>) -> Self {
        Self {
            host: host.to_owned(),
            port,
            user: user.to_owned(),
            auth: auth.clone(),
            secret: secret.map(|s| Secret(Zeroizing::new(s))),
        }
    }

    fn secret(&self) -> Option<&str> {
        self.secret.as_ref().map(|s| s.0.as_str())
    }
}

impl fmt::Debug for TunnelSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TunnelSpec")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("user", &self.user)
            .field("auth", &self.auth)
            .field("secret", &self.secret.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// The engine's SSH connections. It holds each only weakly: a connection lives exactly as long as
/// some database stream rides it.
pub(crate) struct Tunnels {
    hosts: Arc<HostKeys>,
    cells: Mutex<HashMap<(ConnectionId, Arc<TunnelSpec>), Arc<Cell>>>,
}

impl Tunnels {
    pub fn new(user_known_hosts: Option<PathBuf>, app_known_hosts: PathBuf) -> Self {
        Self { hosts: Arc::new(HostKeys::new(user_known_hosts, app_known_hosts)), cells: Mutex::new(HashMap::new()) }
    }

    /// The route for a session of `owner`. `None` gets a route no other session shares, so Test
    /// always dials and authenticates for real. Password auth with no password fails here,
    /// before anything is dialed.
    pub fn route(&self, owner: Option<ConnectionId>, spec: TunnelSpec) -> Result<SshRoute, ConnectError> {
        if spec.auth == SshAuth::Password && spec.secret.is_none() {
            return Err(ConnectError::Ssh { error: SshError::MissingSecret });
        }
        let spec = Arc::new(spec);
        let fresh = || Arc::new(Cell::new(spec.clone(), self.hosts.clone()));
        let cell = match owner {
            None => fresh(),
            Some(id) => self.cells.lock().unwrap().entry((id, spec.clone())).or_insert_with(fresh).clone(),
        };
        Ok(SshRoute(cell))
    }

    /// Drops the connection's routes, so its next session dials. Streams already open keep theirs.
    pub fn forget(&self, id: ConnectionId) {
        self.cells.lock().unwrap().retain(|(owner, _), _| *owner != id);
    }

    pub fn trust(&self, key: HostKeyId) -> Result<(), String> {
        self.hosts.trust(key)
    }
}

/// One spec's SSH connection, dialed on demand. Dials are single-flight: callers that arrive
/// during one wait for it and share its connection or its error.
struct Cell {
    spec: Arc<TunnelSpec>,
    hosts: Arc<HostKeys>,
    /// Dials finished, published only after the lock is released. A caller reads it before
    /// waiting, so a failure newer than that read is the one it waited on, and an older one is
    /// not handed to it. Publishing under the lock would let a caller read the new count and then
    /// wait, see no newer failure, and dial again.
    finished: AtomicU64,
    state: tokio::sync::Mutex<CellState>,
}

#[derive(Default)]
struct CellState {
    up: Weak<Tunnel>,
    failed: Option<(u64, ConnectError)>,
}

impl Cell {
    fn new(spec: Arc<TunnelSpec>, hosts: Arc<HostKeys>) -> Self {
        Self { spec, hosts, finished: AtomicU64::new(0), state: tokio::sync::Mutex::new(CellState::default()) }
    }

    async fn tunnel(&self) -> Result<Arc<Tunnel>, ConnectError> {
        let seen = self.finished.load(Ordering::Acquire);
        let mut state = self.state.lock().await;
        if let Some(tunnel) = state.up.upgrade().filter(|t| !t.handle.is_closed()) {
            return Ok(tunnel);
        }
        if let Some((attempt, error)) = &state.failed
            && *attempt > seen
        {
            return Err(error.clone());
        }
        let dialed = dial(&self.spec, &self.hosts).await;
        let attempt = self.finished.load(Ordering::Relaxed) + 1;
        let result = match dialed {
            Ok(handle) => {
                let tunnel = Arc::new(Tunnel { handle });
                *state = CellState { up: Arc::downgrade(&tunnel), failed: None };
                Ok(tunnel)
            }
            Err(error) => {
                state.failed = Some((attempt, error.clone()));
                Err(error)
            }
        };
        drop(state);
        self.finished.store(attempt, Ordering::Release);
        result
    }
}

#[derive(Clone)]
pub(crate) struct SshRoute(Arc<Cell>);

impl fmt::Debug for SshRoute {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("SshRoute").field(&self.0.spec).finish()
    }
}

impl SshRoute {
    /// A stream to `host:port` as the SSH server resolves it. A tunnel that died since the last
    /// open is dialed again, and an open that raced its death is retried once on a fresh one.
    pub async fn open(&self, host: &str, port: u16) -> Result<TunnelStream, ConnectError> {
        let spec = &self.0.spec;
        let ssh = |error| ConnectError::Ssh { error };
        let mut retried = false;
        loop {
            let tunnel = self.0.tunnel().await?;
            let opened =
                timeout(CHANNEL_TIMEOUT, tunnel.handle.channel_open_direct_tcpip(host, port.into(), "127.0.0.1", 0))
                    .await;
            let message = match opened {
                Ok(Ok(channel)) => return Ok(TunnelStream { inner: channel.into_stream(), _tunnel: tunnel }),
                Ok(Err(_)) if tunnel.handle.is_closed() && !retried => {
                    retried = true;
                    continue;
                }
                Ok(Err(_)) if tunnel.handle.is_closed() => {
                    return Err(ssh(SshError::Network {
                        message: format!("the connection to {}:{} closed", spec.host, spec.port),
                    }));
                }
                Ok(Err(russh::Error::ChannelOpenFailure(reason))) => format!("{reason:?}"),
                Ok(Err(e)) => e.to_string(),
                Err(_) => format!("no answer within {} s", CHANNEL_TIMEOUT.as_secs()),
            };
            return Err(ssh(SshError::Forward {
                message: format!("{}:{} could not open {host}:{port}: {message}", spec.host, spec.port),
            }));
        }
    }
}

struct Tunnel {
    handle: Handle<Client>,
}

/// One channel through a tunnel. It keeps the tunnel's connection open while it lives.
pub(crate) struct TunnelStream {
    inner: ChannelStream<client::Msg>,
    _tunnel: Arc<Tunnel>,
}

impl AsyncRead for TunnelStream {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for TunnelStream {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

struct Client {
    host: String,
    port: u16,
    hosts: Arc<HostKeys>,
}

#[derive(Debug)]
enum Failure {
    Ssh(russh::Error),
    Refused(SshError),
}

impl From<russh::Error> for Failure {
    fn from(e: russh::Error) -> Self {
        Failure::Ssh(e)
    }
}

impl client::Handler for Client {
    type Error = Failure;

    async fn check_server_key(&mut self, key: &PublicKeyOrCertificate) -> Result<bool, Failure> {
        let PublicKeyOrCertificate::PublicKey { key, .. } = key else { return Ok(false) };
        self.hosts.check(&self.host, self.port, key).map(|()| true).map_err(Failure::Refused)
    }
}

async fn dial(spec: &TunnelSpec, hosts: &Arc<HostKeys>) -> Result<Handle<Client>, ConnectError> {
    let ssh = |error| ConnectError::Ssh { error };
    let server = format!("{}:{}", spec.host, spec.port);
    let config = Arc::new(client::Config {
        keepalive_interval: Some(KEEPALIVE_INTERVAL),
        keepalive_max: 3,
        inactivity_timeout: None,
        channel_buffer_size: CHANNEL_BUFFER,
        nodelay: true,
        ..Default::default()
    });
    let client = Client { host: spec.host.clone(), port: spec.port, hosts: hosts.clone() };
    let mut handle = match timeout(DIAL_TIMEOUT, client::connect(config, (spec.host.as_str(), spec.port), client)).await
    {
        Ok(Ok(handle)) => handle,
        Ok(Err(Failure::Refused(error))) => return Err(ssh(error)),
        Ok(Err(Failure::Ssh(e))) => return Err(ssh(SshError::Network { message: format!("{server}: {e}") })),
        Err(_) => {
            return Err(ssh(SshError::Network {
                message: format!("{server} did not finish the SSH handshake within {} s", DIAL_TIMEOUT.as_secs()),
            }));
        }
    };
    match timeout(AUTH_TIMEOUT, auth::authenticate(&mut handle, spec)).await {
        Ok(Ok(())) => Ok(handle),
        Ok(Err(error)) => Err(ssh(error)),
        Err(_) => Err(ssh(SshError::Auth {
            message: format!(
                "{server} did not finish authenticating {} within {} s",
                spec.user,
                AUTH_TIMEOUT.as_secs()
            ),
        })),
    }
}
