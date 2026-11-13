use std::path::Path;
use std::sync::atomic::AtomicUsize;

use russh::keys::{HashAlg, PrivateKey, PublicKey, PublicKeyBase64};
use russh::server::{self, Auth, Msg, Session};
use russh::{Channel, ChannelOpenFailure, Disconnect, MethodKind, MethodSet};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::*;

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../dev/sshd");
const HOST_KEY: &str = "ssh_host_ed25519_key";
/// More packets than russh's default channel buffer of 100, fewer than `CHANNEL_BUFFER`.
const FLOOD_PACKETS: usize = 200;

fn private(name: &str) -> PrivateKey {
    russh::keys::load_secret_key(Path::new(FIXTURE).join(name), None).unwrap()
}

fn public(name: &str) -> PublicKey {
    russh::keys::load_public_key(Path::new(FIXTURE).join(format!("{name}.pub"))).unwrap()
}

#[derive(Clone, Copy, PartialEq)]
enum Login {
    Password,
    KeyboardInteractive,
    Refuse,
}

#[derive(Default)]
struct Counters {
    connections: AtomicUsize,
    auths: AtomicUsize,
    live: AtomicUsize,
    flooded: AtomicUsize,
}

impl Counters {
    fn get(n: &AtomicUsize) -> usize {
        n.load(Ordering::SeqCst)
    }
}

/// An SSH server in the test process. It forwards to an echo listener, refuses every other
/// destination except `flood`, which sends `FLOOD_PACKETS` packets nobody asked for.
struct Bastion {
    port: u16,
    echo: u16,
    counters: Arc<Counters>,
    sessions: Arc<Mutex<Vec<server::Handle>>>,
}

#[derive(Clone)]
struct Gate {
    login: Login,
    echo: u16,
    counters: Arc<Counters>,
}

impl server::Handler for Gate {
    type Error = russh::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        Ok(self.accept(self.login == Login::Password && user == "tunnel" && password == "tunnel"))
    }

    async fn auth_publickey(&mut self, user: &str, key: &PublicKey) -> Result<Auth, Self::Error> {
        let authorized = ["keys/id_ed25519", "keys/id_ed25519_passphrase", "keys/id_rsa"]
            .iter()
            .any(|k| public(k).key_data() == key.key_data());
        Ok(self.accept(self.login != Login::Refuse && user == "tunnel" && authorized))
    }

    async fn auth_keyboard_interactive<'a>(
        &'a mut self,
        user: &str,
        _: &str,
        response: Option<server::Response<'a>>,
    ) -> Result<Auth, Self::Error> {
        if self.login != Login::KeyboardInteractive {
            return Ok(Auth::reject());
        }
        Ok(match response {
            None => Auth::Partial {
                name: "".into(),
                instructions: "".into(),
                prompts: vec![("Password: ".into(), false)].into(),
            },
            Some(mut answers) => {
                let answer = answers.next();
                self.accept(user == "tunnel" && answer.as_deref() == Some(b"tunnel".as_slice()))
            }
        })
    }

    async fn channel_open_direct_tcpip(
        &mut self,
        channel: Channel<Msg>,
        host: &str,
        port: u32,
        _: &str,
        _: u32,
        reply: server::ChannelOpenHandle,
        _: &mut Session,
    ) -> Result<(), Self::Error> {
        let counters = self.counters.clone();
        match (host, port) {
            ("127.0.0.1", port) if port == u32::from(self.echo) => {
                let mut target = TcpStream::connect(("127.0.0.1", self.echo)).await?;
                reply.accept().await;
                tokio::spawn(async move {
                    let _ = tokio::io::copy_bidirectional(&mut channel.into_stream(), &mut target).await;
                });
            }
            ("flood", _) => {
                reply.accept().await;
                tokio::spawn(async move {
                    let mut stream = channel.into_stream();
                    for _ in 0..FLOOD_PACKETS {
                        stream.write_all(&[b'x'; 512]).await.unwrap();
                        stream.flush().await.unwrap();
                    }
                    counters.flooded.store(1, Ordering::SeqCst);
                    std::future::pending::<()>().await;
                });
            }
            _ => reply.reject(ChannelOpenFailure::ConnectFailed).await,
        }
        Ok(())
    }
}

impl Gate {
    fn accept(&self, ok: bool) -> Auth {
        if ok {
            self.counters.auths.fetch_add(1, Ordering::SeqCst);
            Auth::Accept
        } else {
            Auth::reject()
        }
    }
}

impl Bastion {
    async fn start(login: Login) -> Self {
        let echo = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let echo_port = echo.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = echo.accept().await {
                tokio::spawn(async move {
                    let (mut read, mut write) = socket.split();
                    let _ = tokio::io::copy(&mut read, &mut write).await;
                });
            }
        });
        let methods = match login {
            Login::KeyboardInteractive => {
                MethodSet::from(&[MethodKind::PublicKey, MethodKind::KeyboardInteractive][..])
            }
            _ => MethodSet::from(&[MethodKind::Password, MethodKind::PublicKey][..]),
        };
        let config = Arc::new(server::Config {
            keys: vec![private(HOST_KEY)],
            methods,
            auth_rejection_time: Duration::from_millis(1),
            auth_rejection_time_initial: Some(Duration::ZERO),
            ..Default::default()
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let counters = Arc::new(Counters::default());
        let sessions = Arc::new(Mutex::new(Vec::new()));
        let (gate, held) = (Gate { login, echo: echo_port, counters: counters.clone() }, sessions.clone());
        tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                gate.counters.connections.fetch_add(1, Ordering::SeqCst);
                let (config, gate, held) = (config.clone(), gate.clone(), held.clone());
                tokio::spawn(async move {
                    let Ok(running) = server::run_stream(config, socket, gate.clone()).await else { return };
                    held.lock().unwrap().push(running.handle());
                    gate.counters.live.fetch_add(1, Ordering::SeqCst);
                    let _ = running.await;
                    gate.counters.live.fetch_sub(1, Ordering::SeqCst);
                });
            }
        });
        Self { port, echo: echo_port, counters, sessions }
    }

    fn connections(&self) -> usize {
        Counters::get(&self.counters.connections)
    }

    fn auths(&self) -> usize {
        Counters::get(&self.counters.auths)
    }

    async fn drop_connections(&self) {
        let sessions: Vec<_> = self.sessions.lock().unwrap().drain(..).collect();
        for handle in sessions {
            let _ = handle.disconnect(Disconnect::ByApplication, "test".into(), "".into()).await;
        }
    }

    fn known_hosts_line(&self) -> String {
        format!("[127.0.0.1]:{} {}\n", self.port, public(HOST_KEY).to_openssh().unwrap())
    }
}

struct Rig {
    dir: tempfile::TempDir,
    bastion: Bastion,
    tunnels: Tunnels,
}

const OWNER: ConnectionId = ConnectionId(uuid::Uuid::nil());

impl Rig {
    /// A bastion and empty known_hosts files. Both files are read at each dial, so a test writes
    /// them whenever it likes.
    async fn new(login: Login) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let tunnels = Tunnels::new(Some(dir.path().join("user_known_hosts")), dir.path().join("known_hosts"));
        Self { dir, bastion: Bastion::start(login).await, tunnels }
    }

    /// A bastion whose host key the app's known_hosts already trusts.
    async fn trusted(login: Login) -> Self {
        let rig = Self::new(login).await;
        std::fs::write(rig.app_known_hosts(), rig.bastion.known_hosts_line()).unwrap();
        rig
    }

    fn app_known_hosts(&self) -> PathBuf {
        self.dir.path().join("known_hosts")
    }

    fn user_known_hosts(&self) -> PathBuf {
        self.dir.path().join("user_known_hosts")
    }

    fn spec(&self, auth: SshAuth, secret: Option<&str>) -> TunnelSpec {
        TunnelSpec::new("127.0.0.1", self.bastion.port, "tunnel", &auth, secret.map(str::to_owned))
    }

    fn password(&self, secret: &str) -> SshRoute {
        self.tunnels.route(Some(OWNER), self.spec(SshAuth::Password, Some(secret))).unwrap()
    }

    /// A copy of a fixture key with the mode a user's key has.
    fn key(&self, name: &str) -> SshAuth {
        let path = self.dir.path().join(name.replace('/', "_"));
        std::fs::copy(Path::new(FIXTURE).join(name), &path).unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600)).unwrap();
        SshAuth::Key { path: path.display().to_string() }
    }

    async fn echo(&self, route: &SshRoute) -> TunnelStream {
        let mut stream = route.open("127.0.0.1", self.bastion.echo).await.unwrap();
        assert_round_trip(&mut stream).await;
        stream
    }

    async fn refusal(&self, route: SshRoute) -> SshError {
        ssh_error(route.open("127.0.0.1", self.bastion.echo).await)
    }
}

async fn assert_round_trip(stream: &mut TunnelStream) {
    stream.write_all(b"ping").await.unwrap();
    let mut back = [0; 4];
    timeout(Duration::from_secs(5), stream.read_exact(&mut back)).await.expect("echo within 5 s").unwrap();
    assert_eq!(&back, b"ping");
}

fn ssh_error(result: Result<TunnelStream, ConnectError>) -> SshError {
    match result {
        Err(ConnectError::Ssh { error }) => error,
        Err(other) => panic!("not an SSH error: {other:?}"),
        Ok(_) => panic!("the open succeeded"),
    }
}

async fn eventually(what: &str, done: impl Fn() -> bool) {
    let started = std::time::Instant::now();
    while !done() {
        assert!(started.elapsed() < Duration::from_secs(5), "{what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn an_unknown_host_key_is_trusted_once_and_then_connects() {
    let user = "# the user's own file\n";
    let rig = Rig::new(Login::Password).await;
    std::fs::write(rig.user_known_hosts(), user).unwrap();
    let route = rig.password("tunnel");
    let SshError::HostKeyUnknown { host, port, algorithm, fingerprint, trust } = rig.refusal(route.clone()).await
    else {
        panic!("not unknown")
    };
    assert_eq!((host.as_str(), port, algorithm.as_str()), ("127.0.0.1", rig.bastion.port, "ssh-ed25519"));
    assert_eq!(fingerprint, public(HOST_KEY).fingerprint(HashAlg::Sha256).to_string());
    assert_eq!(rig.bastion.auths(), 0, "nothing authenticates to an unknown server");

    rig.tunnels.trust(trust).unwrap();
    assert!(rig.tunnels.trust(trust).is_err(), "a held key is trusted once");
    let trusted = std::fs::read_to_string(rig.app_known_hosts()).unwrap();
    assert_eq!(
        trusted.lines().filter(|l| !l.is_empty()).collect::<Vec<_>>(),
        [rig.bastion.known_hosts_line().trim_end()]
    );
    assert_eq!(std::fs::read_to_string(rig.user_known_hosts()).unwrap(), user, "the user's file is never written");
    rig.echo(&route).await;
}

#[tokio::test]
async fn only_the_newest_thirty_two_unknown_keys_wait_for_trust() {
    let dir = tempfile::tempdir().unwrap();
    let hosts = HostKeys::new(None, dir.path().join("known_hosts"));
    let ids: Vec<HostKeyId> = (0..33)
        .map(|i| match hosts.check(&format!("host{i}"), 22, &public(HOST_KEY)) {
            Err(SshError::HostKeyUnknown { trust, .. }) => trust,
            other => panic!("{other:?}"),
        })
        .collect();
    assert!(hosts.trust(ids[0]).is_err());
    hosts.trust(ids[32]).unwrap();
    assert!(hosts.check("host32", 22, &public(HOST_KEY)).is_ok());
    assert!(hosts.trust(HostKeyId(999)).is_err(), "a key the engine never saw cannot be trusted");
}

#[tokio::test]
async fn a_host_listed_with_several_keys_is_trusted_when_one_matches() {
    let rig = Rig::trusted(Login::Password).await;
    let other = public("keys/id_ed25519").to_openssh().unwrap();
    std::fs::write(rig.user_known_hosts(), format!("[127.0.0.1]:{} {other}\n", rig.bastion.port)).unwrap();
    rig.echo(&rig.password("tunnel")).await;
}

#[tokio::test]
async fn a_changed_host_key_fails_at_its_line() {
    let rig = Rig::new(Login::Password).await;
    let other = public("keys/id_ed25519").to_openssh().unwrap();
    std::fs::write(rig.user_known_hosts(), format!("# comment\n\n[127.0.0.1]:{} {other}\n", rig.bastion.port)).unwrap();
    let error = rig.refusal(rig.password("tunnel")).await;
    let SshError::HostKeyChanged { known_at, fingerprint, .. } = &error else { panic!("{error:?}") };
    assert_eq!(known_at, &format!("{}:3", rig.user_known_hosts().display()));
    assert_eq!(fingerprint, &public(HOST_KEY).fingerprint(HashAlg::Sha256).to_string());
    let text = crate::connection::connect_error_text(&ConnectError::Ssh { error });
    assert!(text.contains(&format!("ssh-keygen -R '[127.0.0.1]:{}'", rig.bastion.port)), "{text}");
}

#[tokio::test]
async fn a_revoked_host_key_fails_even_when_trusted() {
    let rig = Rig::trusted(Login::Password).await;
    std::fs::write(
        rig.user_known_hosts(),
        format!("@revoked * ssh-ed25519 {}\n", public(HOST_KEY).public_key_base64()),
    )
    .unwrap();
    let error = rig.refusal(rig.password("tunnel")).await;
    let SshError::HostKeyRevoked { revoked_at, .. } = error else { panic!("{error:?}") };
    assert_eq!(revoked_at, format!("{}:1", rig.user_known_hosts().display()));
}

#[cfg(unix)]
#[tokio::test]
async fn an_unreadable_known_hosts_file_fails_closed() {
    let rig = Rig::trusted(Login::Password).await;
    std::fs::write(rig.user_known_hosts(), rig.bastion.known_hosts_line()).unwrap();
    std::fs::set_permissions(rig.user_known_hosts(), std::os::unix::fs::PermissionsExt::from_mode(0o000)).unwrap();
    if std::fs::read(rig.user_known_hosts()).is_ok() {
        return;
    }
    assert!(matches!(rig.refusal(rig.password("tunnel")).await, SshError::KnownHosts { .. }));
}

#[tokio::test]
async fn password_auth_takes_the_right_password_and_names_the_methods_for_a_wrong_one() {
    let rig = Rig::trusted(Login::Password).await;
    rig.echo(&rig.password("tunnel")).await;
    let SshError::Auth { message } = rig.refusal(rig.password("wrong")).await else { panic!() };
    assert!(
        message.contains("tunnel")
            && message.contains(&format!("127.0.0.1:{}", rig.bastion.port))
            && message.contains("publickey"),
        "{message}"
    );
    let missing = rig.tunnels.route(Some(OWNER), rig.spec(SshAuth::Password, None));
    assert!(matches!(missing.unwrap_err(), ConnectError::Ssh { error: SshError::MissingSecret }));
    assert_eq!(rig.bastion.connections(), 2, "a missing password dials nothing");
}

#[tokio::test]
async fn a_server_that_asks_only_keyboard_interactive_gets_the_password() {
    let rig = Rig::trusted(Login::KeyboardInteractive).await;
    rig.echo(&rig.password("tunnel")).await;
    assert!(matches!(rig.refusal(rig.password("wrong")).await, SshError::Auth { .. }));
}

#[tokio::test]
async fn key_auth_takes_ed25519_rsa_and_a_passphrase() {
    let rig = Rig::trusted(Login::Password).await;
    let route = |auth, secret: Option<&str>| rig.tunnels.route(None, rig.spec(auth, secret)).unwrap();
    rig.echo(&route(rig.key("keys/id_ed25519"), None)).await;
    rig.echo(&route(rig.key("keys/id_rsa"), None)).await;
    rig.echo(&route(rig.key("keys/id_ed25519_passphrase"), Some("l1data"))).await;
    assert_eq!(rig.refusal(route(rig.key("keys/id_ed25519_passphrase"), None)).await, SshError::MissingSecret);
    assert!(
        matches!(rig.refusal(route(rig.key("keys/id_ed25519_passphrase"), Some("wrong"))).await, SshError::Key { message } if message.contains("passphrase"))
    );
    assert!(
        matches!(rig.refusal(route(rig.key(HOST_KEY), None)).await, SshError::Auth { .. }),
        "a key the server does not authorize"
    );
    assert!(
        matches!(rig.refusal(route(SshAuth::Key { path: "/nonexistent/id".into() }, None)).await, SshError::Key { message } if message.contains("/nonexistent/id"))
    );
}

#[cfg(unix)]
#[tokio::test]
async fn agent_auth_signs_with_a_key_the_agent_holds() {
    use russh::keys::agent::client::AgentClient;
    let rig = Rig::trusted(Login::Password).await;
    let agent = |name: &str| {
        let socket = rig.dir.path().join(name);
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let incoming = async_stream::stream! {
            loop { yield listener.accept().await.map(|(s, _)| s); }
        };
        tokio::spawn(russh::keys::agent::server::serve(Box::pin(incoming), ()));
        SshAuth::Agent { socket: Some(socket.display().to_string()) }
    };
    let (loaded, empty) = (agent("loaded.sock"), agent("empty.sock"));
    let SshAuth::Agent { socket: Some(path) } = &loaded else { unreachable!() };
    AgentClient::connect_uds(path).await.unwrap().add_identity(&private("keys/id_ed25519"), &[]).await.unwrap();

    let route = |auth| rig.tunnels.route(None, rig.spec(auth, None)).unwrap();
    rig.echo(&route(loaded)).await;
    let SshError::Agent { message } = rig.refusal(route(empty)).await else { panic!() };
    assert!(message.contains("no keys"), "{message}");
    let gone = SshAuth::Agent { socket: Some(rig.dir.path().join("gone.sock").display().to_string()) };
    assert!(matches!(rig.refusal(route(gone)).await, SshError::Agent { .. }));
}

#[tokio::test]
async fn concurrent_opens_share_one_connection_and_one_auth() {
    let rig = Rig::trusted(Login::Password).await;
    let echo = rig.bastion.echo;
    let opens = (0..20).map(|_| {
        let route = rig.password("tunnel");
        async move { route.open("127.0.0.1", echo).await }
    });
    let mut streams: Vec<TunnelStream> =
        futures_util::future::join_all(opens).await.into_iter().map(Result::unwrap).collect();
    for stream in &mut streams {
        assert_round_trip(stream).await;
    }
    assert_eq!((rig.bastion.connections(), rig.bastion.auths()), (1, 1));
}

#[tokio::test]
async fn a_failing_bastion_is_dialed_once_per_wave_and_never_cached() {
    let rig = Rig::trusted(Login::Refuse).await;
    let route = rig.password("tunnel");
    let opens = (0..20).map(|_| route.open("127.0.0.1", rig.bastion.echo));
    for result in futures_util::future::join_all(opens).await {
        assert!(matches!(ssh_error(result), SshError::Auth { .. }));
    }
    assert_eq!(rig.bastion.connections(), 1);
    assert!(matches!(rig.refusal(route).await, SshError::Auth { .. }));
    assert_eq!(rig.bastion.connections(), 2, "a later open dials again");
}

#[tokio::test]
async fn a_dropped_connection_ends_its_streams_and_the_next_open_dials_again() {
    let rig = Rig::trusted(Login::Password).await;
    let route = rig.password("tunnel");
    let mut stream = rig.echo(&route).await;
    rig.bastion.drop_connections().await;
    let mut rest = Vec::new();
    let read =
        timeout(Duration::from_secs(5), stream.read_to_end(&mut rest)).await.expect("the stream ends within 5 s");
    assert!(read.is_err() || rest.is_empty(), "{read:?}");
    rig.echo(&route).await;
    assert_eq!(rig.bastion.connections(), 2);
}

#[tokio::test]
async fn the_connection_closes_with_its_last_stream() {
    let rig = Rig::trusted(Login::Password).await;
    let route = rig.password("tunnel");
    let (first, second) = (rig.echo(&route).await, rig.echo(&route).await);
    drop(first);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(Counters::get(&rig.bastion.counters.live), 1, "another stream still rides the connection");
    drop(second);
    eventually("the bastion saw the connection close", || Counters::get(&rig.bastion.counters.live) == 0).await;
    rig.echo(&route).await;
    assert_eq!(rig.bastion.connections(), 2);
}

#[tokio::test]
async fn an_edited_spec_dials_its_own_connection_while_the_old_stream_keeps_working() {
    let rig = Rig::trusted(Login::Password).await;
    let mut old = rig.echo(&rig.password("tunnel")).await;
    let edited = rig.tunnels.route(Some(OWNER), rig.spec(rig.key("keys/id_ed25519"), None)).unwrap();
    rig.echo(&edited).await;
    assert_eq!(rig.bastion.connections(), 2);
    assert_round_trip(&mut old).await;
}

#[tokio::test]
async fn forget_makes_the_next_session_dial_and_spares_open_streams() {
    let rig = Rig::trusted(Login::Password).await;
    let mut open = rig.echo(&rig.password("tunnel")).await;
    rig.tunnels.forget(OWNER);
    rig.echo(&rig.password("tunnel")).await;
    assert_eq!(rig.bastion.connections(), 2);
    assert_round_trip(&mut open).await;
}

#[tokio::test]
async fn a_refused_destination_is_a_forward_error() {
    let rig = Rig::trusted(Login::Password).await;
    let SshError::Forward { message } = ssh_error(rig.password("tunnel").open("10.255.255.1", 5433).await) else {
        panic!()
    };
    assert!(message.contains("10.255.255.1:5433"), "{message}");
}

#[tokio::test]
async fn test_routes_never_share_a_connection() {
    let rig = Rig::trusted(Login::Password).await;
    let _shared = rig.echo(&rig.password("tunnel")).await;
    for _ in 0..2 {
        let private = rig.tunnels.route(None, rig.spec(SshAuth::Password, Some("tunnel"))).unwrap();
        rig.echo(&private).await;
    }
    assert_eq!((rig.bastion.connections(), rig.bastion.auths()), (3, 3));
}

#[tokio::test]
async fn unread_data_on_one_stream_does_not_stall_another() {
    let rig = Rig::trusted(Login::Password).await;
    let route = rig.password("tunnel");
    let _parked = route.open("flood", 1).await.unwrap();
    eventually("the bastion sent the flood", || Counters::get(&rig.bastion.counters.flooded) == 1).await;
    let mut other = timeout(Duration::from_secs(5), route.open("127.0.0.1", rig.bastion.echo))
        .await
        .expect("a second channel opens")
        .unwrap();
    assert_round_trip(&mut other).await;
}
