mod sqlserver_url;

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use crate::entra::AzCli;
use crate::model::*;
use crate::session::Session;
use crate::tunnel::SshRoute;
use crate::view::{self, ConnectionView, Known};

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
pub const KEEPALIVE_IDLE: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct ConnectParams {
    pub database_kind: DatabaseKind,
    pub host: String,
    pub port: u16,
    pub database: String,
    pub user: String,
    pub password: Option<String>,
    pub ssl_mode: SslMode,
    pub ca_cert_path: Option<String>,
    pub via: Option<SshRoute>,
    pub entra: Option<Arc<AzCli>>,
}

pub fn uses_entra(kind: DatabaseKind, auth: Option<SqlServerAuth>) -> bool {
    kind == DatabaseKind::SqlServer && auth == Some(SqlServerAuth::EntraAzureCli)
}

impl ConnectParams {
    /// The one place that chooses which database a session targets.
    pub fn for_database(
        c: &ConnectionConfig,
        database: &str,
        password: Option<String>,
        via: Option<SshRoute>,
        entra: Option<Arc<AzCli>>,
    ) -> Self {
        Self {
            database_kind: c.database_kind,
            host: c.host.clone(),
            port: c.port,
            database: database.to_owned(),
            user: c.user.clone(),
            password,
            ssl_mode: c.ssl_mode,
            ca_cert_path: c.ca_cert_path.clone(),
            via,
            entra,
        }
    }

    pub fn from_draft(
        d: &ConnectionDraft,
        password: Option<String>,
        via: Option<SshRoute>,
        entra: Option<Arc<AzCli>>,
    ) -> Self {
        Self {
            database_kind: d.database_kind,
            host: d.host.clone(),
            port: d.port,
            database: d.database.clone(),
            user: d.user.clone(),
            password,
            ssl_mode: d.ssl_mode,
            ca_cert_path: d.ca_cert_path.clone(),
            via,
            entra,
        }
    }
}

pub fn draft_from_url(url: &str, name: &str) -> Result<ConnectionDraft, String> {
    if url
        .trim()
        .split_once(':')
        .is_some_and(|(scheme, _)| scheme.eq_ignore_ascii_case("mssql") || scheme.eq_ignore_ascii_case("sqlserver"))
    {
        return sqlserver_url::parse(url.trim(), name);
    }
    crate::session::parse_postgres_url(url, name)
}

pub fn connect_error_text(e: &ConnectError) -> String {
    match e {
        ConnectError::Network { message }
        | ConnectError::Tls { message }
        | ConnectError::Auth { message }
        | ConnectError::Keychain { message } => message.clone(),
        ConnectError::MissingPassword => "password missing".into(),
        ConnectError::Server(q) => q.message.clone(),
        ConnectError::Ssh { error } => ssh_error_text(error),
    }
}

/// How OpenSSH names a server in known_hosts and in `ssh-keygen -R`.
pub fn known_hosts_name(host: &str, port: u16) -> String {
    if port == 22 { host.to_owned() } else { format!("[{host}]:{port}") }
}

fn ssh_error_text(e: &SshError) -> String {
    match e {
        SshError::Network { message }
        | SshError::KnownHosts { message }
        | SshError::Auth { message }
        | SshError::Key { message }
        | SshError::Agent { message }
        | SshError::Forward { message } => {
            format!("SSH: {message}")
        }
        SshError::HostKeyUnknown { host, port, algorithm, fingerprint, .. } => {
            format!("SSH: {host}:{port} offered an unknown {algorithm} host key {fingerprint}")
        }
        SshError::HostKeyChanged { host, port, algorithm, fingerprint, known_at } => {
            let (file, _) = known_at.rsplit_once(':').unwrap_or((known_at, ""));
            format!(
                "SSH: the {algorithm} host key of {host}:{port} is now {fingerprint}, but {known_at} holds another. If the server's key was replaced, run `ssh-keygen -R '{}' -f '{file}'`",
                known_hosts_name(host, *port)
            )
        }
        SshError::HostKeyRevoked { host, port, algorithm, fingerprint, revoked_at } => {
            format!("SSH: the {algorithm} host key {fingerprint} of {host}:{port} is revoked at {revoked_at}")
        }
        SshError::MissingSecret => "SSH: no password or key passphrase is saved for the tunnel".into(),
    }
}

pub struct DatabaseInfo {
    pub name: String,
    pub system: bool,
    pub access: DbAccess,
}

/// One connected instance. `slots[home]` lives as long as the instance and its session is the one
/// whose death marks the connection `Lost`. Every other slot is opened for a database the tree
/// shows or a console targets, and its session's death fails only that database.
///
/// Lock order, outermost first: `Shared::connections`, `slots`, `Shared::consoles`,
/// `Console::latest`, `databases`, `view`, `probes`, `show_checks`. A thread that holds one of
/// these takes only locks later in the list. `state()` walks the whole list under
/// `connections`, so a path that takes `probes` or `view` first and then `slots` deadlocks
/// against a snapshot.
pub struct Instance {
    pub config: ConnectionConfig,
    pub server: ServerInfo,
    pub home: String,
    pub databases: RwLock<Arc<Vec<DatabaseInfo>>>,
    /// The engine's copy of the stored view, written only by `Engine::edit_view` and the deferred
    /// show check in `load_catalog`.
    pub view: RwLock<ConnectionView>,
    pub slots: Mutex<HashMap<String, Arc<DbSlot>>>,
    /// Schema counts of databases without a slot, from `probe_databases`. Cleared by
    /// `refresh_connection`; dies with the instance.
    pub probes: Mutex<HashMap<String, SchemaCount>>,
    pub probe_gate: tokio::sync::Semaphore,
    /// Databases an edit showed while their catalog was unknown. After each one's first load the
    /// engine checks that its stored selection still resolves to a schema.
    pub show_checks: Mutex<BTreeSet<String>>,
    /// Set once the instance is disconnected or lost. A task that still holds the instance may
    /// finish its query, but it opens no session and converges no slot after this.
    closed: AtomicBool,
}

pub const PROBE_PERMITS: usize = 4;

/// The slots `converge` keeps and the ones it creates. `open` is a subset of `keep`: a shown
/// database past `view::AUTO_OPEN_LIMIT` is kept but gets its slot only when something asks for
/// its catalog.
pub struct Wanted {
    pub keep: BTreeSet<String>,
    pub open: BTreeSet<String>,
}

/// One metadata session and catalog for one database.
pub struct DbSlot {
    pub db: DatabaseRef,
    /// Opened on first use; `None` again after a loss until the next load reopens it. Loads and
    /// counts run under this lock, so concurrent loads of one database are single-flight.
    pub meta: tokio::sync::Mutex<Option<Session>>,
    pub catalog: RwLock<CatalogSlot>,
    pub refresh: RefreshGate,
}

pub enum CatalogSlot {
    Idle,
    /// The first load is in flight.
    Opening,
    Ready(Arc<Catalog>),
    /// Keeps the last good catalog so a failed refresh does not blank the tree.
    Failed {
        message: String,
        last: Option<Arc<Catalog>>,
    },
}

impl DbSlot {
    pub fn new(db: DatabaseRef) -> Self {
        Self {
            db,
            meta: tokio::sync::Mutex::new(None),
            catalog: RwLock::new(CatalogSlot::Idle),
            refresh: RefreshGate::default(),
        }
    }

    pub fn ready(&self) -> Option<Arc<Catalog>> {
        match &*self.catalog.read().unwrap() {
            CatalogSlot::Ready(c) => Some(c.clone()),
            _ => None,
        }
    }

    pub fn last(&self) -> Option<Arc<Catalog>> {
        match &*self.catalog.read().unwrap() {
            CatalogSlot::Ready(c) | CatalogSlot::Failed { last: Some(c), .. } => Some(c.clone()),
            _ => None,
        }
    }

    pub fn status(&self) -> CatalogStatus {
        match &*self.catalog.read().unwrap() {
            CatalogSlot::Idle => CatalogStatus::Idle,
            CatalogSlot::Opening => CatalogStatus::Loading,
            CatalogSlot::Ready(c) => CatalogStatus::Ready { version: c.version },
            CatalogSlot::Failed { message, last } => {
                CatalogStatus::Failed { message: message.clone(), version: last.as_ref().map(|c| c.version) }
            }
        }
    }

    pub fn set(&self, state: CatalogSlot) {
        *self.catalog.write().unwrap() = state;
    }

    pub fn fail(&self, message: String) {
        let last = self.last();
        self.set(CatalogSlot::Failed { message, last });
    }
}

/// Coalesces scheduled refreshes: while a load runs, further requests only mark it dirty, and the
/// running loop loads once more when it finishes. A burst of DDL costs at most two loads.
#[derive(Default)]
pub struct RefreshGate {
    state: Mutex<(bool, bool)>,
}

impl RefreshGate {
    /// True when the caller must run the load loop; false when a running loop will cover it.
    pub fn request(&self) -> bool {
        let mut g = self.state.lock().unwrap();
        let (running, dirty) = &mut *g;
        if *running {
            *dirty = true;
            false
        } else {
            *running = true;
            true
        }
    }

    /// After a load: true when another load is owed.
    pub fn again(&self) -> bool {
        let mut g = self.state.lock().unwrap();
        let (running, dirty) = &mut *g;
        if *dirty {
            *dirty = false;
            true
        } else {
            *running = false;
            false
        }
    }
}

impl Instance {
    pub fn new(
        config: ConnectionConfig,
        server: ServerInfo,
        home: String,
        databases: Vec<DatabaseInfo>,
        view: ConnectionView,
        home_slot: DbSlot,
    ) -> Self {
        Self {
            config,
            server,
            home: home.clone(),
            databases: RwLock::new(Arc::new(databases)),
            view: RwLock::new(view),
            slots: Mutex::new(HashMap::from([(home, Arc::new(home_slot))])),
            probes: Mutex::new(HashMap::new()),
            probe_gate: tokio::sync::Semaphore::new(PROBE_PERMITS),
            show_checks: Mutex::new(BTreeSet::new()),
            closed: AtomicBool::new(false),
        }
    }

    pub fn view(&self) -> ConnectionView {
        self.view.read().unwrap().clone()
    }

    /// The served catalog of every slot that has one: Ready, or the last good one behind Failed.
    pub fn catalogs(&self) -> HashMap<String, Arc<Catalog>> {
        served_catalogs(&self.slots.lock().unwrap())
    }

    pub fn slot_names(&self) -> Vec<String> {
        self.slots.lock().unwrap().keys().cloned().collect()
    }

    /// The databases the view shows, resolved against the current `pg_database` list.
    pub fn shown(&self) -> BTreeSet<String> {
        let databases = self.databases.read().unwrap().clone();
        let known = Known { home: &self.home, databases: &databases, catalogs: &HashMap::new() };
        view::resolve_databases(&self.view.read().unwrap(), &known)
    }

    pub fn slot(&self, database: &str) -> Option<Arc<DbSlot>> {
        self.slots.lock().unwrap().get(database).cloned()
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// Drops every slot, which closes each metadata session as soon as its last holder lets go,
    /// and stops the instance from opening another.
    pub fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.slots.lock().unwrap().clear();
    }

    pub fn knows(&self, database: &str) -> bool {
        self.databases.read().unwrap().iter().any(|d| d.name == database)
    }

    pub fn canonical_database<'a>(&'a self, requested: &'a str) -> &'a str {
        if requested == self.config.database { &self.home } else { requested }
    }

    /// Converges the slots to `wanted`: drops the ones outside `keep`, which closes their
    /// sessions, creates the missing ones in `open`, and returns every slot whose catalog has not
    /// been loaded yet. Running it twice changes nothing. `wanted` runs under the slot lock, so
    /// a view written before this call is what it reads, and no converge can apply a set
    /// computed from an older view after one computed from a newer one.
    pub fn converge(&self, id: ConnectionId, wanted: impl FnOnce() -> Wanted) -> Vec<Arc<DbSlot>> {
        let mut slots = self.slots.lock().unwrap();
        if self.is_closed() {
            return Vec::new();
        }
        let wanted = wanted();
        slots.retain(|name, _| wanted.keep.contains(name));
        for name in &wanted.open {
            if !slots.contains_key(name) && self.knows(name) {
                slots.insert(
                    name.clone(),
                    Arc::new(DbSlot::new(DatabaseRef { connection_id: id, database: name.clone() })),
                );
            }
        }
        slots.values().filter(|s| matches!(*s.catalog.read().unwrap(), CatalogSlot::Idle)).cloned().collect()
    }

    /// The tree's view of the instance, resolved from the database list, the stored view, each
    /// slot's served catalog, and the probe cache.
    pub fn state(&self) -> InstanceState {
        let slots = self.slots.lock().unwrap();
        let catalogs = served_catalogs(&slots);
        let list = self.databases.read().unwrap().clone();
        let view = self.view.read().unwrap();
        let probes = self.probes.lock().unwrap();
        let known = Known { home: &self.home, databases: &list, catalogs: &catalogs };
        let shown = view::resolve_databases(&view, &known);
        let databases = list
            .iter()
            .map(|d| {
                let entry = view.database(&d.name, &self.home);
                let catalog = catalogs.get(&d.name);
                DatabaseState {
                    name: d.name.clone(),
                    system: d.system,
                    access: d.access,
                    shown: shown.contains(&d.name),
                    show_system_schemas: entry.show_system_schemas,
                    shown_schemas: catalog.map(|c| view::resolve_schemas(&entry, c)).unwrap_or_default(),
                    schema_count: match catalog {
                        Some(c) => SchemaCount::Known { count: c.schemas.iter().filter(|s| !s.system).count() as u32 },
                        None => probes.get(&d.name).cloned().unwrap_or(SchemaCount::Unknown),
                    },
                    catalog: slots.get(&d.name).map_or(CatalogStatus::Idle, |s| s.status()),
                }
            })
            .collect();
        InstanceState {
            configured_database: self.config.database.clone(),
            home: self.home.clone(),
            show_system_databases: view.show_system_databases,
            databases,
        }
    }
}

fn served_catalogs(slots: &HashMap<String, Arc<DbSlot>>) -> HashMap<String, Arc<Catalog>> {
    slots.iter().filter_map(|(name, slot)| slot.last().map(|c| (name.clone(), c))).collect()
}

/// The outcome every caller of one connect attempt waits for, `None` until the attempt ends.
pub type ConnectAttempt = tokio::sync::watch::Receiver<Option<Result<ServerInfo, ConnectError>>>;

pub enum ConnSlot {
    Disconnected,
    Connecting(ConnectAttempt),
    Connected(Arc<Instance>),
    Failed(ConnectError),
    Lost(String),
}

pub struct ConnEntry {
    pub rev: u64,
    pub slot: ConnSlot,
}

impl ConnEntry {
    pub fn state(&self, id: ConnectionId) -> ConnectionState {
        let status = match &self.slot {
            ConnSlot::Disconnected => ConnStatus::Disconnected,
            ConnSlot::Connecting(_) => ConnStatus::Connecting,
            ConnSlot::Connected(instance) => {
                ConnStatus::Connected { server: instance.server.clone(), instance: instance.state() }
            }
            ConnSlot::Failed(error) => ConnStatus::Failed { error: error.clone() },
            ConnSlot::Lost(reason) => ConnStatus::Lost { reason: reason.clone() },
        };
        ConnectionState { id, rev: self.rev, status }
    }

    pub fn instance(&self) -> Option<Arc<Instance>> {
        match &self.slot {
            ConnSlot::Connected(instance) => Some(instance.clone()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_sensitive_database_names_keep_distinct_slots() {
        let id = ConnectionId(uuid::Uuid::nil());
        let config = ConnectionConfig {
            database_kind: DatabaseKind::SqlServer,
            id,
            name: "case sensitive".into(),
            host: "localhost".into(),
            port: 1433,
            database: "Shop".into(),
            user: "sa".into(),
            ssl_mode: SslMode::Require,
            ca_cert_path: None,
            has_password: false,
            color: None,
            ssh: None,
            environment: None,
            sql_server_auth: None,
        };
        let databases =
            ["Shop", "shop"].map(|name| DatabaseInfo { name: name.into(), system: false, access: DbAccess::Ok });
        let home = DbSlot::new(DatabaseRef { connection_id: id, database: "Shop".into() });
        let instance = Instance::new(
            config,
            ServerInfo { version: String::new(), version_num: 0, encoding: "Unicode".into() },
            "Shop".into(),
            databases.into(),
            ConnectionView::initial(),
            home,
        );
        instance.converge(id, || Wanted {
            keep: BTreeSet::from(["Shop".into(), "shop".into()]),
            open: BTreeSet::from(["Shop".into(), "shop".into()]),
        });
        for name in ["Shop", "shop"] {
            assert_eq!(instance.canonical_database(name), name);
            assert_eq!(instance.slot(name).unwrap().db.database, name);
        }
        assert_eq!(instance.canonical_database("SHOP"), "SHOP");
        assert!(!instance.knows("SHOP"));
        assert!(instance.slot("SHOP").is_none());
    }

    #[test]
    fn a_failed_reload_keeps_the_last_catalog_and_its_version() {
        let db = DatabaseRef { connection_id: ConnectionId(uuid::Uuid::nil()), database: "shop".into() };
        let slot = DbSlot::new(db.clone());
        slot.fail("first load failed".into());
        assert_eq!(slot.status(), CatalogStatus::Failed { message: "first load failed".into(), version: None });
        assert!(slot.last().is_none());

        let catalog = Arc::new(Catalog {
            database_kind: crate::model::DatabaseKind::Postgres,
            db,
            version: 7,
            default_schema: None,
            schemas: Vec::new(),
            extensions: Vec::new(),
        });
        slot.set(CatalogSlot::Ready(catalog.clone()));
        slot.fail("statement timeout".into());
        assert_eq!(slot.status(), CatalogStatus::Failed { message: "statement timeout".into(), version: Some(7) });
        assert!(slot.ready().is_none());
        assert_eq!(slot.last().map(|c| c.version), Some(7));
    }

    #[test]
    fn url_becomes_a_draft() {
        let d = draft_from_url("postgres://l1data:s%40cret@db.example:5433/shop?sslmode=require", "dev").unwrap();
        assert_eq!(
            (d.host.as_str(), d.port, d.database.as_str(), d.user.as_str()),
            ("db.example", 5433, "shop", "l1data")
        );
        assert!(matches!(d.password, PasswordInput::Set(ref p) if p == "s@cret"));
        assert_eq!(d.ssl_mode, SslMode::Require);
        let bare = draft_from_url("postgres://localhost/shop", "x").unwrap();
        assert_eq!((bare.port, bare.ssl_mode), (5432, SslMode::Prefer));
        assert!(matches!(bare.password, PasswordInput::Clear));
        assert!(draft_from_url("not a url", "x").is_err());
        assert_eq!(
            draft_from_url("postgres://db.example/shop?sslmode=verify-full", "").unwrap().ssl_mode,
            SslMode::VerifyFull
        );
        assert_eq!(
            draft_from_url("host=db.example dbname=shop sslmode=verify-ca", "").unwrap().ssl_mode,
            SslMode::VerifyCa
        );
        assert_eq!(
            draft_from_url("postgres://:::", "").unwrap_err(),
            "invalid connection string: invalid value for option `port`"
        );
    }
}
