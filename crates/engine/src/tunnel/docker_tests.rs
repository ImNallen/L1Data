//! Against the `dev/sshd` bastion (`pnpm db:ssh:up`), which reaches `postgres:5432` and
//! `mssql:1433` by their compose names, names only the bastion resolves.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::broadcast;

use crate::assistant::tools::{QUERY_TIMEOUT, run_query};
use crate::model::*;
use crate::{AgentPath, Engine, EngineConfig, MemoryKeyStore};

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../dev/sshd");
/// `ssh-keygen -lf dev/sshd/ssh_host_ed25519_key.pub`
const HOST_FINGERPRINT: &str = "SHA256:IwTtFKPPOY6WYVaFlYcDMLe6uLWpgNlLDk98MyvA8jE";

struct Rig {
    engine: Arc<Engine>,
    events: broadcast::Receiver<AppEvent>,
    dir: tempfile::TempDir,
}

impl Rig {
    /// `known` trusts the bastion through the committed `dev/sshd/known_hosts`, as a user's own
    /// OpenSSH file would.
    async fn start(known: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let user_known_hosts = known.then(|| Path::new(FIXTURE).join("known_hosts"));
        let engine = Engine::open(EngineConfig { data_dir: dir.path().join("data"), keys: Arc::new(MemoryKeyStore::default()), agent_path: AgentPath::Fixed("".into()), az_binary: None, user_known_hosts, updates: Arc::new(crate::ScriptedTransport::named("off").unwrap()) })
            .await
            .unwrap();
        Self { events: engine.subscribe(), engine, dir }
    }

    /// A copy of a fixture key with the mode a user's key has.
    fn key(&self, name: &str) -> SshAuth {
        let path = self.dir.path().join(name);
        std::fs::copy(Path::new(FIXTURE).join("keys").join(name), &path).unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600)).unwrap();
        SshAuth::Key { path: path.display().to_string() }
    }

    async fn tab(&self, connection_id: ConnectionId) -> TabId {
        let tab = TabId(uuid::Uuid::new_v4());
        self.engine.console_configure(tab, ConsoleConfig { connection_id, database: "shop".into(), schema: None, mode: TxnMode::AutoCommit }).await.unwrap();
        tab
    }

    async fn start_run(&self, tab: TabId, sql: &str) -> RunId {
        self.engine.console_run(tab, RunRequest::Script { text: sql.into(), target: RunTarget::All, mode: RunMode::Run, params: Vec::new(), cells: Vec::new() }, None).await.unwrap().run_id
    }

    async fn finished(&mut self, tab: TabId, run: RunId) -> ConsoleState {
        loop {
            let event = tokio::time::timeout(Duration::from_secs(30), self.events.recv()).await.expect("an event within 30 s").unwrap();
            if let AppEvent::ConsoleChanged(state) = event
                && state.tab_id == tab
                && matches!(state.run, RunState::Finished { run_id, .. } if run_id == run)
            {
                return state;
            }
        }
    }

    async fn scalar(&mut self, tab: TabId, sql: &str) -> String {
        let run = self.start_run(tab, sql).await;
        let state = self.finished(tab, run).await;
        let RunState::Finished { statements, outcome: RunOutcome::Ok, .. } = &state.run else { panic!("{sql}: {:?}", state.run) };
        let Some(RunOutput::Rows { result, .. }) = statements[0].outputs.first() else { panic!("{sql} returned no rows") };
        self.engine.result_page(result.r#ref, 0, 1).unwrap().rows[0][0].clone().unwrap()
    }
}

fn tunnel(auth: SshAuth, secret: Option<&str>) -> SshTunnelDraft {
    SshTunnelDraft { host: "localhost".into(), port: 2222, user: "tunnel".into(), auth, secret: secret.map_or(PasswordInput::Clear, |s| PasswordInput::Set(s.into())) }
}

fn postgres(ssh: SshTunnelDraft) -> ConnectionDraft {
    ConnectionDraft {
        database_kind: DatabaseKind::Postgres,
        id: None,
        name: "through the bastion".into(),
        host: "postgres".into(),
        port: 5432,
        database: "shop".into(),
        user: "l1data".into(),
        ssl_mode: SslMode::Prefer,
        ca_cert_path: None,
        password: PasswordInput::Set("l1data".into()),
        color: None,
        ssh: Some(ssh),
        environment: None,
        sql_server_auth: None,
    }
}

fn ssh_error(result: Result<ServerInfo, ConnectError>) -> SshError {
    match result {
        Err(ConnectError::Ssh { error }) => error,
        other => panic!("not an SSH error: {other:?}"),
    }
}

#[tokio::test]
async fn first_use_asks_to_trust_the_bastion_and_then_connects() {
    let rig = Rig::start(false).await;
    let config = rig.engine.save_connection(postgres(tunnel(SshAuth::Password, Some("tunnel")))).await.unwrap();
    let SshError::HostKeyUnknown { host, port, algorithm, fingerprint, trust } = ssh_error(rig.engine.connect(config.id).await) else { panic!() };
    assert_eq!((host.as_str(), port, algorithm.as_str(), fingerprint.as_str()), ("localhost", 2222, "ssh-ed25519", HOST_FINGERPRINT));
    rig.engine.ssh_trust_host_key(trust).unwrap();
    assert!(rig.engine.connect(config.id).await.unwrap().version.starts_with("16"));
    let trusted = std::fs::read_to_string(rig.engine.data_dir().join("known_hosts")).unwrap();
    assert!(trusted.contains("[localhost]:2222 ssh-ed25519 "), "{trusted}");
}

#[tokio::test]
async fn a_tunneled_connection_loads_its_catalog_runs_and_cancels() {
    let mut rig = Rig::start(true).await;
    let config = rig.engine.save_connection(postgres(tunnel(SshAuth::Password, Some("tunnel")))).await.unwrap();
    rig.engine.connect(config.id).await.unwrap();
    let catalog = rig.engine.catalog(DatabaseRef { connection_id: config.id, database: "shop".into() }).await.unwrap();
    assert!(catalog.schemas.iter().any(|s| s.name == "public" && s.relations.iter().any(|r| r.name == "orders")), "the catalog has public.orders");

    let tab = rig.tab(config.id).await;
    assert_eq!(rig.scalar(tab, "select count(*) from orders where id > 0").await.parse::<u64>().map(|n| n > 0), Ok(true));
    let run = rig.start_run(tab, "select pg_sleep(30)").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let started = Instant::now();
    rig.engine.console_cancel(tab).await;
    let state = rig.finished(tab, run).await;
    assert!(started.elapsed() < Duration::from_secs(2), "the cancel went through the tunnel, took {:?}", started.elapsed());
    assert!(matches!(state.run, RunState::Finished { outcome: RunOutcome::Cancelled, .. }), "{:?}", state.run);
    assert_eq!(rig.scalar(tab, "select 7").await, "7", "the session is usable after the cancel");

    rig.engine.disconnect(config.id, true).await.unwrap();
    rig.engine.connect(config.id).await.unwrap();
    let tab = rig.tab(config.id).await;
    assert_eq!(rig.scalar(tab, "select 8").await, "8", "a reconnect dials a fresh tunnel");
}

#[tokio::test]
async fn the_model_queries_through_the_tunnel_on_a_read_only_session() {
    let rig = Rig::start(true).await;
    let config = rig.engine.save_connection(postgres(tunnel(SshAuth::Password, Some("tunnel")))).await.unwrap();
    let params = rig.engine.shared.params(&config, "shop").unwrap();
    assert!(params.via.is_some());
    let rows = run_query(&rig.engine.shared, &params, "select current_database(), current_setting('transaction_read_only')", QUERY_TIMEOUT).await.unwrap();
    assert_eq!(rows.rows, [[Some("shop".to_owned()), Some("on".to_owned())]]);
    let refused = run_query(&rig.engine.shared, &params, "create table assistant_wrote_this (id int)", QUERY_TIMEOUT).await.unwrap_err();
    assert!(refused.contains("read-only"), "{refused}");
}

#[tokio::test]
async fn each_auth_method_reaches_postgres() {
    let rig = Rig::start(true).await;
    let test = |ssh| rig.engine.test_connection(postgres(ssh));
    assert!(test(tunnel(SshAuth::Password, Some("tunnel"))).await.unwrap().version.starts_with("16"));
    test(tunnel(rig.key("id_ed25519"), None)).await.unwrap();
    test(tunnel(rig.key("id_rsa"), None)).await.unwrap();
    test(tunnel(rig.key("id_ed25519_passphrase"), Some("l1data"))).await.unwrap();
    #[cfg(unix)]
    test(tunnel(agent_holding(&rig, "id_ed25519").await, None)).await.unwrap();

    assert!(matches!(ssh_error(test(tunnel(SshAuth::Password, Some("wrong"))).await), SshError::Auth { message } if message.contains("tunnel")));
    assert_eq!(ssh_error(test(tunnel(rig.key("id_ed25519_passphrase"), None)).await), SshError::MissingSecret);
    assert_eq!(ssh_error(test(tunnel(SshAuth::Password, None)).await), SshError::MissingSecret);
}

#[cfg(unix)]
async fn agent_holding(rig: &Rig, key: &str) -> SshAuth {
    let socket = rig.dir.path().join("agent.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let incoming = async_stream::stream! {
        loop { yield listener.accept().await.map(|(s, _)| s); }
    };
    tokio::spawn(russh::keys::agent::server::serve(Box::pin(incoming), ()));
    let private = russh::keys::load_secret_key(Path::new(FIXTURE).join("keys").join(key), None).unwrap();
    russh::keys::agent::client::AgentClient::connect_uds(&socket).await.unwrap().add_identity(&private, &[]).await.unwrap();
    SshAuth::Agent { socket: Some(socket.display().to_string()) }
}

#[tokio::test]
async fn a_destination_the_bastion_does_not_permit_is_a_forward_error() {
    let rig = Rig::start(true).await;
    let draft = ConnectionDraft { port: 5433, ..postgres(tunnel(SshAuth::Password, Some("tunnel"))) };
    let SshError::Forward { message } = ssh_error(rig.engine.test_connection(draft).await) else { panic!() };
    assert!(message.contains("postgres:5433"), "{message}");
}

/// Needs the SQL Server container from `pnpm db:up` as well. It runs when the `mssql-tests` feature is on or SQL Server
/// answers on its published port, and says so when it skips.
#[tokio::test]
async fn sql_server_speaks_tds_through_the_tunnel() {
    let host = std::env::var("L1_DATA_TEST_MSSQL_HOST").unwrap_or_else(|_| "localhost".into());
    let port = std::env::var("L1_DATA_TEST_MSSQL_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(1434u16);
    if !cfg!(feature = "mssql-tests") && tokio::net::TcpStream::connect((host.as_str(), port)).await.is_err() {
        eprintln!("skipped: SQL Server is not up at {host}:{port}");
        return;
    }
    let mut rig = Rig::start(true).await;
    let draft = ConnectionDraft {
        database_kind: DatabaseKind::SqlServer,
        host: "mssql".into(),
        port: 1433,
        user: "sa".into(),
        password: PasswordInput::Set(std::env::var("L1_DATA_TEST_MSSQL_PASSWORD").unwrap_or_else(|_| "L1Data_dev_Str0ng".into())),
        ssl_mode: SslMode::Require,
        ..postgres(tunnel(rig.key("id_ed25519"), None))
    };
    rig.engine.test_connection(draft.clone()).await.unwrap();
    let config = rig.engine.save_connection(draft).await.unwrap();
    rig.engine.connect(config.id).await.unwrap();
    let tab = rig.tab(config.id).await;
    assert_eq!(rig.scalar(tab, "SELECT DB_NAME()").await, "shop");
}
