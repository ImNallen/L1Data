mod access;
mod aggregate;
mod assistant;
mod catalog;
mod connection;
mod console;
mod database_map;
mod entra;
mod explain;
mod export;
mod files;
mod history;
mod import;
pub mod model;
mod notebook_file;
mod params;
mod plan;
mod profile;
mod provider;
mod results;
mod saved;
mod script;
mod secrets;
mod session;
mod shell_path;
mod showplan;
mod sql;
mod store;
mod text;
mod tunnel;
mod txn;
mod update;
mod view;
mod writes;

use std::collections::{BTreeSet, HashMap};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock, Weak};
use std::time::Duration;

use tokio::sync::broadcast;

use connection::{CatalogSlot, ConnEntry, ConnSlot, ConnectParams, DbSlot, Instance, Wanted};
use console::{Console, Owed, PlannedStmt, RunPlan, SessionSlot, Work};
use model::*;
use provider::ProbeDepth;
use secrets::{SecretSlot, Vault};
use session::Session;
use store::{HistoryInsert, Store};
use tunnel::TunnelSpec;
use view::{ConnectionView, Known, Selection};

pub use assistant::SHUTDOWN_BOUND;
pub use connection::draft_from_url;
pub use secrets::{KeyStore, KeyringKeyStore, MemoryKeyStore, SecretError};
pub use shell_path::AgentPath;
pub use store::StoreError;
pub use tunnel::openssh_known_hosts;
pub use update::{Installed, Progress, Release, ScriptedTransport, UpdateTransport, Verified};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub struct EngineConfig {
    pub data_dir: PathBuf,
    pub keys: Arc<dyn KeyStore>,
    pub agent_path: AgentPath,
    /// The program Entra ID sign-in runs in place of `az` on the PATH.
    pub az_binary: Option<PathBuf>,
    /// The OpenSSH known_hosts that SSH tunnels also trust, read at each dial and never written.
    /// The app passes `openssh_known_hosts()`. Tests pass `None`, so they never read the
    /// developer's file.
    pub user_known_hosts: Option<PathBuf>,
    pub updates: Arc<dyn UpdateTransport>,
}

pub struct Engine {
    shared: Arc<Shared>,
}

pub(crate) struct Shared {
    data_dir: PathBuf,
    vault: Vault,
    store: Store,
    settings: RwLock<Settings>,
    access: access::Access,
    connections: RwLock<HashMap<ConnectionId, ConnEntry>>,
    consoles: RwLock<HashMap<TabId, Arc<Console>>>,
    events: broadcast::Sender<AppEvent>,
    next_run: AtomicU64,
    /// Engine-wide, so a reconnect never reissues a catalog version the UI has already seen.
    next_catalog_version: AtomicU64,
    /// Serializes every view edit, so two concurrent `edit_view` calls cannot interleave apply
    /// and persist.
    view_edits: tokio::sync::Mutex<()>,
    timezone: String,
    /// The runtime `Engine::open` ran on. Every task the engine starts goes through it, so a
    /// sync entry point works from any thread: Tauri runs sync commands inline on the main
    /// thread, where a bare `tokio::spawn` panics.
    runtime: tokio::runtime::Handle,
    providers: provider::Providers,
    assistant: assistant::Assistant,
    process_groups: Arc<provider::agent::ProcessGroups>,
    next_chat_rev: AtomicU64,
    tunnels: tunnel::Tunnels,
    updates: update::Updates,
    az: Arc<entra::AzCli>,
}

/// A saved SSH secret applies only to the auth kind it was saved for.
fn keeps_ssh_secret(prior: Option<&SshTunnel>, auth: &SshAuth) -> bool {
    prior.is_some_and(|p| p.has_secret && std::mem::discriminant(&p.auth) == std::mem::discriminant(auth))
}

const EVENT_BUFFER: usize = 1024;
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
const DB_FILE: &str = "l1-data.sqlite3";
const KV_WORKSPACE: &str = "workspace";
const KV_SETTINGS: &str = "settings";
const BOOT_CHATS: u32 = 200;
const NEWER_SIGN_IN: &str = "This connection signs in with a method from a newer version of L1 Data.";

impl Shared {
    pub(crate) fn emit(&self, ev: AppEvent) {
        let _ = self.events.send(ev);
    }

    pub(crate) fn spawn<F>(&self, task: F) -> tokio::task::JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.runtime.spawn(task)
    }

    /// Spawns a task that holds `console`'s session as `run_id`. Every such task starts here,
    /// because a panic drops the session without returning it and nothing else would free the tab.
    pub(crate) fn spawn_holding<F>(self: &Arc<Self>, console: Arc<Console>, run_id: RunId, task: F)
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let task = self.spawn(task);
        let shared = self.clone();
        self.spawn(async move {
            let Err(e) = task.await else { return };
            let Ok(panic) = e.try_into_panic() else { return };
            let text = panic
                .downcast_ref::<&str>()
                .map(|s| (*s).to_owned())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_default();
            console
                .fail_task(
                    &shared,
                    run_id,
                    format!("The task failed inside L1 Data and its session was closed: {text}"),
                )
                .await;
        });
    }

    /// Runs `task` through `spawn_holding` and waits for its output. A caller that stops waiting
    /// leaves the task running, so the session still goes back to the tab. `None` means the task
    /// panicked, which `spawn_holding` has already reported.
    pub(crate) async fn run_holding<T: Send + 'static>(
        self: &Arc<Self>,
        console: Arc<Console>,
        run_id: RunId,
        task: impl Future<Output = T> + Send + 'static,
    ) -> Option<T> {
        let (reply, result) = tokio::sync::oneshot::channel();
        self.spawn_holding(console, run_id, async move {
            let _ = reply.send(task.await);
        });
        result.await.ok()
    }

    pub(crate) fn settings(&self) -> Settings {
        self.settings.read().unwrap().clone()
    }

    fn next_run_id(&self) -> RunId {
        RunId(self.next_run.fetch_add(1, Ordering::Relaxed))
    }

    pub(crate) fn record_history(&self, h: HistoryInsert) {
        match self.store.insert_history_and_prune(h) {
            Ok(entry) => self.emit(AppEvent::HistoryAppended(entry)),
            Err(e) => eprintln!("history insert failed: {e}"),
        }
    }

    fn connection_state(&self, id: ConnectionId) -> Option<ConnectionState> {
        self.connections.read().unwrap().get(&id).map(|e| e.state(id))
    }

    fn set_connection(&self, id: ConnectionId, slot: ConnSlot) {
        let mut map = self.connections.write().unwrap();
        let entry = map.entry(id).or_insert(ConnEntry { rev: 0, slot: ConnSlot::Disconnected });
        entry.rev += 1;
        entry.slot = slot;
        let state = entry.state(id);
        drop(map);
        self.emit(AppEvent::ConnectionChanged(state));
    }

    fn instance(&self, id: ConnectionId) -> Option<Arc<Instance>> {
        self.connections.read().unwrap().get(&id).and_then(ConnEntry::instance)
    }

    fn config(&self, id: ConnectionId) -> Option<ConnectionConfig> {
        self.store.connection(id).ok()?
    }

    /// A notebook's database resolves as its cells' do, so they still match.
    fn resolve_workspace(&self, workspace: &mut Workspace) {
        let tabs =
            workspace.tabs.iter_mut().filter_map(|t| Some((t.console.connection_id?, t.console.database.as_mut()?)));
        let notebooks =
            workspace.notebooks.iter_mut().filter_map(|n| n.db.as_mut().map(|db| (db.connection_id, &mut db.database)));
        for (id, database) in tabs.chain(notebooks) {
            if let Some(instance) = self.instance(id) {
                *database = instance.canonical_database(database).to_owned();
            }
        }
    }

    fn params(&self, config: &ConnectionConfig, database: &str) -> Result<ConnectParams, ConnectError> {
        let entra = self.entra(config.database_kind, config.sql_server_auth)?;
        let password = if config.has_password { self.saved_secret(config.id, SecretSlot::Database)? } else { None };
        let via = match &config.ssh {
            Some(t) => {
                let secret = if t.has_secret { self.saved_secret(config.id, SecretSlot::Ssh)? } else { None };
                Some(self.tunnels.route(Some(config.id), TunnelSpec::new(&t.host, t.port, &t.user, &t.auth, secret))?)
            }
            None => None,
        };
        Ok(ConnectParams::for_database(config, database, password, via, entra))
    }

    fn entra(
        &self,
        kind: DatabaseKind,
        auth: Option<SqlServerAuth>,
    ) -> Result<Option<Arc<entra::AzCli>>, ConnectError> {
        match (kind, auth) {
            (DatabaseKind::SqlServer, Some(SqlServerAuth::Unknown)) => Err(ConnectError::Auth {
                message: format!("{NEWER_SIGN_IN} Update L1 Data, or edit the connection and choose a sign-in method."),
            }),
            _ => Ok(connection::uses_entra(kind, auth).then(|| self.az.clone())),
        }
    }

    async fn open_tab_session(&self, console: &ConsoleConfigView) -> Result<console::IdleSession, ConnectError> {
        let saved = self
            .config(console.connection_id)
            .ok_or_else(|| ConnectError::Network { message: "unknown connection".into() })?;
        let instance = self.instance(saved.id);
        let database = instance
            .as_ref()
            .map_or(console.database.as_str(), |instance| instance.canonical_database(&console.database));
        let params = self.params(&saved, database)?;
        let session = Box::pin(Session::open(&params, console.schema.as_deref(), &self.timezone)).await?;
        Ok(console::IdleSession::new(session, saved))
    }

    fn next_catalog_version(&self) -> u64 {
        self.next_catalog_version.fetch_add(1, Ordering::Relaxed)
    }

    pub(crate) fn next_chat_rev(&self) -> u64 {
        self.next_chat_rev.fetch_add(1, Ordering::Relaxed)
    }

    fn saved_secret(&self, id: ConnectionId, slot: SecretSlot) -> Result<Option<String>, ConnectError> {
        let keychain = |message: String| ConnectError::Keychain { message };
        let Some(sealed) = self.store.secret(id, slot).map_err(|e| keychain(e.to_string()))? else { return Ok(None) };
        self.vault.open(id, slot, &sealed).map_err(|e| keychain(e.to_string()))
    }

    fn save_ssh(&self, id: ConnectionId, prior: Option<&SshTunnel>, t: SshTunnelDraft) -> Result<SshTunnel, SaveError> {
        let store = |e: StoreError| SaveError::Store { message: e.to_string() };
        let auth = match t.auth {
            SshAuth::Key { path } => SshAuth::Key { path: path.trim().to_owned() },
            SshAuth::Agent { socket } => {
                SshAuth::Agent { socket: socket.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()) }
            }
            SshAuth::Password => SshAuth::Password,
        };
        let has_secret = match (&auth, t.secret) {
            (SshAuth::Agent { .. }, _) | (_, PasswordInput::Clear) => false,
            (_, PasswordInput::Keep) => keeps_ssh_secret(prior, &auth),
            (_, PasswordInput::Set(secret)) => {
                let sealed = self
                    .vault
                    .seal(id, SecretSlot::Ssh, &secret)
                    .map_err(|e| SaveError::Keychain { message: e.to_string() })?;
                self.store.set_secret(id, SecretSlot::Ssh, &sealed).map_err(store)?;
                true
            }
        };
        if !has_secret {
            self.store.delete_secret(id, SecretSlot::Ssh).map_err(store)?;
        }
        Ok(SshTunnel { host: t.host.trim().to_owned(), port: t.port, user: t.user.trim().to_owned(), auth, has_secret })
    }

    fn all_consoles(&self) -> Vec<Arc<Console>> {
        self.consoles.read().unwrap().values().cloned().collect()
    }

    /// Databases of `id` that may have a slot (`keep`): the home database, the shown ones, the
    /// database of every console on the connection, and the database of every running assistant
    /// turn. The ones that get a slot without being asked (`open`) are the same minus the shown
    /// databases past the auto-open cap.
    fn wanted(&self, instance: &Instance) -> Wanted {
        let mut keep = BTreeSet::from([instance.home.clone()]);
        for console in self.all_consoles() {
            let config = console.latest.lock().unwrap().config.clone();
            if config.connection_id == instance.config.id {
                keep.insert(config.database);
            }
        }
        keep.extend(self.assistant.running_databases(instance.config.id));
        let shown = instance.shown();
        let mut open = keep.clone();
        open.extend(view::auto_open(&shown));
        keep.extend(shown);
        Wanted { keep, open }
    }

    /// Converges the current instance's slots to `wanted` and starts a load for each slot not
    /// loaded yet.
    fn reconcile(self: &Arc<Self>, id: ConnectionId) {
        if let Some(instance) = self.instance(id) {
            self.reconcile_instance(&instance);
        }
    }

    fn reconcile_instance(self: &Arc<Self>, instance: &Arc<Instance>) {
        self.converge(instance, || self.wanted(instance));
    }

    fn converge(self: &Arc<Self>, instance: &Arc<Instance>, wanted: impl FnOnce() -> Wanted) {
        let id = instance.config.id;
        let opened = instance.converge(id, wanted);
        self.republish_connection(id);
        for slot in opened {
            let shared = self.clone();
            let instance = instance.clone();
            self.spawn(async move {
                let _ = shared.ensure_catalog(&instance, &slot).await;
            });
        }
    }

    /// The slot of a wanted database, created on first use. The instance it converges is the one
    /// fetched here, so a reconnect in between cannot leave it without the slot; the slot is
    /// missing afterwards only when the instance closed or the database left `pg_database`.
    fn slot(self: &Arc<Self>, db: &DatabaseRef) -> Result<(Arc<Instance>, Arc<DbSlot>), CatalogError> {
        let instance = self.instance(db.connection_id).ok_or(CatalogError::NotConnected)?;
        let db = DatabaseRef {
            connection_id: db.connection_id,
            database: instance.canonical_database(&db.database).to_owned(),
        };
        if !instance.knows(&db.database) {
            return Err(CatalogError::UnknownDatabase { database: db.database.clone() });
        }
        if instance.slot(&db.database).is_none() {
            if !self.wanted(&instance).keep.contains(&db.database) {
                return Err(CatalogError::NotShown { database: db.database.clone() });
            }
            self.converge(&instance, || {
                let mut wanted = self.wanted(&instance);
                wanted.open.insert(db.database.clone());
                wanted
            });
        }
        let slot = instance.slot(&db.database).ok_or_else(|| {
            if instance.knows(&db.database) {
                CatalogError::NotConnected
            } else {
                CatalogError::UnknownDatabase { database: db.database.clone() }
            }
        })?;
        Ok((instance, slot))
    }

    /// The Ready catalog, or one load of it. Loads of one database are single-flight: callers
    /// that arrive during a load wait for it and get its result. A load that fails after an
    /// earlier one succeeded serves the last good catalog; the failure shows on the connection
    /// snapshot, and the next call tries again.
    async fn ensure_catalog(
        self: &Arc<Self>,
        instance: &Arc<Instance>,
        slot: &Arc<DbSlot>,
    ) -> Result<Arc<Catalog>, CatalogError> {
        if let Some(catalog) = slot.ready() {
            return Ok(catalog);
        }
        let mut meta = slot.meta.lock().await;
        if let Some(catalog) = slot.ready() {
            return Ok(catalog);
        }
        match self.load_catalog(instance, slot, &mut meta).await {
            Ok(catalog) => Ok(catalog),
            Err(e) => slot.last().ok_or(e),
        }
    }

    /// One load under the slot's session lock. Opens the session when the slot has none.
    async fn load_catalog(
        self: &Arc<Self>,
        instance: &Arc<Instance>,
        slot: &DbSlot,
        meta: &mut Option<Session>,
    ) -> Result<Arc<Catalog>, CatalogError> {
        let id = instance.config.id;
        if slot.last().is_none() {
            slot.set(CatalogSlot::Opening);
            self.republish_connection(id);
        }
        let session = match self.session_in(instance, slot, meta).await {
            Ok(session) => session,
            Err(e) => {
                let message = connection::connect_error_text(&e);
                slot.fail(message.clone());
                self.republish_connection(id);
                return Err(CatalogError::Failed { message });
            }
        };
        let version = self.next_catalog_version();
        let extra = view::system_schemas_to_load(
            &instance.view.read().unwrap().database(&slot.db.database, &instance.home),
            instance.config.database_kind,
        );
        match session.catalog(slot.db.clone(), version, &extra).await {
            Ok(catalog) => {
                let catalog = Arc::new(catalog);
                slot.set(CatalogSlot::Ready(catalog.clone()));
                let owed_check = instance.show_checks.lock().unwrap().remove(&slot.db.database);
                if owed_check {
                    self.check_shown_schemas(instance, &slot.db.database, &catalog).await;
                }
                self.republish_connection(id);
                Ok(catalog)
            }
            Err(e) => {
                let message = match session.closed_reason() {
                    Some(reason) => {
                        *meta = None;
                        reason
                    }
                    None => e.message,
                };
                slot.fail(message.clone());
                if slot.db.database == instance.home && meta.is_none() {
                    self.mark_lost(id, instance, message.clone());
                } else {
                    self.republish_connection(id);
                }
                Err(CatalogError::Failed { message })
            }
        }
    }

    /// The deferred half of `ViewEdit::Database { shown: true }`: a database shown while its
    /// catalog was unknown, whose stored `Only` selection resolves to no schema once the catalog
    /// is known, falls back to its default schema.
    async fn check_shown_schemas(self: &Arc<Self>, instance: &Arc<Instance>, database: &str, catalog: &Catalog) {
        let _edits = self.view_edits.lock().await;
        if instance.is_closed() {
            return;
        }
        let view = instance.view();
        let entry = view.database(database, &instance.home);
        if !matches!(entry.schemas, Selection::Only { .. }) || !view::resolve_schemas(&entry, catalog).is_empty() {
            return;
        }
        let known = Known { home: &instance.home, databases: &[], catalogs: &HashMap::new() };
        let next =
            view.apply(&ViewEdit::Schemas { database: database.to_owned(), preset: Preset::DefaultOnly }, &known);
        if let Err(e) = self.store.set_view(instance.config.id, &next) {
            eprintln!("connection view save failed: {e}");
        }
        *instance.view.write().unwrap() = next;
    }

    /// Re-reads `pg_database` on the home session into the instance and drops the probe cache,
    /// since a database may have been dropped and recreated under the same name.
    async fn reread_databases(self: &Arc<Self>, instance: &Arc<Instance>) -> Result<(), CatalogError> {
        let failed = |message: String| CatalogError::Failed { message };
        let home = instance.slot(&instance.home).ok_or(CatalogError::NotConnected)?;
        let mut meta = home.meta.lock().await;
        let session = self
            .session_in(instance, &home, &mut meta)
            .await
            .map_err(|e| failed(connection::connect_error_text(&e)))?;
        let (databases, _) = session.list_databases().await.map_err(|e| failed(e.message))?;
        instance.probes.lock().unwrap().clear();
        *instance.databases.write().unwrap() = Arc::new(databases);
        Ok(())
    }

    /// The slot's open session, reopened if it died or was never opened.
    async fn session_in<'m>(
        self: &Arc<Self>,
        instance: &Arc<Instance>,
        slot: &DbSlot,
        meta: &'m mut Option<Session>,
    ) -> Result<&'m mut Session, ConnectError> {
        if meta.as_ref().is_some_and(|s| s.closed_reason().is_some()) {
            *meta = None;
        }
        if meta.is_none() {
            if instance.is_closed() {
                return Err(ConnectError::Network { message: "disconnected".into() });
            }
            let params = self.params(&instance.config, &slot.db.database)?;
            let session = Session::open_metadata(&params, &self.timezone).await?;
            self.watch_slot_session(instance, &slot.db, session.backend_pid, session.closed_watch());
            *meta = Some(session);
        }
        Ok(meta.as_mut().expect("opened above"))
    }

    fn republish_connection(&self, id: ConnectionId) {
        let mut map = self.connections.write().unwrap();
        let Some(entry) = map.get_mut(&id) else { return };
        entry.rev += 1;
        let state = entry.state(id);
        drop(map);
        self.emit(AppEvent::ConnectionChanged(state));
    }

    fn mark_lost(&self, id: ConnectionId, instance: &Arc<Instance>, reason: String) {
        let still_current = self.instance(id).is_some_and(|i| Arc::ptr_eq(&i, instance));
        if still_current {
            self.set_connection(id, ConnSlot::Lost(reason));
            instance.close();
        }
    }

    /// The home session's death loses the instance. Another slot's death fails only its database,
    /// and the next load on it reopens the session. The task holds the instance weakly: the
    /// session it watches lives inside the instance, so a strong handle would keep that session
    /// open past a disconnect.
    fn watch_slot_session(
        self: &Arc<Self>,
        instance: &Arc<Instance>,
        db: &DatabaseRef,
        backend_pid: i32,
        mut closed: tokio::sync::watch::Receiver<Option<String>>,
    ) {
        let shared = self.clone();
        let instance: Weak<Instance> = Arc::downgrade(instance);
        let db = db.clone();
        self.spawn(async move {
            while closed.changed().await.is_ok() {
                let Some(reason) = closed.borrow().clone() else { continue };
                let Some(instance) = instance.upgrade() else { return };
                if db.database == instance.home {
                    shared.mark_lost(db.connection_id, &instance, reason);
                } else if let Some(slot) = instance.slot(&db.database) {
                    let mut meta = slot.meta.lock().await;
                    if meta.as_ref().is_some_and(|s| s.backend_pid == backend_pid) {
                        *meta = None;
                        slot.fail(reason);
                        shared.republish_connection(db.connection_id);
                    }
                }
                return;
            }
        });
    }

    pub(crate) fn schedule_refreshes(self: &Arc<Self>, db: DatabaseRef, owed: Owed) {
        if owed.catalog {
            match self.instance(db.connection_id) {
                Some(instance) if instance.config.database_kind == DatabaseKind::SqlServer => {
                    let tracked_slots: Vec<_> = instance.slots.lock().unwrap().values().cloned().collect();
                    for slot in tracked_slots {
                        self.schedule_slot_catalog_refresh(instance.clone(), slot);
                    }
                }
                _ => self.schedule_catalog_refresh(db.clone()),
            }
        }
        if owed.databases {
            self.schedule_database_list_refresh(db.connection_id);
        }
    }

    fn schedule_catalog_refresh(self: &Arc<Self>, db: DatabaseRef) {
        let Ok((instance, slot)) = self.slot(&db) else { return };
        self.schedule_slot_catalog_refresh(instance, slot);
    }

    fn schedule_slot_catalog_refresh(self: &Arc<Self>, instance: Arc<Instance>, slot: Arc<DbSlot>) {
        if !slot.refresh.request() {
            return;
        }
        let shared = self.clone();
        self.spawn(async move {
            loop {
                let mut meta = slot.meta.lock().await;
                let _ = shared.load_catalog(&instance, &slot, &mut meta).await;
                drop(meta);
                if !slot.refresh.again() {
                    return;
                }
            }
        });
    }

    fn schedule_database_list_refresh(self: &Arc<Self>, id: ConnectionId) {
        let shared = self.clone();
        self.spawn(async move {
            let Some(instance) = shared.instance(id) else { return };
            match shared.reread_databases(&instance).await {
                Ok(()) => shared.reconcile_instance(&instance),
                Err(e) => eprintln!("database list refresh failed: {e:?}"),
            }
        });
    }

    /// The catalog of one database, loaded on first use. Only the home database, the shown ones,
    /// the databases of this connection's consoles, and those of running assistant turns get one.
    async fn catalog(self: &Arc<Self>, db: &DatabaseRef) -> Result<Arc<Catalog>, CatalogError> {
        let (instance, slot) = self.slot(db)?;
        self.ensure_catalog(&instance, &slot).await
    }

    async fn relation_detail(self: &Arc<Self>, db: &DatabaseRef, oid: u32) -> Result<RelationDetail, DetailError> {
        let (instance, slot) = self.slot(db).map_err(|error| DetailError::Catalog { error })?;
        // A `Failed` slot still serves its last catalog; reloading here would retry on every expand.
        let version = match slot.last() {
            Some(catalog) => catalog.version,
            None => {
                self.ensure_catalog(&instance, &slot).await.map_err(|error| DetailError::Catalog { error })?.version
            }
        };
        let mut meta = slot.meta.lock().await;
        let session = self
            .session_in(&instance, &slot, &mut meta)
            .await
            .map_err(|e| DetailError::Failed { message: connection::connect_error_text(&e) })?;
        session
            .detail(slot.db.clone(), oid, version)
            .await
            .map_err(|e| DetailError::Failed { message: e.message })?
            .ok_or(DetailError::Gone)
    }

    async fn object_script(
        self: &Arc<Self>,
        db: &DatabaseRef,
        target: &ScriptTarget,
    ) -> Result<ObjectScript, DetailError> {
        let (instance, slot) = self.slot(db).map_err(|error| DetailError::Catalog { error })?;
        let mut meta = slot.meta.lock().await;
        let session = self
            .session_in(&instance, &slot, &mut meta)
            .await
            .map_err(|e| DetailError::Failed { message: connection::connect_error_text(&e) })?;
        session.script(target).await
    }

    /// Joins the connection's current attempt or starts one. The check and the claim happen under
    /// one write lock, so concurrent callers share a single attempt.
    async fn connect(self: &Arc<Self>, id: ConnectionId) -> Result<ServerInfo, ConnectError> {
        let (mut attempt, started) = {
            let mut map = self.connections.write().unwrap();
            let entry = map.entry(id).or_insert(ConnEntry { rev: 0, slot: ConnSlot::Disconnected });
            match &entry.slot {
                ConnSlot::Connected(instance) => return Ok(instance.server.clone()),
                // A closed channel means the attempt's task panicked and will never answer.
                ConnSlot::Connecting(attempt) if attempt.has_changed().is_ok() => (attempt.clone(), None),
                _ => {
                    let (done, attempt) = tokio::sync::watch::channel(None);
                    entry.rev += 1;
                    entry.slot = ConnSlot::Connecting(attempt.clone());
                    (attempt, Some((done, entry.state(id))))
                }
            }
        };
        if let Some((done, state)) = started {
            self.emit(AppEvent::ConnectionChanged(state));
            let shared = self.clone();
            self.spawn(async move {
                let _ = done.send(Some(shared.open_connection(id).await));
            });
        }
        let outcome = attempt.wait_for(Option::is_some).await.ok().and_then(|done| done.clone());
        outcome.unwrap_or_else(|| {
            Err(ConnectError::Network { message: "The connection attempt stopped inside L1 Data.".into() })
        })
    }

    /// One connect attempt. It runs in its own task, so a caller that stops waiting cannot abandon it.
    async fn open_connection(self: &Arc<Self>, id: ConnectionId) -> Result<ServerInfo, ConnectError> {
        match self.open_instance(id).await {
            Ok((instance, home_pid, home_closed)) => {
                let server = instance.server.clone();
                self.set_connection(id, ConnSlot::Connected(instance.clone()));
                for console in self.all_consoles() {
                    let mut inner = console.inner.lock().await;
                    if inner.config.connection_id == id {
                        let database = instance.canonical_database(&inner.config.database);
                        if database != inner.config.database {
                            inner.config.database = database.to_owned();
                            console.publish(&mut inner, self);
                        }
                    }
                }
                let home = DatabaseRef { connection_id: id, database: instance.home.clone() };
                self.watch_slot_session(&instance, &home, home_pid, home_closed);
                self.reconcile(id);
                Ok(server)
            }
            Err(e) => {
                self.set_connection(id, ConnSlot::Failed(e.clone()));
                Err(e)
            }
        }
    }

    /// Opens the home session and reads `pg_database`. Catalogs load afterwards, each on its own.
    async fn open_instance(
        &self,
        id: ConnectionId,
    ) -> Result<(Arc<Instance>, i32, tokio::sync::watch::Receiver<Option<String>>), ConnectError> {
        let config = self.config(id).ok_or_else(|| ConnectError::Network { message: "unknown connection".into() })?;
        let params = self.params(&config, &config.database)?;
        let mut session = Session::open_metadata(&params, &self.timezone).await?;
        let (databases, home) = session.list_databases().await.map_err(ConnectError::Server)?;
        let (server, pid, closed) = (session.server.clone(), session.backend_pid, session.closed_watch());
        let slot = DbSlot::new(DatabaseRef { connection_id: id, database: home.clone() });
        *slot.meta.lock().await = Some(session);
        // An edit on the previous instance may still be persisting; its row is the one to read.
        let view = {
            let _edits = self.view_edits.lock().await;
            self.stored_view(id)
        };
        Ok((Arc::new(Instance::new(config, server, home, databases, view, slot)), pid, closed))
    }

    fn stored_view(&self, id: ConnectionId) -> ConnectionView {
        match self.store.view(id) {
            Ok(view) => view.unwrap_or_else(ConnectionView::initial),
            Err(e) => {
                eprintln!("connection view load failed: {e}");
                ConnectionView::initial()
            }
        }
    }

    /// Consoles of `id` with an open transaction, with `has_writes` probed on their idle sessions.
    /// The probe takes the session out like a run does, so the lock is not held across the query.
    /// The probes run together, each for at most `PROBE_TIMEOUT`, so a stalled network delays the
    /// check once rather than forever. A probe with no answer counts as writes.
    async fn open_transactions(self: &Arc<Self>, consoles: &[Arc<Console>]) -> Vec<OpenTxn> {
        let probes = consoles.iter().map(|console| async move {
            let run_id = self.next_run_id();
            let taken = {
                let mut inner = console.inner.lock().await;
                if inner.txn == TxnState::Idle {
                    return None;
                }
                let taken = console.take_session(&mut inner, run_id, Work::Probe);
                if taken.is_some() {
                    console.publish(&mut inner, self);
                }
                taken
            };
            let has_writes = match taken {
                None => true,
                Some((mut session, _stop)) => {
                    let has_writes = tokio::time::timeout(PROBE_TIMEOUT, session.has_writes()).await.unwrap_or(true);
                    let lost = session.closed_reason();
                    let edited = console
                        .edit(self, run_id, |inner| {
                            let txn = inner.txn;
                            Console::return_session(inner, session, txn, lost)
                        })
                        .await;
                    if let Ok(Some((db, owed))) = edited {
                        self.schedule_refreshes(db, owed);
                    }
                    has_writes
                }
            };
            Some(OpenTxn { tab_id: console.tab, has_writes })
        });
        futures_util::future::join_all(probes).await.into_iter().flatten().collect()
    }

    async fn console_txn_command(
        self: &Arc<Self>,
        tab: TabId,
        commit: bool,
        confirmed: Option<WriteDigest>,
    ) -> Result<(), TxnError> {
        let console = self.console(tab).ok_or(TxnError::NoTransaction)?;
        let (mut session, commits, run_id) = {
            let mut inner = console.inner.lock().await;
            let idle = match &inner.session {
                SessionSlot::Busy { .. } => return Err(TxnError::Busy),
                SessionSlot::Empty | SessionSlot::Lost { .. } => return Err(TxnError::NoTransaction),
                SessionSlot::Idle(_) if inner.txn == TxnState::Idle => return Err(TxnError::NoTransaction),
                SessionSlot::Idle(idle) => idle,
            };
            let work = if commit && inner.txn != TxnState::Aborted {
                let saved = self.config(inner.config.connection_id).ok_or(TxnError::NoTransaction)?;
                let target = writes::Target {
                    saved: &saved,
                    opened: idle.opened(),
                    console: &inner.config,
                    txn: inner.txn,
                    unasked: &inner.unasked,
                };
                Work::Commit(
                    writes::check(&target, &["COMMIT"], confirmed.as_ref())
                        .map_err(|confirmation| TxnError::ConfirmWrites { confirmation })?,
                )
            } else {
                Work::Rollback
            };
            let commits = matches!(work, Work::Commit(_));
            let run_id = self.next_run_id();
            let (session, _stop) = console.take_session(&mut inner, run_id, work).expect("slot is idle");
            console.publish(&mut inner, self);
            (session, commits, run_id)
        };
        let shared = self.clone();
        let task_console = console.clone();
        let finished = self.run_holding(console.clone(), run_id, async move {
            let this = &shared;
            let console = &task_console;
            let (kind, sql) =
                if commits { (sql::StmtKind::Commit, "COMMIT") } else { (sql::StmtKind::Rollback, "ROLLBACK") };
            let started = std::time::Instant::now();
            let mut result = session.finish_transaction(commits).await;
            let elapsed_ms = started.elapsed().as_millis().min(u32::MAX as u128) as u32;
            let fatal = result.as_ref().err().filter(|e| e.severity == "FATAL").map(|e| e.message.clone());
            let lost = session.closed_reason_settled(fatal).await;
            if commits
                && lost.is_some()
                && let Err(error) = &mut result
            {
                error.message = format!("Commit outcome is unknown because the session was lost: {}", error.message);
            }
            let mut lines = vec![console::statement_line(0, sql)];
            lines.extend(console::notice_lines(&mut session));
            lines.push(match &result {
                Ok(()) => OutputBody::Command { statement: 0, tag: sql.into(), elapsed_ms },
                Err(error) => OutputBody::Error { statement: Some(0), error: error.clone() },
            });
            console.output_all(this, run_id, lines);
            let outcome = if result.is_ok() { txn::StmtOutcome::Ok } else { txn::StmtOutcome::Err };
            let edited = console
                .edit(this, run_id, |inner| {
                    let txn = session.observed_txn().unwrap_or_else(|| txn::txn_after(inner.txn, &kind, outcome));
                    if txn::ended(&kind, outcome, session.observed_txn()) {
                        inner.unasked = WriteList::default();
                    }
                    let retired = Console::return_session(inner, session, txn, lost);
                    retired.or_else(|| inner.finish_refreshes(commits && result.is_ok()))
                })
                .await;
            if let Ok(Some((db, owed))) = edited {
                this.schedule_refreshes(db, owed);
            }
            match result {
                Ok(()) if commit && !commits => Err(TxnError::RolledBack),
                Ok(()) => Ok(()),
                Err(error) => Err(TxnError::Query(error)),
            }
        });
        finished.await.unwrap_or_else(|| {
            let what = if commits { "Commit outcome is unknown: the commit" } else { "The rollback" };
            Err(TxnError::Query(session::error(format!("{what} stopped inside L1 Data and its session was closed."))))
        })
    }

    fn console(&self, tab: TabId) -> Option<Arc<Console>> {
        self.consoles.read().unwrap().get(&tab).cloned()
    }

    fn result_set(&self, r: ResultRef) -> Option<Arc<results::ResultSet>> {
        self.console(r.tab_id)?.result_set(r)
    }
}

impl Engine {
    pub async fn open(cfg: EngineConfig) -> Result<Arc<Self>, StoreError> {
        tokio::fs::create_dir_all(&cfg.data_dir).await?;
        let store = Store::open(&cfg.data_dir.join(DB_FILE))?;
        // An update can install an older build, which must still start when a newer build saved
        // settings it cannot read. The row stays as saved until the user changes a setting.
        let settings = match store.kv_get::<Settings>(KV_SETTINGS) {
            Err(StoreError::Json(e)) => {
                eprintln!("settings load failed, so this launch uses the defaults: {e}");
                None
            }
            loaded => loaded?,
        }
        .unwrap_or_default();
        let access = store
            .kv_get::<access::AccessLease>("assistant_access")
            .ok()
            .flatten()
            .filter(|a| a.mode == settings.assistant_access)
            .unwrap_or_else(|| access::AccessLease::new(settings.assistant_access));
        store.kv_set("assistant_access", &access)?;
        let (events, _) = broadcast::channel(EVENT_BUFFER);
        let tunnels = tunnel::Tunnels::new(cfg.user_known_hosts, cfg.data_dir.join("known_hosts"));
        let updates = update::Updates::open(&store, cfg.updates);
        let shared = Arc::new(Shared {
            data_dir: cfg.data_dir,
            vault: Vault::new(cfg.keys),
            store,
            settings: RwLock::new(settings),
            access: access::Access::new(access),
            connections: RwLock::new(HashMap::new()),
            consoles: RwLock::new(HashMap::new()),
            events,
            next_run: AtomicU64::new(1),
            next_catalog_version: AtomicU64::new(1),
            view_edits: tokio::sync::Mutex::new(()),
            timezone: iana_time_zone::get_timezone().unwrap_or_else(|_| "UTC".into()),
            runtime: tokio::runtime::Handle::current(),
            az: Arc::new(entra::AzCli::new(cfg.agent_path.clone(), cfg.az_binary)),
            providers: provider::Providers::new(cfg.agent_path),
            assistant: assistant::Assistant::new(),
            process_groups: Arc::new(provider::agent::ProcessGroups::default()),
            next_chat_rev: AtomicU64::new(1),
            tunnels,
            updates,
        });
        tokio::fs::create_dir_all(shared.assistant_dir()).await?;
        shared.probe_all(ProbeDepth::Full);
        shared.spawn(provider::health_loop(Arc::downgrade(&shared)));
        shared.spawn(update::schedule(Arc::downgrade(&shared)));
        Ok(Arc::new(Self { shared }))
    }

    pub fn data_dir(&self) -> &Path {
        &self.shared.data_dir
    }

    /// The runtime the engine's tasks run on, for a caller outside any runtime, such as the
    /// shell's exit handler.
    pub fn runtime(&self) -> tokio::runtime::Handle {
        self.shared.runtime.clone()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AppEvent> {
        self.shared.events.subscribe()
    }

    pub fn boot(&self) -> BootSnapshot {
        let s = &self.shared;
        let live = s.connections.read().unwrap().iter().map(|(id, e)| e.state(*id)).collect();
        let consoles = s.consoles.read().unwrap().values().map(|c| c.latest.lock().unwrap().clone()).collect();
        let saved = s.store.connections().unwrap_or_else(|e| {
            eprintln!("connections load failed: {e}");
            store::SavedConnections { configs: Vec::new(), unreadable: 0 }
        });
        let mut workspace = s
            .store
            .kv_get(KV_WORKSPACE)
            .unwrap_or_else(|e| {
                eprintln!("workspace load failed: {e}");
                None
            })
            .unwrap_or_default();
        s.resolve_workspace(&mut workspace);
        let chats = s.store.chats(BOOT_CHATS).unwrap_or_else(|e| {
            eprintln!("chats load failed: {e}");
            Vec::new()
        });
        let mut live_chats = s.assistant.live_states();
        let chats = chats
            .into_iter()
            .map(|(id, r)| {
                live_chats.remove(&id).unwrap_or(ChatState {
                    id,
                    rev: 0,
                    title: r.title,
                    updated_at_ms: r.updated_at_ms,
                    options: r.options,
                    status: ChatStatus::Idle,
                    entry_count: r.entry_count,
                })
            })
            .collect();
        let saved_queries = saved::snapshot(&s.store).unwrap_or_else(|e| {
            eprintln!("saved queries load failed: {e}");
            SavedQueries { rev: 0, items: Vec::new() }
        });
        BootSnapshot {
            connections: saved.configs,
            unreadable_connections: saved.unreadable,
            live,
            consoles,
            workspace,
            settings: s.settings(),
            providers: s.provider_states(),
            chats,
            updates: s.updates.state(),
            saved_queries,
            home_dir: files::home_dir(),
        }
    }

    pub async fn save_connection(&self, d: ConnectionDraft) -> Result<ConnectionConfig, SaveError> {
        let s = &self.shared;
        let invalid = |message: &str| SaveError::Invalid { message: message.into() };
        if d.name.trim().is_empty() {
            return Err(invalid("name is required"));
        }
        if d.host.trim().is_empty() {
            return Err(invalid("host is required"));
        }
        if d.port == 0 {
            return Err(invalid("port must be between 1 and 65535"));
        }
        if d.database.trim().is_empty() {
            return Err(invalid("database is required"));
        }
        let sql_server_auth = d.sql_server_auth.filter(|_| d.database_kind == DatabaseKind::SqlServer);
        let newer_sign_in =
            || SaveError::Invalid { message: format!("{NEWER_SIGN_IN} Choose SQL login or Microsoft Entra ID.") };
        if sql_server_auth == Some(SqlServerAuth::Unknown) {
            return Err(newer_sign_in());
        }
        let entra = connection::uses_entra(d.database_kind, sql_server_auth);
        if !entra && d.user.trim().is_empty() {
            return Err(invalid("user is required"));
        }
        if let Some(t) = &d.ssh {
            if t.host.trim().is_empty() {
                return Err(invalid("SSH host is required"));
            }
            if t.port == 0 {
                return Err(invalid("SSH port must be between 1 and 65535"));
            }
            if t.user.trim().is_empty() {
                return Err(invalid("SSH user is required"));
            }
            if matches!(&t.auth, SshAuth::Key { path } if path.trim().is_empty()) {
                return Err(invalid("SSH key file is required"));
            }
        }
        let existing = d.id.and_then(|id| s.config(id));
        if existing.as_ref().is_some_and(|c| c.database_kind != d.database_kind) {
            return Err(invalid("An existing connection cannot change database kind. Create a new connection."));
        }
        if sql_server_auth.is_none()
            && existing.as_ref().is_some_and(|c| c.sql_server_auth == Some(SqlServerAuth::Unknown))
        {
            return Err(newer_sign_in());
        }
        let id = d.id.unwrap_or_else(|| ConnectionId(uuid::Uuid::new_v4()));
        let store = |e: StoreError| SaveError::Store { message: e.to_string() };
        let password = if entra { &PasswordInput::Clear } else { &d.password };
        let has_password = match password {
            PasswordInput::Set(pw) => {
                let sealed = s
                    .vault
                    .seal(id, SecretSlot::Database, pw)
                    .map_err(|e| SaveError::Keychain { message: e.to_string() })?;
                s.store.set_secret(id, SecretSlot::Database, &sealed).map_err(store)?;
                true
            }
            PasswordInput::Clear => {
                s.store.delete_secret(id, SecretSlot::Database).map_err(store)?;
                false
            }
            PasswordInput::Keep => existing.as_ref().is_some_and(|c| c.has_password),
        };
        let prior = existing.as_ref().and_then(|c| c.ssh.as_ref());
        let ssh = match d.ssh {
            Some(t) => Some(s.save_ssh(id, prior, t)?),
            None => {
                s.store.delete_secret(id, SecretSlot::Ssh).map_err(store)?;
                None
            }
        };
        let config = ConnectionConfig {
            database_kind: d.database_kind,
            id,
            name: d.name.trim().to_owned(),
            host: d.host.trim().to_owned(),
            port: d.port,
            database: d.database.trim().to_owned(),
            user: if entra { String::new() } else { d.user.trim().to_owned() },
            ssl_mode: d.ssl_mode,
            ca_cert_path: d.ca_cert_path.filter(|p| !p.trim().is_empty()),
            has_password,
            color: d.color,
            ssh,
            environment: d.environment,
            sql_server_auth,
        };
        s.store.upsert_connection(&config).map_err(store)?;
        Ok(config)
    }

    pub async fn delete_connection(&self, id: ConnectionId) -> Result<(), DisconnectError> {
        self.disconnect(id, false).await?;
        let s = &self.shared;
        s.store.delete_connection(id).map_err(|e| DisconnectError::Store { message: e.to_string() })?;
        s.connections.write().unwrap().remove(&id);
        Ok(())
    }

    /// The dialog's URL field. Pure, so the sync command never needs the runtime.
    pub fn parse_connection_url(&self, url: &str) -> Result<ConnectionDraft, String> {
        draft_from_url(url, "")
    }

    /// The statement chooser and the editor's run mark. Pure, so the sync command never needs the runtime.
    pub fn script_outline(&self, text: &str, cursor: DocOffset, database_kind: DatabaseKind) -> ScriptOutline {
        text::script_outline(text, cursor, database_kind)
    }

    pub fn script_params(
        &self,
        text: &str,
        target: RunTarget,
        database_kind: DatabaseKind,
    ) -> Result<Vec<Param>, RunError> {
        params::discover(text, target, database_kind)
    }

    /// The editor's reference underline and jump. Pure, so the sync command never needs the runtime.
    pub fn cell_references(&self, text: &str, database_kind: DatabaseKind) -> Vec<CellReference> {
        text::cell_references(text, database_kind)
    }

    /// The editor's completion. Pure, so the sync command never needs the runtime.
    pub fn completion_context(&self, text: &str, cursor: DocOffset, database_kind: DatabaseKind) -> CompletionContext {
        text::completion_context(text, cursor, database_kind)
    }

    /// The editor's format action. Pure, so the sync command never needs the runtime.
    pub fn editor_format(
        &self,
        text: &str,
        target: RunTarget,
        database_kind: DatabaseKind,
    ) -> Result<FormatEdit, FormatError> {
        text::format_edit(text, target, database_kind)
    }

    pub fn table_script(&self, db: DatabaseRef, schema: &str, table: &str) -> Result<TableScript, String> {
        let kind = self.shared.config(db.connection_id).ok_or("Unknown connection")?.database_kind;
        Ok(text::table_script(kind, schema, table))
    }
    pub fn table_order(
        &self,
        db: DatabaseRef,
        schema: &str,
        table: &str,
        order_by: &str,
        edit: TableOrderEdit,
    ) -> Result<TableOrderView, String> {
        let kind = self.shared.config(db.connection_id).ok_or("Unknown connection")?.database_kind;
        let catalog = match kind {
            DatabaseKind::Postgres => None,
            DatabaseKind::SqlServer => self
                .shared
                .instance(db.connection_id)
                .and_then(|instance| instance.slot(instance.canonical_database(&db.database)))
                .and_then(|slot| slot.last()),
        };
        text::table_order(kind, catalog.as_ref().and_then(|c| c.relation(schema, table)), order_by, edit)
    }

    pub async fn test_connection(&self, d: ConnectionDraft) -> Result<ServerInfo, ConnectError> {
        let s = &self.shared;
        let entra = s.entra(d.database_kind, d.sql_server_auth)?;
        let password = match (&d.password, d.id) {
            _ if entra.is_some() => None,
            (PasswordInput::Set(pw), _) => Some(pw.clone()),
            (PasswordInput::Clear, _) | (PasswordInput::Keep, None) => None,
            (PasswordInput::Keep, Some(id)) => s.saved_secret(id, SecretSlot::Database)?,
        };
        let via = match &d.ssh {
            Some(t) => {
                let prior = d.id.and_then(|id| s.config(id)).and_then(|c| c.ssh);
                let secret = match (&t.secret, d.id) {
                    (PasswordInput::Set(secret), _) => Some(secret.clone()),
                    (PasswordInput::Keep, Some(id)) if keeps_ssh_secret(prior.as_ref(), &t.auth) => {
                        s.saved_secret(id, SecretSlot::Ssh)?
                    }
                    _ => None,
                };
                Some(s.tunnels.route(None, TunnelSpec::new(&t.host, t.port, &t.user, &t.auth, secret))?)
            }
            None => None,
        };
        let session = Session::open(&ConnectParams::from_draft(&d, password, via, entra), None, &s.timezone).await?;
        Ok(session.server)
    }

    pub async fn connect(&self, id: ConnectionId) -> Result<ServerInfo, ConnectError> {
        self.shared.connect(id).await
    }

    pub async fn disconnect(&self, id: ConnectionId, force: bool) -> Result<(), DisconnectError> {
        let s = &self.shared;
        let consoles: Vec<Arc<Console>> = {
            let all = s.all_consoles();
            let mut mine = Vec::new();
            for c in all {
                if c.inner.lock().await.config.connection_id == id {
                    mine.push(c);
                }
            }
            mine
        };
        if !force {
            let open = s.open_transactions(&consoles).await;
            if !open.is_empty() {
                return Err(DisconnectError::OpenTransactions { tabs: open });
            }
        }
        for c in &consoles {
            c.drop_session(s).await;
        }
        let instance = s.instance(id);
        if s.connection_state(id).is_some() {
            s.set_connection(id, ConnSlot::Disconnected);
        }
        if let Some(instance) = instance {
            instance.close();
        }
        s.tunnels.forget(id);
        Ok(())
    }

    /// Adds a host key a dial reported as `HostKeyUnknown` to the app's known_hosts. Only a key
    /// the engine observed can be trusted, once, within ten minutes.
    pub fn ssh_trust_host_key(&self, key: HostKeyId) -> Result<(), String> {
        self.shared.tunnels.trust(key)
    }

    pub async fn catalog(&self, db: DatabaseRef) -> Result<Arc<Catalog>, CatalogError> {
        self.shared.catalog(&db).await
    }

    pub async fn refresh_catalog(&self, db: DatabaseRef) -> Result<u64, CatalogError> {
        let (instance, slot) = self.shared.slot(&db)?;
        let mut meta = slot.meta.lock().await;
        Ok(self.shared.load_catalog(&instance, &slot, &mut meta).await?.version)
    }

    /// Re-reads `pg_database`, drops the probe cache, reconciles the slots, and refreshes every
    /// catalog that has been loaded or has failed. The "Refresh" item on the connection menu.
    pub async fn refresh_connection(&self, id: ConnectionId) -> Result<(), CatalogError> {
        let s = &self.shared;
        let instance = s.instance(id).ok_or(CatalogError::NotConnected)?;
        s.reread_databases(&instance).await?;
        s.reconcile_instance(&instance);
        for name in instance.slot_names() {
            let Some(slot) = instance.slot(&name) else { continue };
            if !matches!(slot.status(), CatalogStatus::Idle | CatalogStatus::Loading) {
                s.schedule_catalog_refresh(slot.db.clone());
            }
        }
        Ok(())
    }

    /// Applies one picker edit to the connection's stored view: apply, persist, reconcile the
    /// slots, reload the catalogs whose system schemas changed, republish. Refused while
    /// disconnected: a checkbox edit resolves against the instance's lists, which an edit without
    /// one would materialize from nothing.
    pub async fn edit_view(&self, id: ConnectionId, edit: ViewEdit) -> Result<(), CatalogError> {
        let s = &self.shared;
        let _edits = s.view_edits.lock().await;
        let instance = s.instance(id).ok_or(CatalogError::NotConnected)?;
        let view = instance.view();
        let databases = instance.databases.read().unwrap().clone();
        // Showing a database whose catalog is unknown defers the empty-selection reset to its
        // first load. The check is registered before the catalogs are read: a load that finishes
        // in between is then seen here, and the check is withdrawn as `apply` covers it.
        let deferred = match &edit {
            ViewEdit::Database { name, shown: true } => Some(name.clone()),
            _ => None,
        };
        if let Some(name) = &deferred {
            instance.show_checks.lock().unwrap().insert(name.clone());
        }
        let catalogs = instance.catalogs();
        let known = Known { home: &instance.home, databases: &databases, catalogs: &catalogs };
        let next = view.apply(&edit, &known);
        s.store.set_view(id, &next).map_err(|e| CatalogError::Failed { message: e.to_string() })?;
        // A schema choice or a hide supersedes a pending check; the check must not later reset
        // a selection the user made on purpose.
        let settled = match &edit {
            ViewEdit::Database { name, .. } if deferred.is_none() || catalogs.contains_key(name) => Some(name),
            ViewEdit::Schemas { database, .. } | ViewEdit::Schema { database, .. } => Some(database),
            _ => None,
        };
        if let Some(name) = settled {
            instance.show_checks.lock().unwrap().remove(name);
        }
        let reload: Vec<String> = instance
            .slot_names()
            .into_iter()
            .filter(|db| {
                view::system_schemas_to_load(&view.database(db, &instance.home), instance.config.database_kind)
                    != view::system_schemas_to_load(&next.database(db, &instance.home), instance.config.database_kind)
            })
            .collect();
        *instance.view.write().unwrap() = next;
        s.reconcile_instance(&instance);
        for database in reload {
            s.schedule_catalog_refresh(DatabaseRef { connection_id: id, database });
        }
        Ok(())
    }

    /// Counts the non-system schemas of the named databases that have no slot and no cached
    /// count, each through a short-lived session, at most `PROBE_PERMITS` at a time. Fire and
    /// forget: each result lands on the connection snapshot.
    pub fn probe_databases(&self, id: ConnectionId, databases: Vec<String>) {
        let s = &self.shared;
        let Some(instance) = s.instance(id) else { return };
        let candidates: Vec<String> =
            databases.into_iter().filter(|name| instance.slot(name).is_none() && instance.knows(name)).collect();
        let mut fresh = Vec::new();
        {
            let mut probes = instance.probes.lock().unwrap();
            for name in candidates {
                if !probes.contains_key(&name) {
                    probes.insert(name.clone(), SchemaCount::Probing);
                    fresh.push(name);
                }
            }
        }
        if fresh.is_empty() {
            return;
        }
        s.republish_connection(id);
        for name in fresh {
            let (shared, instance) = (s.clone(), instance.clone());
            s.spawn(async move {
                let _permit = instance.probe_gate.acquire().await;
                if instance.is_closed() {
                    return;
                }
                let probe = async {
                    let params =
                        shared.params(&instance.config, &name).map_err(|e| connection::connect_error_text(&e))?;
                    let mut session = Session::open_metadata(&params, &shared.timezone)
                        .await
                        .map_err(|e| connection::connect_error_text(&e))?;
                    session.count_schemas().await.map_err(|e| e.message)
                };
                let count = match tokio::time::timeout(PROBE_TIMEOUT, probe).await {
                    Ok(Ok(count)) => SchemaCount::Known { count },
                    Ok(Err(message)) => SchemaCount::Unavailable { message },
                    Err(_) => {
                        SchemaCount::Unavailable { message: format!("timed out after {} s", PROBE_TIMEOUT.as_secs()) }
                    }
                };
                if instance.is_closed() {
                    return;
                }
                // A re-read cleared the cache while this probe ran; its count is of the old list.
                let mut probes = instance.probes.lock().unwrap();
                if probes.get(&name) != Some(&SchemaCount::Probing) {
                    return;
                }
                probes.insert(name, count);
                drop(probes);
                shared.republish_connection(id);
            });
        }
    }

    /// Counts on the SQL Server tab's native session, or PostgreSQL's metadata session.
    pub async fn exact_row_count(
        &self,
        tab_id: TabId,
        db: DatabaseRef,
        schema: String,
        table: String,
        query: TableQuery,
    ) -> Result<u64, CountError> {
        let (instance, slot) = self.shared.slot(&db).map_err(|_| CountError::NotConnected)?;
        if instance.config.database_kind == DatabaseKind::Postgres {
            let sql = text::table_count(DatabaseKind::Postgres, &schema, &table, None, &query)
                .map_err(|message| CountError::Failed { message })?;
            let mut meta = slot.meta.lock().await;
            let session = self
                .shared
                .session_in(&instance, &slot, &mut meta)
                .await
                .map_err(|e| CountError::Failed { message: connection::connect_error_text(&e) })?;
            return session.count(&sql).await.map_err(|e| CountError::Failed { message: e.message });
        }

        let s = &self.shared;
        let failed = |message: &str| CountError::Failed { message: message.into() };
        let console = s.console(tab_id).ok_or_else(|| failed("The count tab is not configured."))?;
        let validate = |inner: &console::ConsoleInner| {
            if !s.instance(db.connection_id).is_some_and(|current| Arc::ptr_eq(&current, &instance)) {
                return Err(CountError::NotConnected);
            }
            if console.closed.load(Ordering::Relaxed)
                || !s.console(tab_id).is_some_and(|current| Arc::ptr_eq(&current, &console))
                || inner.config.connection_id != db.connection_id
                || instance.canonical_database(&inner.config.database) != instance.canonical_database(&db.database)
            {
                return Err(failed("The count tab's target changed."));
            }
            let current =
                s.config(inner.config.connection_id).ok_or_else(|| failed("The count tab is not configured."))?;
            if current.database_kind != DatabaseKind::SqlServer || inner.config.mode != TxnMode::AutoCommit {
                return Err(failed("SQL Server counts require a native Auto-commit tab."));
            }
            if matches!(inner.session, SessionSlot::Busy { .. }) {
                return Err(failed("The count tab is busy."));
            }
            Ok(())
        };
        {
            let inner = console.inner.lock().await;
            validate(&inner)?;
        }
        let catalog = if !query.filters.is_empty() {
            Some(self.shared.catalog(&db).await.map_err(|error| match error {
                CatalogError::Failed { message } => CountError::Failed { message },
                _ => CountError::Failed { message: "Cannot load column types for table filters.".into() },
            })?)
        } else {
            None
        };
        let relation = catalog.as_ref().and_then(|catalog| catalog.relation(&schema, &table));
        let (config, needs_session) = {
            let inner = console.inner.lock().await;
            validate(&inner)?;
            (inner.config.clone(), matches!(inner.session, SessionSlot::Empty | SessionSlot::Lost { .. }))
        };
        let sql = text::table_count(DatabaseKind::SqlServer, &schema, &table, relation, &query)
            .map_err(|message| CountError::Failed { message })?;
        let fresh = if needs_session {
            Some(
                s.open_tab_session(&config)
                    .await
                    .map_err(|error| CountError::Failed { message: connection::connect_error_text(&error) })?,
            )
        } else {
            None
        };
        let run_id = s.next_run_id();
        let mut inner = console.inner.lock().await;
        validate(&inner)?;
        if let Some(idle) = fresh
            && matches!(inner.session, SessionSlot::Empty | SessionSlot::Lost { .. })
        {
            inner.session = SessionSlot::Idle(idle);
        }
        let (session, stop) =
            console.take_session(&mut inner, run_id, Work::Count).ok_or_else(|| failed("The count tab is busy."))?;
        console.publish(&mut inner, s);
        drop(inner);

        let (reply, result) = tokio::sync::oneshot::channel();
        s.spawn_holding(
            console.clone(),
            run_id,
            console::count_task(console, s.clone(), session, run_id, sql, stop, reply),
        );
        result.await.unwrap_or_else(|_| Err(failed("The count task stopped.")))
    }

    /// Defaults, keys, indexes, and triggers of one relation, read on the database's metadata
    /// session when the tree opens the relation. The engine keeps no copy.
    pub async fn relation_detail(&self, db: DatabaseRef, oid: u32) -> Result<RelationDetail, DetailError> {
        self.shared.relation_detail(&db, oid).await
    }

    /// The DDL that creates `target`, read on the database's metadata session.
    pub async fn object_script(&self, db: DatabaseRef, target: ScriptTarget) -> Result<ObjectScript, DetailError> {
        self.shared.object_script(&db, &target).await
    }

    /// A connection, database, or mode change needs an idle tab with no open transaction. A
    /// connection or database change also resets the session.
    pub async fn console_configure(&self, tab: TabId, c: ConsoleConfig) -> Result<ConsoleState, ConfigureError> {
        let s = &self.shared;
        let connection = s.config(c.connection_id).ok_or(ConfigureError::UnknownConnection)?;
        txn::validate_mode(connection.database_kind, c.mode).map_err(|what| ConfigureError::Unsupported { what })?;
        let mut view =
            ConsoleConfigView { connection_id: c.connection_id, database: c.database, schema: c.schema, mode: c.mode };
        let console = {
            let mut map = s.consoles.write().unwrap();
            map.entry(tab).or_insert_with(|| Arc::new(Console::new(tab, view.clone()))).clone()
        };
        let mut inner = console.inner.lock().await;
        if let Some(instance) = s.instance(view.connection_id) {
            view.database = instance.canonical_database(&view.database).to_owned();
        }
        let previous = inner.config.connection_id;
        let target_changed = inner.config.connection_id != view.connection_id || inner.config.database != view.database;
        let mode_changed = inner.config.mode != view.mode;
        if target_changed || mode_changed {
            if matches!(inner.session, SessionSlot::Busy { .. }) {
                return Err(ConfigureError::Busy);
            }
            if inner.txn != TxnState::Idle {
                return Err(ConfigureError::TransactionOpen);
            }
        }
        let due = if target_changed {
            let due = inner.finish_refreshes(false);
            inner.end_session(SessionSlot::Empty);
            due
        } else {
            None
        };
        if inner.config != view {
            inner.import = None;
        }
        inner.config = view.clone();
        let state = console.publish(&mut inner, s);
        drop(inner);
        if let Some((db, owed)) = due {
            s.schedule_refreshes(db, owed);
        }
        s.reconcile(view.connection_id);
        if previous != view.connection_id {
            s.reconcile(previous);
        }
        Ok(state)
    }

    pub async fn console_run(
        &self,
        tab: TabId,
        r: RunRequest,
        confirmed: Option<WriteDigest>,
    ) -> Result<RunAccepted, RunError> {
        let s = &self.shared;
        let console = s.console(tab).ok_or(RunError::NotConfigured)?;
        let (mut config, busy) = {
            let inner = console.inner.lock().await;
            (inner.config.clone(), matches!(inner.session, SessionSlot::Busy { .. }))
        };
        let connection = s.config(config.connection_id).ok_or(RunError::NotConfigured)?;
        txn::validate_mode(connection.database_kind, config.mode).map_err(|what| RunError::Unsupported { what })?;
        if busy {
            return Err(RunError::Busy);
        }
        s.connect(config.connection_id).await.map_err(|error| RunError::Connect { error })?;
        let instance = s.instance(config.connection_id).ok_or(RunError::NotConfigured)?;
        config.database = instance.canonical_database(&config.database).to_owned();
        if instance.config.database_kind == DatabaseKind::SqlServer && !instance.knows(&config.database) {
            return Err(RunError::NotConfigured);
        }

        let mut explain_mode = None;
        let mut page = None;
        let mut sources = Vec::new();
        let executable = match r {
            RunRequest::Script { text, target, mode, params, cells } => {
                explain_mode = mode.explain();
                let bound = params::bind(text, target, connection.database_kind, params)?;
                let (expanded, inlined) = sql::refs::expand(bound, &cells, connection.database_kind)?;
                sources = inlined;
                expanded
            }
            RunRequest::TablePage { schema, table, page: index, query } => {
                let catalog = self.catalog(config.db()).await.map_err(|_| RunError::NotConfigured)?;
                let relation = catalog.relation(&schema, &table).cloned();
                let size = s.settings().table_page_size.max(1);
                let window = Some((size, u64::from(index) * u64::from(size)));
                let text =
                    text::table_browse(connection.database_kind, &schema, &table, relation.as_ref(), &query, window)
                        .map_err(|_| RunError::BadFilter)?;
                let all =
                    text::table_browse(connection.database_kind, &schema, &table, relation.as_ref(), &query, None)
                        .map_err(|_| RunError::BadFilter)?;
                page =
                    Some(console::TablePage { rerun: export::rerun(connection.database_kind, &all, true), relation });
                let spans = text::pick(&text, sql::ByteTarget::All, connection.database_kind)
                    .map_err(|_| RunError::BadFilter)?;
                if spans.len() != 1 {
                    return Err(RunError::BadFilter);
                }
                let ranges = spans.iter().map(|span| text::OffsetMap::new(&text).range(span)).collect();
                let changed = vec![false; spans.len()];
                params::ExecutableScript { text, spans, ranges, changed }
            }
        };
        let params::ExecutableScript { text, spans, ranges, changed } = executable;
        if spans.len() > usize::from(u16::MAX) {
            return Err(RunError::Unsupported { what: "Too many execution units".into() });
        }
        if spans.is_empty() {
            return Err(RunError::NoStatement);
        }
        if explain_mode.is_none() && connection.database_kind == DatabaseKind::SqlServer {
            for span in &spans {
                sql::tsql::validate_batch(span.text(&text)).map_err(|what| RunError::Unsupported { what })?;
            }
        }
        let statements: Vec<PlannedStmt> = if explain_mode.is_some() {
            Vec::new()
        } else {
            spans
                .iter()
                .enumerate()
                .map(|(index, span)| {
                    let sql = span.text(&text);
                    let kind = text::classify(sql, connection.database_kind);
                    let transaction = txn::plan(connection.database_kind, config.mode, &kind)
                        .map_err(|what| RunError::Unsupported { what })?;
                    Ok(PlannedStmt {
                        span: *span,
                        origin: ranges[index].from,
                        changed: changed[index],
                        kind,
                        transaction,
                    })
                })
                .collect::<Result<_, RunError>>()?
        };
        if statements.iter().any(|p| p.kind == sql::StmtKind::CopyStdio) {
            return Err(RunError::Unsupported { what: "COPY FROM STDIN / TO STDOUT".into() });
        }
        let sent: Vec<&str> = spans.iter().map(|span| span.text(&text)).collect();

        let mut fresh = None;
        let needs_session = matches!(console.inner.lock().await.session, SessionSlot::Empty | SessionSlot::Lost { .. });
        if needs_session {
            fresh = Some(s.open_tab_session(&config).await.map_err(|error| RunError::Connect { error })?);
        }

        let mut inner = console.inner.lock().await;
        let current = s.config(inner.config.connection_id).ok_or(RunError::NotConfigured)?;
        txn::validate_mode(current.database_kind, inner.config.mode).map_err(|what| RunError::Unsupported { what })?;
        if inner.config.connection_id == instance.config.id {
            inner.config.database = instance.canonical_database(&inner.config.database).to_owned();
        }
        if inner.config != config {
            return Err(RunError::Busy);
        }
        if let Some(idle) = fresh {
            match inner.session {
                SessionSlot::Empty | SessionSlot::Lost { .. } => inner.session = SessionSlot::Idle(idle),
                SessionSlot::Idle(_) => {}
                SessionSlot::Busy { .. } => return Err(RunError::Busy),
            }
        }
        let SessionSlot::Idle(idle) = &inner.session else { return Err(RunError::Busy) };
        let target = writes::Target {
            saved: &current,
            opened: idle.opened(),
            console: &config,
            txn: inner.txn,
            unasked: &inner.unasked,
        };
        if let Some(mode) = explain_mode {
            if current.database_kind == DatabaseKind::SqlServer {
                let analyze = matches!(mode, explain::ExplainMode::Analyze);
                let prepared = match showplan::prepare(analyze, &text, &spans, &target, confirmed.as_ref()) {
                    Ok(prepared) => prepared,
                    Err(RunError::ConfirmWrites { confirmation }) => {
                        console.publish(&mut inner, s);
                        return Err(RunError::ConfirmWrites { confirmation });
                    }
                    Err(error) => return Err(error),
                };
                let run_id = s.next_run_id();
                let entered = inner.txn;
                let launch = showplan::launch(
                    &console,
                    &mut inner,
                    run_id,
                    prepared,
                    config.schema.clone(),
                    current.clone(),
                    config.database.clone(),
                    entered,
                )?;
                console.results.lock().unwrap().clear();
                inner.run = RunState::Running {
                    run_id,
                    started_at_ms: console::now_ms(),
                    statements: ranges
                        .iter()
                        .enumerate()
                        .map(|(i, range)| StatementRun {
                            index: i as u16,
                            range: *range,
                            status: StmtStatus::Pending,
                            outputs: Vec::new(),
                        })
                        .collect(),
                };
                console.publish(&mut inner, s);
                drop(inner);
                s.spawn_holding(console.clone(), run_id, showplan::run(console.clone(), s.clone(), launch));
                return Ok(RunAccepted { run_id, statements: ranges, sources });
            }
            let prepared = match explain::prepare(
                mode,
                current.database_kind,
                inner.txn,
                &text,
                &spans,
                &target,
                confirmed.as_ref(),
            ) {
                Ok(prepared) => prepared,
                Err(RunError::ConfirmWrites { confirmation }) => {
                    console.publish(&mut inner, s);
                    return Err(RunError::ConfirmWrites { confirmation });
                }
                Err(error) => return Err(error),
            };
            let prepared = prepared.source(ranges[0].from, changed[0]);
            let run_id = s.next_run_id();
            let entered = inner.txn;
            let launch = explain::launch(
                &console,
                &mut inner,
                run_id,
                prepared,
                config.schema.clone(),
                current.clone(),
                config.database.clone(),
                entered,
            )?;
            console.results.lock().unwrap().clear();
            inner.run = RunState::Running {
                run_id,
                started_at_ms: console::now_ms(),
                statements: ranges
                    .iter()
                    .enumerate()
                    .map(|(i, range)| StatementRun {
                        index: i as u16,
                        range: *range,
                        status: StmtStatus::Pending,
                        outputs: Vec::new(),
                    })
                    .collect(),
            };
            console.publish(&mut inner, s);
            drop(inner);
            s.spawn_holding(console.clone(), run_id, explain::run(console.clone(), s.clone(), launch));
            return Ok(RunAccepted { run_id, statements: ranges, sources });
        }
        debug_assert_eq!(sent.len(), statements.len(), "the fence bits index the planned statements");
        let (checked, read_only) = match writes::check_run(&target, &sent, &inner.proven, confirmed.as_ref()) {
            Ok(checked) => checked,
            Err(confirmation) => {
                console.publish(&mut inner, s);
                return Err(RunError::ConfirmWrites { confirmation });
            }
        };
        let writes_stay_unasked = checked.writes_stay_unasked();
        let run_id = s.next_run_id();
        let Some((session, stop)) = console.take_session(&mut inner, run_id, Work::Run(checked)) else {
            return Err(RunError::Busy);
        };
        let txn = inner.txn;
        console.results.lock().unwrap().clear();
        inner.run = RunState::Running {
            run_id,
            started_at_ms: console::now_ms(),
            statements: ranges
                .iter()
                .enumerate()
                .map(|(i, range)| StatementRun {
                    index: i as u16,
                    range: *range,
                    status: StmtStatus::Pending,
                    outputs: Vec::new(),
                })
                .collect(),
        };
        console.publish(&mut inner, s);
        drop(inner);

        let plan = RunPlan {
            run_id,
            text,
            statements,
            txn,
            schema: config.schema.clone(),
            database: config.database.clone(),
            connection,
            page,
            writes_stay_unasked,
            read_only,
        };
        s.spawn_holding(console.clone(), run_id, console::run_task(console.clone(), s.clone(), session, plan, stop));
        Ok(RunAccepted { run_id, statements: ranges, sources })
    }

    pub async fn console_cancel(&self, tab: TabId) {
        let Some(console) = self.shared.console(tab) else { return };
        let cancel = {
            let inner = console.inner.lock().await;
            match &inner.session {
                SessionSlot::Busy { cancel, stop, .. } => {
                    stop.store(true, Ordering::Relaxed);
                    cancel.clone()
                }
                _ => None,
            }
        };
        if let Some(cancel) = cancel {
            cancel.cancel().await;
        }
    }

    pub async fn console_commit(&self, tab: TabId, confirmed: Option<WriteDigest>) -> Result<(), TxnError> {
        self.shared.console_txn_command(tab, true, confirmed).await
    }

    pub async fn console_rollback(&self, tab: TabId) -> Result<(), TxnError> {
        self.shared.console_txn_command(tab, false, None).await
    }

    /// Applies PostgreSQL staged table edits on the tab's own session, all or nothing. With no transaction open, auto-commit
    /// mode commits them and manual mode leaves them in a new open transaction. Inside an open transaction they
    /// run under a savepoint, so a failure leaves that transaction as it was.
    pub async fn table_apply(
        &self,
        tab: TabId,
        changes: TableChanges,
        confirmed: Option<WriteDigest>,
    ) -> Result<ApplySummary, ApplyError> {
        let s = &self.shared;
        let console = s.console(tab).ok_or(ApplyError::NotConfigured)?;
        let config = console.inner.lock().await.config.clone();
        let connection = s.config(config.connection_id).ok_or(ApplyError::NotConfigured)?;
        if connection.database_kind == DatabaseKind::SqlServer {
            return Err(ApplyError::Unsupported { what: session::SQL_SERVER_EDITS_UNSUPPORTED.into() });
        }
        s.connect(config.connection_id).await.map_err(|error| ApplyError::Connect { error })?;
        let catalog = self.catalog(config.db()).await.map_err(|_| ApplyError::NotConfigured)?;
        let relation = catalog.relation(&changes.schema, &changes.table).ok_or(ApplyError::NotConfigured)?;
        let pk = relation.pk_columns();
        let full_key = |key: &[CellWrite]| {
            !pk.is_empty() && key.len() == pk.len() && pk.iter().all(|p| key.iter().any(|k| &k.column == p))
        };
        if !changes.deletes.iter().all(|k| full_key(k)) || !changes.updates.iter().all(|u| full_key(&u.key)) {
            return Err(ApplyError::NoPrimaryKey);
        }

        let plan = session::plan_edits(&changes, relation)?;

        let needs_session = matches!(console.inner.lock().await.session, SessionSlot::Empty | SessionSlot::Lost { .. });
        let mut fresh = None;
        if needs_session {
            fresh = Some(s.open_tab_session(&config).await.map_err(|error| ApplyError::Connect { error })?);
        }

        let (mut session, txn, stop, (open, finish, undo), run_id, writes_stay_unasked) = {
            let mut inner = console.inner.lock().await;
            let current = s.config(inner.config.connection_id).ok_or(ApplyError::NotConfigured)?;
            if current.database_kind == DatabaseKind::SqlServer
                || matches!(&inner.session, SessionSlot::Idle(idle) if idle.database_kind() == DatabaseKind::SqlServer)
            {
                return Err(ApplyError::Unsupported { what: session::SQL_SERVER_EDITS_UNSUPPORTED.into() });
            }
            if inner.config != config {
                return Err(ApplyError::Busy);
            }
            if let Some(idle) = fresh {
                match inner.session {
                    SessionSlot::Empty | SessionSlot::Lost { .. } => inner.session = SessionSlot::Idle(idle),
                    SessionSlot::Idle(_) => {}
                    SessionSlot::Busy { .. } => return Err(ApplyError::Busy),
                }
            }
            if inner.txn == TxnState::Aborted {
                return Err(ApplyError::TransactionAborted);
            }
            let txn = inner.txn;
            let wrap = if txn == TxnState::Active {
                (
                    "SAVEPOINT l1_apply",
                    Some("RELEASE SAVEPOINT l1_apply"),
                    "ROLLBACK TO SAVEPOINT l1_apply; RELEASE SAVEPOINT l1_apply",
                )
            } else {
                ("BEGIN", (config.mode == TxnMode::AutoCommit).then_some("COMMIT"), "ROLLBACK")
            };
            let sent: Vec<&str> =
                std::iter::once(wrap.0).chain(plan.iter().map(|edit| edit.sql.as_str())).chain(wrap.1).collect();
            let SessionSlot::Idle(idle) = &inner.session else { return Err(ApplyError::Busy) };
            let target = writes::Target {
                saved: &current,
                opened: idle.opened(),
                console: &config,
                txn,
                unasked: &inner.unasked,
            };
            let checked = match writes::check(&target, &sent, confirmed.as_ref()) {
                Ok(checked) => checked,
                Err(confirmation) => {
                    console.publish(&mut inner, s);
                    return Err(ApplyError::ConfirmWrites { confirmation });
                }
            };
            let writes_stay_unasked = checked.writes_stay_unasked();
            let run_id = s.next_run_id();
            let Some((session, stop)) = console.take_session(&mut inner, run_id, Work::Edit(checked)) else {
                return Err(ApplyError::Busy);
            };
            console.publish(&mut inner, s);
            (session, txn, stop, wrap, run_id, writes_stay_unasked)
        };

        let shared = s.clone();
        let task_console = console.clone();
        let applied = s.run_holding(console.clone(), run_id, async move {
            let s = &shared;
            let console = &task_console;
            let inside = txn == TxnState::Active;
            let mut lines = Vec::new();
            let mut outcome: Result<(), ApplyError> = async {
                session.batch(open).await.map_err(|e| ApplyError::Transaction { error: e })?;
                for (i, edit) in plan.iter().enumerate() {
                    let change = &edit.change;
                    let sql = &edit.sql;
                    if stop.load(Ordering::Relaxed) {
                        return Err(ApplyError::Transaction { error: session::error("Cancelled") });
                    }
                    let statement = i as u16;
                    lines.push(console::statement_line(statement, sql));
                    let started = std::time::Instant::now();
                    let executed = session.execute_edit(edit).await;
                    let elapsed_ms = started.elapsed().as_millis().min(u32::MAX as u128) as u32;
                    lines.extend(console::notice_lines(&mut session));
                    let rows = match executed {
                        Ok(rows) => rows,
                        Err(e) => {
                            let error = e;
                            lines.push(OutputBody::Error { statement: Some(statement), error: error.clone() });
                            return Err(ApplyError::Failed { change: *change, error });
                        }
                    };
                    let verb = match change.kind {
                        ChangeKind::Insert => "INSERT 0",
                        ChangeKind::Update => "UPDATE",
                        ChangeKind::Delete => "DELETE",
                    };
                    lines.push(OutputBody::Command { statement, tag: format!("{verb} {rows}"), elapsed_ms });
                    if change.kind != ChangeKind::Insert && rows != 1 {
                        return Err(ApplyError::RowMismatch { change: *change, rows });
                    }
                }
                if let Some(finish) = finish {
                    session.batch(finish).await.map_err(|error| ApplyError::OutcomeUnknown { error })?;
                }
                Ok(())
            }
            .await;
            if outcome.is_err()
                && let Err(rollback) = session.batch(undo).await
            {
                let message = format!(
                    "Staged edits failed; rollback also failed: {}. The session was retired. Commit outcome may be unknown.",
                    rollback.message
                );
                session.retire(message.clone());
                outcome = Err(ApplyError::OutcomeUnknown { error: session::error(message) });
            }
            let after = session.observed_txn().unwrap_or(match (&outcome, inside, finish) {
                (_, true, _) => TxnState::Active,
                (Ok(()), false, None) => TxnState::Active,
                _ => TxnState::Idle,
            });
            let lost = session.closed_reason_settled(None).await;
            lines.extend(console::notice_lines(&mut session));
            if let Err(ApplyError::Transaction { error } | ApplyError::OutcomeUnknown { error }) = &outcome {
                lines.push(OutputBody::Error { statement: None, error: error.clone() });
            }
            console.output_all(s, run_id, lines);
            let held = writes_stay_unasked && outcome.is_ok();
            let ended = !inside && (finish.is_some() || outcome.is_err());
            let returned = console.edit(s, run_id, |inner| {
                if held {
                    plan.iter().for_each(|edit| inner.unasked.add(&edit.sql));
                }
                if ended {
                    inner.unasked = WriteList::default();
                }
                Console::return_session(inner, session, after, lost)
            });
            if let Ok(Some((db, owed))) = returned.await {
                s.schedule_refreshes(db, owed);
            }
            outcome.map(|()| ApplySummary { committed: after == TxnState::Idle })
        });
        applied.await.unwrap_or_else(|| {
            Err(ApplyError::OutcomeUnknown {
                error: session::error("The edit stopped inside L1 Data and its session was closed."),
            })
        })
    }

    pub async fn table_import_prepare(
        &self,
        tab: TabId,
        target: ImportTarget,
        file: String,
    ) -> Result<ImportSourceId, ImportError> {
        import::prepare(&self.shared, tab, target, file).await
    }

    pub async fn table_import_preview(
        &self,
        tab: TabId,
        source: ImportSourceId,
        header: bool,
    ) -> Result<ImportPreview, ImportError> {
        import::preview(&self.shared, tab, source, header).await
    }

    pub async fn table_import(
        &self,
        tab: TabId,
        request: ImportRequest,
        confirmed: Option<WriteDigest>,
    ) -> Result<ImportSummary, ImportError> {
        import::import(&self.shared, tab, request, confirmed).await
    }

    pub async fn table_import_discard(&self, tab: TabId, source: ImportSourceId) -> Result<(), ImportError> {
        import::discard(&self.shared, tab, source).await
    }

    pub async fn console_close(&self, tab: TabId, force: bool) -> Result<(), DisconnectError> {
        let s = &self.shared;
        let Some(console) = s.console(tab) else { return Ok(()) };
        if !force {
            let open = s.open_transactions(std::slice::from_ref(&console)).await;
            if !open.is_empty() {
                return Err(DisconnectError::OpenTransactions { tabs: open });
            }
        }
        console.closed.store(true, Ordering::Relaxed);
        console.drop_session(s).await;
        s.consoles.write().unwrap().remove(&tab);
        let connection_id = console.latest.lock().unwrap().config.connection_id;
        s.reconcile(connection_id);
        Ok(())
    }

    pub fn result_page(&self, r: ResultRef, start: u32, len: u32) -> Result<RowPage, ResultError> {
        let set = self.shared.result_set(r).ok_or(ResultError::Gone)?;
        set.page(start, len).ok_or(ResultError::OutOfRange)
    }

    pub fn result_cell(&self, r: ResultRef, row: u32, col: u16) -> Result<Option<String>, ResultError> {
        let set = self.shared.result_set(r).ok_or(ResultError::Gone)?;
        set.cell(row, col).ok_or(ResultError::OutOfRange)
    }

    pub async fn result_cell_matches(
        &self,
        r: ResultRef,
        cell: ResultCell,
        query: String,
    ) -> Result<bool, ResultError> {
        let set = self.shared.result_set(r).ok_or(ResultError::Gone)?;
        let shared = self.shared.clone();
        self.shared
            .runtime
            .spawn_blocking(move || set.cell_matches(cell, &query, || shared.result_set(r).is_some()))
            .await
            .expect("result_cell_matches task panicked")
    }

    pub async fn result_find(
        &self,
        r: ResultRef,
        query: String,
        direction: ResultFindDirection,
    ) -> Result<ResultFind, ResultError> {
        let set = self.shared.result_set(r).ok_or(ResultError::Gone)?;
        let shared = self.shared.clone();
        self.shared
            .runtime
            .spawn_blocking(move || set.find(&query, direction, || shared.result_set(r).is_some()))
            .await
            .expect("result_find task panicked")
    }

    pub async fn result_aggregate(
        &self,
        r: ResultRef,
        rows: RowSpan,
        cols: ColSpan,
    ) -> Result<CellAggregate, ResultError> {
        let set = self.shared.result_set(r).ok_or(ResultError::Gone)?;
        self.shared
            .runtime
            .spawn_blocking(move || {
                let copied = set.copy_rect(rows, cols)?;
                Ok(aggregate::aggregate(&copied))
            })
            .await
            .expect("result_aggregate task panicked")
    }

    pub async fn result_series(
        &self,
        r: ResultRef,
        x: Option<u16>,
        y: u16,
        max_points: u32,
    ) -> Result<ChartSeries, ResultError> {
        let set = self.shared.result_set(r).ok_or(ResultError::Gone)?;
        self.shared
            .runtime
            .spawn_blocking(move || set.series(x, y, max_points))
            .await
            .expect("result_series task panicked")
    }

    pub async fn result_profile(&self, r: ResultRef, col: u16) -> Result<ColumnProfile, ResultError> {
        let set = self.shared.result_set(r).ok_or(ResultError::Gone)?;
        self.shared
            .runtime
            .spawn_blocking(move || Ok(profile::profile(&set.column(col)?)))
            .await
            .expect("result_profile task panicked")
    }

    /// The SQL of the rows behind one row of a grouped PostgreSQL result. It rewrites the SQL that
    /// produced the result, and only the SQL that may run again unasked.
    pub fn result_drill(&self, r: ResultRef, point: DrillPoint) -> Result<DrillQuery, DrillError> {
        let set = self.shared.result_set(r).ok_or(DrillError::Gone)?;
        let unsupported = |reason: &str| DrillError::Unsupported { reason: reason.into() };
        if set.database_kind != DatabaseKind::Postgres {
            return Err(unsupported("Drill-down works on PostgreSQL results for now."));
        }
        if set.origin.page {
            return Err(unsupported("A table page filters the table instead."));
        }
        let sql = match &set.origin.rerun {
            results::Rerun::Ready(sql) => sql,
            results::Rerun::Unvetted { .. } => {
                return Err(unsupported("The chart's query calls functions that may not be safe to run again."));
            }
            results::Rerun::Refused(reason) => return Err(unsupported(reason)),
        };
        let drill = sql::drill::plan(sql).map_err(|reason| {
            if sql::drill::UNGROUPED.contains(&reason) { DrillError::Ungrouped } else { unsupported(reason) }
        })?;
        let keys = drill
            .columns()
            .map(|c| set.columns.get(usize::from(c)))
            .collect::<Option<Vec<_>>>()
            .ok_or(DrillError::OutOfRange)?;
        // PostgreSQL cannot read a text literal as an anonymous record, so `(ROW(a, b)) = '(1,2)'` always fails.
        if keys.iter().any(|k| k.r#type.display == "record") {
            return Err(unsupported("A grouped column is an anonymous ROW, which cannot be compared to its text."));
        }
        let query = |sql: String| DrillQuery { sql, db: set.db.clone(), schema: set.origin.schema.clone() };
        match point {
            DrillPoint::Probe => Ok(query(String::new())),
            DrillPoint::Row { row, title } => {
                let values = drill
                    .columns()
                    .map(|c| set.cell(row, c))
                    .collect::<Option<Vec<_>>>()
                    .ok_or(DrillError::OutOfRange)?;
                Ok(query(drill.sql(&values, &title)))
            }
        }
    }

    pub fn result_plan(&self, r: ResultRef) -> Result<Plan, ResultError> {
        let set = self.shared.result_set(r).ok_or(ResultError::Gone)?;
        if set.meta().truncated.is_some() {
            return Err(ResultError::NotAPlan { what: "The plan was truncated.".into() });
        }
        let format = set.plan_format.ok_or(ResultError::NotAPlan { what: "The result is not a plan.".into() })?;
        let cell = set.cell(0, 0).ok_or(ResultError::NotAPlan { what: "The result is not a plan.".into() })?;
        drop(set);
        let Some(document) = cell else {
            return Err(ResultError::NotAPlan { what: "The result is not a plan.".into() });
        };
        let parsed = match format {
            PlanFormat::Json => plan::parse(&document),
            PlanFormat::Xml => showplan::parse(&document),
        };
        parsed.map_err(|what| ResultError::NotAPlan { what })
    }

    /// Writes one result to `path` as `format`. The file appears at `path` only when complete, and
    /// every error leaves `path` as it was.
    pub async fn result_export(
        &self,
        r: ResultRef,
        format: ExportFormat,
        scope: ExportScope,
        path: String,
    ) -> Result<ExportSummary, ExportError> {
        export::export(&self.shared, r, format, scope, path).await
    }

    pub async fn notebook_open(&self, path: String) -> Result<NotebookOpened, FileReadError> {
        let read = move || {
            files::open(&path).map(|f| NotebookOpened {
                binding: f.binding,
                name: f.name,
                notebook: notebook_file::parse(&f.text),
            })
        };
        self.shared
            .runtime
            .spawn_blocking(read)
            .await
            .unwrap_or_else(|e| Err(FileReadError::Io { message: e.to_string() }))
    }

    /// The notebook is printed here, never in the webview (decision 9).
    pub async fn notebook_save(
        &self,
        target: SaveTarget,
        notebook: NotebookText,
    ) -> Result<NotebookSaved, FileWriteError> {
        let write = move || {
            let (text, plain) = notebook_file::print(&notebook);
            files::save(target, &text).map(|f| NotebookSaved { binding: f.binding, name: f.name, plain })
        };
        self.shared
            .runtime
            .spawn_blocking(write)
            .await
            .unwrap_or_else(|e| Err(FileWriteError::Io { message: e.to_string() }))
    }

    pub fn history(&self, q: HistoryQuery) -> HistoryPage {
        self.shared.store.history(&q).unwrap_or_else(|e| {
            eprintln!("history query failed: {e}");
            HistoryPage { entries: Vec::new(), has_more: false }
        })
    }

    pub fn history_browse(&self, q: HistoryBrowseQuery) -> Result<HistoryBrowsePage, HistoryError> {
        self.shared.store.history_browse(&q)
    }

    pub fn history_group(&self, q: HistoryGroupQuery) -> Result<HistoryGroupPage, HistoryError> {
        self.shared.store.history_group(&q)
    }

    pub fn clear_history(&self) {
        if let Err(e) = self.shared.store.clear_history() {
            eprintln!("history clear failed: {e}");
        }
    }

    pub fn saved_query_save(
        &self,
        text: &str,
        target: RunTarget,
        database_kind: DatabaseKind,
    ) -> Result<SaveQueryReply, SavedQueryError> {
        saved::save(&self.shared, text, target, database_kind)
    }

    pub fn saved_query_update(&self, id: SavedQueryId, edit: SavedQueryEdit) -> Result<SavedQueries, SavedQueryError> {
        saved::update(&self.shared, id, edit)
    }

    pub fn saved_query_delete(&self, id: SavedQueryId) -> Result<SavedQueries, SavedQueryError> {
        saved::delete(&self.shared, id)
    }

    pub fn saved_query_used(&self, id: SavedQueryId) -> Result<SavedQueries, SavedQueryError> {
        saved::used(&self.shared, id)
    }

    pub fn save_workspace(&self, mut w: Workspace) {
        self.shared.resolve_workspace(&mut w);
        if let Err(e) = self.shared.store.kv_set(KV_WORKSPACE, &w) {
            eprintln!("workspace save failed: {e}");
        }
    }

    pub fn settings(&self) -> Settings {
        self.shared.settings()
    }

    pub fn set_settings(&self, s: Settings) {
        let mut access = self.shared.access.lock();
        if access.mode != s.assistant_access {
            *access = access::AccessLease::new(s.assistant_access);
            if let Err(e) = self.shared.store.kv_set("assistant_access", &*access) {
                eprintln!("assistant access save failed: {e}");
            }
            self.shared.access.changed(*access);
            self.shared.revoke_assistant_access();
        }
        if let Err(e) = self.shared.store.kv_set(KV_SETTINGS, &s) {
            eprintln!("settings save failed: {e}");
        }
        let before = std::mem::replace(&mut *self.shared.settings.write().unwrap(), s.clone());
        drop(access);
        self.shared.reprobe_changed(&before.providers, &s.providers);
    }

    /// The chat with every entry, the running turn's as of now. An unknown id opens a draft with
    /// default options, held in memory until its first message.
    pub async fn assistant_open(&self, chat: ChatId) -> Result<ChatView, ChatError> {
        self.shared.assistant_open(chat)
    }

    /// Validates the model and effort against the provider's last probe. Applies at the next turn.
    pub fn assistant_configure(&self, chat: ChatId, options: ChatOptions) -> Result<ChatState, ChatError> {
        self.shared.assistant_configure(chat, options)
    }

    /// Stores the user entry and starts the turn. `Busy` while a turn runs.
    pub async fn assistant_send(&self, chat: ChatId, request: SendRequest) -> Result<TurnAccepted, SendError> {
        self.shared.assistant_send(chat, request).await
    }

    pub fn assistant_stop(&self, chat: ChatId) {
        self.shared.assistant_stop(chat)
    }

    pub fn assistant_approve(
        &self,
        chat: ChatId,
        approval: ApprovalId,
        decision: Decision,
    ) -> Result<(), ApproveError> {
        self.shared.assistant_approve(chat, approval, decision)
    }

    pub async fn assistant_delete(&self, chat: ChatId) {
        self.shared.assistant_delete(chat).await
    }

    /// Full probe of every enabled provider. Fire and forget.
    pub fn provider_refresh(&self) {
        self.shared.probe_all(ProbeDepth::Full);
    }

    /// Asks the Claude CLI for the subscription limits left, bounded by 8 s.
    /// The child sits in a process group that quit kills.
    pub async fn usage_limits(&self) -> LimitsReport {
        self.shared.read_limits().await
    }

    /// Starts a check now, fire and forget. It does nothing while a check runs, while a release is
    /// held, or while one downloads or installs.
    pub fn update_check(&self) {
        self.shared.update_check()
    }

    /// Starts downloading the release the snapshot offers. `Stale` unless the snapshot offers
    /// `version` or already holds it.
    pub fn update_download(&self, version: String) -> Result<(), UpdateError> {
        self.shared.update_download(&version)
    }

    /// Answers `Confirm` with what a restart would end, until `accepted` covers it. Then it stops
    /// the assistant's turns, installs the verified download, and asks the shell to relaunch.
    pub async fn update_install(
        &self,
        version: String,
        accepted: Option<RestartImpact>,
    ) -> Result<InstallReply, UpdateError> {
        self.shared.update_install(&version, accepted).await
    }

    /// Saves the track and checks it at once. `Busy` while a download or an install runs.
    pub fn update_set_track(&self, track: UpdateTrack) -> Result<(), UpdateError> {
        self.shared.update_set_track(track)
    }

    /// Stops every assistant turn, then kills any agent process still alive. The shell calls it
    /// on quit. It returns within `SHUTDOWN_BOUND`, so the shell's own bound must be larger.
    pub async fn shutdown(&self) {
        self.shared.shutdown().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn engine() -> (Arc<Engine>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::open(EngineConfig {
            data_dir: dir.path().join("nested"),
            keys: Arc::new(MemoryKeyStore::default()),
            agent_path: AgentPath::Fixed("".into()),
            az_binary: None,
            user_known_hosts: None,
            updates: Arc::new(crate::ScriptedTransport::named("off").unwrap()),
        })
        .await
        .unwrap();
        (engine, dir)
    }

    fn draft(name: &str) -> ConnectionDraft {
        ConnectionDraft {
            database_kind: crate::model::DatabaseKind::Postgres,
            id: None,
            name: name.into(),
            host: "localhost".into(),
            port: 5433,
            database: "shop".into(),
            user: "l1data".into(),
            ssl_mode: SslMode::Prefer,
            ca_cert_path: None,
            password: PasswordInput::Set("pw".into()),
            color: None,
            ssh: None,
            environment: None,
            sql_server_auth: None,
        }
    }

    #[tokio::test]
    async fn boot_counts_the_saved_connections_it_cannot_read() {
        let (engine, _dir) = engine().await;
        let kept = engine.save_connection(draft("kept")).await.unwrap();
        engine.shared.store.with_saved(|c, _| {
            c.execute(r#"INSERT INTO connections (id, json) VALUES ('newer', '{"sslMode":"VerifyPinned"}')"#, [])
                .unwrap()
        });
        let boot = engine.boot();
        assert_eq!(
            (boot.connections.iter().map(|c| c.id).collect::<Vec<_>>(), boot.unreadable_connections),
            (vec![kept.id], 1)
        );
    }

    #[tokio::test]
    async fn a_sign_in_method_from_a_newer_build_loads_but_never_connects_or_saves() {
        let (engine, _dir) = engine().await;
        let login = ConnectionDraft {
            database_kind: DatabaseKind::SqlServer,
            port: 1,
            password: PasswordInput::Clear,
            sql_server_auth: Some(SqlServerAuth::SqlLogin),
            ..draft("newer")
        };
        let saved = engine.save_connection(login.clone()).await.unwrap();
        engine
            .shared
            .store
            .upsert_connection(&ConnectionConfig { sql_server_auth: Some(SqlServerAuth::Unknown), ..saved.clone() })
            .unwrap();
        assert_eq!(
            engine.boot().connections.iter().map(|c| c.sql_server_auth).collect::<Vec<_>>(),
            [Some(SqlServerAuth::Unknown)]
        );
        let Err(ConnectError::Auth { message }) = engine.connect(saved.id).await else {
            panic!("a newer method must not connect as another one")
        };
        assert!(message.contains("newer version of L1 Data"), "{message}");
        let newer = ConnectionDraft { id: Some(saved.id), sql_server_auth: Some(SqlServerAuth::Unknown), ..login };
        let Err(ConnectError::Auth { message }) = engine.test_connection(newer.clone()).await else {
            panic!("a newer method must not test as another one")
        };
        assert!(message.contains("newer version of L1 Data"), "{message}");
        let Err(SaveError::Invalid { message }) = engine.save_connection(newer.clone()).await else {
            panic!("a newer method must not be rewritten")
        };
        assert!(message.contains("newer version of L1 Data"), "{message}");
        let Err(SaveError::Invalid { message }) =
            engine.save_connection(ConnectionDraft { sql_server_auth: None, ..newer.clone() }).await
        else {
            panic!("a save that names no method must not replace a newer one")
        };
        assert!(message.contains("newer version of L1 Data"), "{message}");
        let chosen = engine
            .save_connection(ConnectionDraft { sql_server_auth: Some(SqlServerAuth::SqlLogin), ..newer })
            .await
            .unwrap();
        assert_eq!(chosen.sql_server_auth, Some(SqlServerAuth::SqlLogin), "naming a method replaces the newer one");
    }

    #[tokio::test]
    async fn open_creates_data_dir_and_boots_empty() {
        let (engine, _dir) = engine().await;
        assert!(engine.data_dir().is_dir());
        let boot = engine.boot();
        assert!(boot.connections.is_empty() && boot.consoles.is_empty());
        assert_eq!(boot.settings.max_rows, 200_000);
    }

    #[tokio::test]
    async fn postgres_header_sort_does_not_require_a_connection_or_catalog() {
        let (engine, _dir) = engine().await;
        let config =
            engine.save_connection(ConnectionDraft { password: PasswordInput::Clear, ..draft("sort") }).await.unwrap();
        let db = DatabaseRef { connection_id: config.id, database: "shop".into() };
        let inspected = engine.table_order(db.clone(), "public", "orders", "ID DESC", TableOrderEdit::Inspect).unwrap();
        assert_eq!(inspected.sql, "ID DESC");
        let keys = inspected.keys.unwrap();
        assert_eq!((keys[0].column.as_str(), keys[0].desc), ("id", true));
        let sorted = engine
            .table_order(
                db.clone(),
                "public",
                "orders",
                "ID DESC",
                TableOrderEdit::Cycle { column: "id".into(), additive: true },
            )
            .unwrap();
        assert!(sorted.sql.is_empty() && sorted.keys.unwrap().is_empty());
        let advanced = "lower(name) DESC";
        let sorted = engine
            .table_order(
                db,
                "public",
                "orders",
                advanced,
                TableOrderEdit::Cycle { column: "id".into(), additive: false },
            )
            .unwrap();
        assert_eq!(sorted.sql, advanced);
        assert!(sorted.keys.is_none());
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn sql_server_header_sort_reports_an_unloaded_catalog_without_starting_work() {
        let (engine, _dir) = engine().await;
        let config = engine
            .save_connection(ConnectionDraft {
                database_kind: DatabaseKind::SqlServer,
                password: PasswordInput::Clear,
                ..draft("sort")
            })
            .await
            .unwrap();
        let db = DatabaseRef { connection_id: config.id, database: "shop".into() };
        let inspected = engine.table_order(db.clone(), "dbo", "orders", "ID DESC", TableOrderEdit::Inspect).unwrap();
        assert_eq!(inspected.sql, "ID DESC");
        assert!(inspected.keys.is_none());
        let sort_engine = engine.clone();
        let error = std::thread::spawn(move || {
            sort_engine.table_order(
                db,
                "dbo",
                "orders",
                "",
                TableOrderEdit::Cycle { column: "id".into(), additive: false },
            )
        })
        .join()
        .unwrap()
        .unwrap_err();
        assert!(error.contains("Connect") && error.contains("refresh") && error.contains("catalog"), "{error}");
        assert!(engine.boot().live.is_empty());
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn boot_lists_stored_chats_newest_first_as_idle() {
        let (engine, _dir) = engine().await;
        let options = ChatOptions {
            provider: ProviderKind::Claude,
            model: ModelId("opus".into()),
            effort: None,
            permission: Permission::Auto,
        };
        let record = |title: &str, updated_at_ms| store::ChatRecord {
            title: title.into(),
            created_at_ms: 1,
            updated_at_ms,
            options: options.clone(),
            session: None,
            entry_count: 4,
            access: None,
            recap_floor: 0,
        };
        let (old, new) = (ChatId(uuid::Uuid::from_u128(1)), ChatId(uuid::Uuid::from_u128(2)));
        engine.shared.store.put_chat(old, &record("old", 10)).unwrap();
        engine.shared.store.put_chat(new, &record("new", 20)).unwrap();
        let state = |id, title: &str, updated_at_ms| ChatState {
            id,
            rev: 0,
            title: title.into(),
            updated_at_ms,
            options: options.clone(),
            status: ChatStatus::Idle,
            entry_count: 4,
        };
        assert_eq!(engine.boot().chats, vec![state(new, "new", 20), state(old, "old", 10)]);
    }

    #[tokio::test]
    async fn a_notebook_opens_and_saves_through_the_engine_in_the_files_own_line_breaks() {
        let (engine, dir) = engine().await;
        let path = dir.path().join("q.sql");
        std::fs::write(&path, "select 1;\r\n").unwrap();
        let opened = engine.notebook_open(path.to_string_lossy().into_owned()).await.unwrap();
        assert_eq!((opened.name.as_str(), opened.notebook.plain, opened.notebook.cells.len()), ("q.sql", true, 1));
        assert_eq!(opened.notebook.cells[0].text, "select 1;\n");
        let saved = engine
            .notebook_save(
                SaveTarget::Bound { binding: opened.binding.clone(), overwrite: None },
                opened.notebook.clone(),
            )
            .await
            .unwrap();
        assert_eq!((std::fs::read(&path).unwrap(), saved.plain), (b"select 1;\r\n".to_vec(), true));

        let mut two = opened.notebook.clone();
        two.header = NotebookHeader {
            connection: Some("local".into()),
            kind: Some(DatabaseKind::Postgres),
            database: Some("shop".into()),
            unknown: vec![],
        };
        two.cells
            .push(NotebookCell { text: "select 2;".into(), meta: CellMeta { manual: true, ..CellMeta::default() } });
        let saved = engine
            .notebook_save(SaveTarget::Bound { binding: saved.binding, overwrite: None }, two.clone())
            .await
            .unwrap();
        let disk = String::from_utf8(std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            disk,
            "-- l1 notebook: connection=\"local\" kind=\"postgres\" database=\"shop\"\r\n-- %%\r\nselect 1;\r\n\r\n-- %% mode=\"manual\"\r\nselect 2;\r\n"
        );
        assert!(!saved.plain);
        let reopened = engine.notebook_open(saved.binding.path.clone()).await.unwrap();
        assert_eq!(
            (reopened.notebook, reopened.binding),
            (NotebookText { plain: false, ..two.clone() }, saved.binding.clone())
        );

        let stale = engine.notebook_save(SaveTarget::Bound { binding: opened.binding, overwrite: None }, two).await;
        assert!(matches!(stale, Err(FileWriteError::Changed { .. })), "{stale:?}");
        engine.shutdown().await;
    }

    const PLAINTEXT: &str = "plaintext-hunter2";

    fn db_bytes(dir: &Path) -> Vec<u8> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.file_name().unwrap().to_string_lossy().starts_with(DB_FILE))
            .flat_map(|p| std::fs::read(p).unwrap())
            .collect()
    }

    struct NoKeychain;

    impl KeyStore for NoKeychain {
        fn load(&self) -> Result<Option<[u8; 32]>, SecretError> {
            Err(SecretError("keychain touched".into()))
        }

        fn save(&self, _: &[u8; 32]) -> Result<(), SecretError> {
            Err(SecretError("keychain touched".into()))
        }
    }

    #[tokio::test]
    async fn passwordless_save_clear_and_delete_never_touch_the_keychain() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::open(EngineConfig {
            data_dir: dir.path().to_owned(),
            keys: Arc::new(NoKeychain),
            agent_path: AgentPath::Fixed("".into()),
            az_binary: None,
            user_known_hosts: None,
            updates: Arc::new(crate::ScriptedTransport::named("off").unwrap()),
        })
        .await
        .unwrap();
        let saved =
            engine.save_connection(ConnectionDraft { password: PasswordInput::Clear, ..draft("dev") }).await.unwrap();
        let kept = engine
            .save_connection(ConnectionDraft { id: Some(saved.id), password: PasswordInput::Keep, ..draft("dev") })
            .await
            .unwrap();
        assert!(!kept.has_password);
        engine.delete_connection(saved.id).await.unwrap();
        let err = engine.save_connection(draft("dev")).await.unwrap_err();
        assert!(matches!(err, SaveError::Keychain { ref message } if message == "keychain touched"));
    }

    #[tokio::test]
    async fn testing_an_entra_draft_never_reads_the_saved_password() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = |keys: Arc<dyn KeyStore>| EngineConfig {
            data_dir: dir.path().to_owned(),
            keys,
            agent_path: AgentPath::Fixed("".into()),
            az_binary: None,
            user_known_hosts: None,
            updates: Arc::new(crate::ScriptedTransport::named("off").unwrap()),
        };
        let login = ConnectionDraft {
            database_kind: DatabaseKind::SqlServer,
            port: 1,
            ssl_mode: SslMode::VerifyFull,
            sql_server_auth: Some(SqlServerAuth::SqlLogin),
            ..draft("azure")
        };
        let saved = {
            let engine = Engine::open(cfg(Arc::new(MemoryKeyStore::default()))).await.unwrap();
            let saved = engine.save_connection(login.clone()).await.unwrap();
            engine.shutdown().await;
            saved
        };
        assert!(saved.has_password);
        let engine = Engine::open(cfg(Arc::new(NoKeychain))).await.unwrap();
        let entra = ConnectionDraft {
            id: Some(saved.id),
            password: PasswordInput::Keep,
            sql_server_auth: Some(SqlServerAuth::EntraAzureCli),
            ..login
        };
        let tried = engine.test_connection(entra).await;
        assert!(matches!(tried, Err(ConnectError::Network { .. })), "the keychain was read: {tried:?}");
    }

    #[tokio::test]
    async fn settings_a_newer_build_saved_load_as_the_defaults_and_stay_saved() {
        let (engine, dir) = engine().await;
        let newer = serde_json::json!({ "colorScheme": "Sepia", "maxRows": 5 });
        engine.shared.store.kv_set(KV_SETTINGS, &newer).unwrap();
        engine.shutdown().await;
        drop(engine);
        let config = EngineConfig {
            data_dir: dir.path().join("nested"),
            keys: Arc::new(MemoryKeyStore::default()),
            agent_path: AgentPath::Fixed("".into()),
            az_binary: None,
            user_known_hosts: None,
            updates: Arc::new(crate::ScriptedTransport::named("off").unwrap()),
        };
        let reopened = Engine::open(config).await.unwrap();
        assert_eq!(reopened.boot().settings.max_rows, Settings::default().max_rows);
        assert_eq!(reopened.shared.store.kv_get::<serde_json::Value>(KV_SETTINGS).unwrap(), Some(newer));
    }

    #[tokio::test]
    async fn connections_settings_and_workspace_persist_across_open() {
        let dir = tempfile::tempdir().unwrap();
        let keys = Arc::new(MemoryKeyStore::default());
        let cfg = || EngineConfig {
            data_dir: dir.path().to_owned(),
            keys: keys.clone(),
            agent_path: AgentPath::Fixed("".into()),
            az_binary: None,
            user_known_hosts: None,
            updates: Arc::new(crate::ScriptedTransport::named("off").unwrap()),
        };
        let password = |engine: &Engine, id| engine.shared.saved_secret(id, SecretSlot::Database).unwrap();
        let saved = {
            let engine = Engine::open(cfg()).await.unwrap();
            let saved = engine
                .save_connection(ConnectionDraft {
                    password: PasswordInput::Set(PLAINTEXT.into()),
                    environment: Some(Environment::Production),
                    ..draft("dev")
                })
                .await
                .unwrap();
            assert!(saved.has_password);
            assert_eq!(password(&engine, saved.id).as_deref(), Some(PLAINTEXT));
            engine.set_settings(Settings { max_rows: 10, ..Settings::default() });
            engine.save_workspace(Workspace {
                layout: Layout { sidebar: false, ..Layout::default() },
                ..Workspace::default()
            });
            saved
        };
        let engine = Engine::open(cfg()).await.unwrap();
        let boot = engine.boot();
        assert_eq!(boot.connections.len(), 1);
        assert_eq!(boot.connections[0].id, saved.id);
        assert_eq!(boot.connections[0].environment, Some(Environment::Production));
        assert_eq!(boot.settings.max_rows, 10);
        assert!(!boot.workspace.layout.sidebar);
        assert_eq!(password(&engine, saved.id).as_deref(), Some(PLAINTEXT));
        assert!(!db_bytes(dir.path()).windows(PLAINTEXT.len()).any(|w| w == PLAINTEXT.as_bytes()));

        let renamed = engine
            .save_connection(ConnectionDraft {
                id: Some(saved.id),
                name: "renamed".into(),
                password: PasswordInput::Keep,
                ..draft("x")
            })
            .await
            .unwrap();
        assert!(renamed.has_password && renamed.name == "renamed");
        assert_eq!(renamed.environment, None, "the draft's environment replaces the saved one");
        assert_eq!(password(&engine, saved.id).as_deref(), Some(PLAINTEXT));
        let cleared = engine
            .save_connection(ConnectionDraft { id: Some(saved.id), password: PasswordInput::Clear, ..draft("x") })
            .await
            .unwrap();
        assert!(!cleared.has_password);
        assert_eq!(engine.shared.store.secret(saved.id, SecretSlot::Database).unwrap(), None);
        assert_eq!(password(&engine, saved.id), None);
        assert_eq!(engine.boot().connections.len(), 1);

        engine.delete_connection(saved.id).await.unwrap();
        assert!(engine.boot().connections.is_empty());
    }

    #[tokio::test]
    async fn the_ssh_secret_follows_the_save_rules() {
        let (engine, _dir) = engine().await;
        let tunnel = |auth: SshAuth, secret| {
            Some(SshTunnelDraft { host: " bastion ".into(), port: 22, user: "tunnel ".into(), auth, secret })
        };
        let set = |s: &str| PasswordInput::Set(s.into());
        let saved = engine
            .save_connection(ConnectionDraft { ssh: tunnel(SshAuth::Password, set("pw")), ..draft("t") })
            .await
            .unwrap();
        let id = saved.id;
        let secret = || engine.shared.saved_secret(id, SecretSlot::Ssh).unwrap();
        let save = |ssh| {
            engine.save_connection(ConnectionDraft { id: Some(id), password: PasswordInput::Keep, ssh, ..draft("t") })
        };
        assert_eq!(
            saved.ssh,
            Some(SshTunnel {
                host: "bastion".into(),
                port: 22,
                user: "tunnel".into(),
                auth: SshAuth::Password,
                has_secret: true
            })
        );
        assert_eq!(secret().as_deref(), Some("pw"));

        assert!(save(tunnel(SshAuth::Password, PasswordInput::Keep)).await.unwrap().ssh.unwrap().has_secret);
        assert_eq!(secret().as_deref(), Some("pw"), "Keep keeps the secret while the auth kind is unchanged");

        let key = SshAuth::Key { path: " ~/.ssh/id_ed25519 ".into() };
        let switched = save(tunnel(key.clone(), PasswordInput::Keep)).await.unwrap().ssh.unwrap();
        assert_eq!((switched.auth, switched.has_secret), (SshAuth::Key { path: "~/.ssh/id_ed25519".into() }, false));
        assert_eq!(secret(), None, "a password is never kept as a passphrase");

        assert!(save(tunnel(key, set("l1data"))).await.unwrap().ssh.unwrap().has_secret);
        let agent =
            save(tunnel(SshAuth::Agent { socket: Some(" ".into()) }, set("ignored"))).await.unwrap().ssh.unwrap();
        assert_eq!((agent.auth, agent.has_secret), (SshAuth::Agent { socket: None }, false));
        assert_eq!(secret(), None, "agent auth holds no secret");

        save(tunnel(SshAuth::Password, set("pw2"))).await.unwrap();
        assert!(!save(tunnel(SshAuth::Password, PasswordInput::Clear)).await.unwrap().ssh.unwrap().has_secret);
        assert_eq!(secret(), None);

        save(tunnel(SshAuth::Password, set("pw3"))).await.unwrap();
        assert_eq!(save(None).await.unwrap().ssh, None);
        assert_eq!(secret(), None, "removing the tunnel deletes its secret");
        assert_eq!(
            engine.shared.saved_secret(id, SecretSlot::Database).unwrap().as_deref(),
            Some("pw"),
            "the database password is untouched"
        );

        save(tunnel(SshAuth::Password, set("pw4"))).await.unwrap();
        engine.delete_connection(id).await.unwrap();
        assert_eq!(engine.shared.store.secret(id, SecretSlot::Ssh).unwrap(), None);
    }

    #[tokio::test]
    async fn an_invalid_tunnel_is_rejected_before_any_secret_is_sealed() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::open(EngineConfig {
            data_dir: dir.path().to_owned(),
            keys: Arc::new(NoKeychain),
            agent_path: AgentPath::Fixed("".into()),
            az_binary: None,
            user_known_hosts: None,
            updates: Arc::new(crate::ScriptedTransport::named("off").unwrap()),
        })
        .await
        .unwrap();
        let tunnel = SshTunnelDraft {
            host: "bastion".into(),
            port: 22,
            user: "tunnel".into(),
            auth: SshAuth::Password,
            secret: PasswordInput::Set("pw".into()),
        };
        for bad in [
            SshTunnelDraft { host: " ".into(), ..tunnel.clone() },
            SshTunnelDraft { port: 0, ..tunnel.clone() },
            SshTunnelDraft { user: "".into(), ..tunnel.clone() },
            SshTunnelDraft { auth: SshAuth::Key { path: " ".into() }, ..tunnel.clone() },
        ] {
            let err =
                engine.save_connection(ConnectionDraft { ssh: Some(bad.clone()), ..draft("x") }).await.unwrap_err();
            assert!(matches!(err, SaveError::Invalid { .. }), "{bad:?}: {err:?}");
        }
        assert!(engine.boot().connections.is_empty());
    }

    #[tokio::test]
    async fn invalid_drafts_are_rejected() {
        let (engine, _dir) = engine().await;
        let err = engine.save_connection(ConnectionDraft { name: "  ".into(), ..draft("x") }).await.unwrap_err();
        assert!(matches!(err, SaveError::Invalid { .. }));
        let err = engine.save_connection(ConnectionDraft { port: 0, ..draft("x") }).await.unwrap_err();
        assert!(matches!(err, SaveError::Invalid { .. }));
        assert!(engine.boot().connections.is_empty());
    }

    #[tokio::test]
    async fn format_edits_are_in_utf16_units_past_non_ascii_text() {
        let (engine, _dir) = engine().await;
        assert_eq!(
            engine.editor_format(
                "select 'café 😀';\nselect a,b from t;\n",
                RunTarget::Cursor { offset: DocOffset(20) },
                DatabaseKind::Postgres
            ),
            Ok(FormatEdit { from: DocOffset(18), to: DocOffset(35), insert: "SELECT a,\n  b\nFROM t".into() })
        );
        assert_eq!(
            engine.editor_format(
                "-- café 😀\nselect a,b from t",
                RunTarget::Cursor { offset: DocOffset(13) },
                DatabaseKind::Postgres
            ),
            Ok(FormatEdit {
                from: DocOffset(0),
                to: DocOffset(28),
                insert: "-- café 😀\nSELECT a,\n  b\nFROM t".into()
            }),
            "a leading comment is part of its statement"
        );
        assert_eq!(
            engine.editor_format(
                "select '漢字';\nselect 'é', x from t; select 2",
                RunTarget::Cursor { offset: DocOffset(15) },
                DatabaseKind::Postgres
            ),
            Ok(FormatEdit { from: DocOffset(13), to: DocOffset(33), insert: "SELECT 'é',\n  x\nFROM t".into() })
        );
    }

    #[tokio::test]
    async fn format_covers_the_statements_a_selection_touches_and_keeps_the_text_between() {
        let (engine, _dir) = engine().await;
        let text = "select 1;\nselect a,b from t;";
        let both =
            FormatEdit { from: DocOffset(0), to: DocOffset(27), insert: "SELECT 1;\nSELECT a,\n  b\nFROM t".into() };
        assert_eq!(
            engine.editor_format(
                text,
                RunTarget::Selection { from: DocOffset(3), to: DocOffset(12) },
                DatabaseKind::Postgres
            ),
            Ok(both.clone())
        );
        assert_eq!(engine.editor_format(text, RunTarget::All, DatabaseKind::Postgres), Ok(both));
        assert_eq!(
            engine.editor_format(
                text,
                RunTarget::Selection { from: DocOffset(8), to: DocOffset(10) },
                DatabaseKind::Postgres
            ),
            Err(FormatError::NothingToFormat),
            "only the semicolon and the newline"
        );
        assert_eq!(
            engine.editor_format(
                "select 1;\n\nselect 2",
                RunTarget::Cursor { offset: DocOffset(10) },
                DatabaseKind::Postgres
            ),
            Err(FormatError::NothingToFormat)
        );
    }

    #[tokio::test]
    async fn console_requires_a_known_connection_and_results_are_gone_without_a_run() {
        let (engine, _dir) = engine().await;
        let tab = TabId(uuid::Uuid::new_v4());
        let unknown = ConsoleConfig {
            connection_id: ConnectionId(uuid::Uuid::new_v4()),
            database: "shop".into(),
            schema: None,
            mode: TxnMode::AutoCommit,
        };
        assert!(matches!(engine.console_configure(tab, unknown).await, Err(ConfigureError::UnknownConnection)));
        let r = ResultRef { tab_id: tab, run_id: RunId(1), index: 0 };
        assert!(matches!(engine.result_page(r, 0, 10), Err(ResultError::Gone)));
        assert!(matches!(engine.result_find(r, "x".into(), ResultFindDirection::First).await, Err(ResultError::Gone)));
        assert!(matches!(
            engine
                .console_run(
                    tab,
                    RunRequest::Script {
                        text: "select 1".into(),
                        target: RunTarget::All,
                        mode: RunMode::Run,
                        params: Vec::new(),
                        cells: Vec::new()
                    },
                    None
                )
                .await,
            Err(RunError::NotConfigured)
        ));
        assert!(matches!(engine.console_commit(tab, None).await, Err(TxnError::NoTransaction)));
    }

    #[tokio::test]
    async fn mssql_manual_configuration_refuses_without_creating_or_changing_a_console() {
        let (engine, _dir) = engine().await;
        let connection = engine
            .save_connection(ConnectionDraft { database_kind: DatabaseKind::SqlServer, ..draft("mssql") })
            .await
            .unwrap();
        let tab = TabId(uuid::Uuid::new_v4());
        let config = |mode| ConsoleConfig { connection_id: connection.id, database: "shop".into(), schema: None, mode };
        let refused = engine.console_configure(tab, config(TxnMode::Manual)).await;
        assert!(
            matches!(refused, Err(ConfigureError::Unsupported { ref what }) if what.contains("Select Auto-commit")),
            "{refused:?}"
        );
        assert!(engine.boot().consoles.is_empty());
        let before = engine.console_configure(tab, config(TxnMode::AutoCommit)).await.unwrap();
        let refused = engine.console_configure(tab, config(TxnMode::Manual)).await;
        assert!(matches!(refused, Err(ConfigureError::Unsupported { .. })));
        assert_eq!(serde_json::to_value(&engine.boot().consoles[0]).unwrap(), serde_json::to_value(before).unwrap());
        assert!(engine.shared.instance(connection.id).is_none());
        let pg = engine.save_connection(draft("postgres")).await.unwrap();
        let pg_config =
            ConsoleConfig { connection_id: pg.id, database: "shop".into(), schema: None, mode: TxnMode::Manual };
        assert_eq!(engine.console_configure(tab, pg_config).await.unwrap().config.mode, TxnMode::Manual);
    }

    #[tokio::test]
    async fn restored_mssql_manual_tabs_keep_their_mode_and_refuse_before_network_work() {
        let (engine, _dir) = engine().await;
        let refuser = Refuser::start();
        let connection = engine
            .save_connection(ConnectionDraft {
                database_kind: DatabaseKind::SqlServer,
                host: "127.0.0.1".into(),
                port: refuser.port,
                ..draft("legacy")
            })
            .await
            .unwrap();
        let tab = TabId(uuid::Uuid::new_v4());
        engine.save_workspace(Workspace {
            tabs: vec![TabSnapshot {
                id: tab,
                title: "Saved Manual".into(),
                text: Some("UPDATE dbo.target SET n=2".into()),
                table: None,
                file: None,
                collapsed: false,
                console: ConsoleConfigSnapshot {
                    connection_id: Some(connection.id),
                    database: Some("shop".into()),
                    schema: None,
                    mode: TxnMode::Manual,
                },
            }],
            active: Some(tab),
            ..Workspace::default()
        });
        let saved: Workspace = serde_json::from_value(serde_json::to_value(engine.boot().workspace).unwrap()).unwrap();
        assert_eq!(saved.tabs[0].console.mode, TxnMode::Manual);
        let config = ConsoleConfig {
            connection_id: connection.id,
            database: "shop".into(),
            schema: None,
            mode: saved.tabs[0].console.mode,
        };
        assert!(matches!(engine.console_configure(tab, config).await, Err(ConfigureError::Unsupported { .. })));
        let legacy = Arc::new(Console::new(
            tab,
            ConsoleConfigView {
                connection_id: connection.id,
                database: "shop".into(),
                schema: None,
                mode: TxnMode::Manual,
            },
        ));
        engine.shared.consoles.write().unwrap().insert(tab, legacy.clone());
        let before = serde_json::to_value(legacy.snapshot(&*legacy.inner.lock().await)).unwrap();
        for request in [
            RunRequest::Script {
                text: "UPDATE dbo.target SET n=2".into(),
                target: RunTarget::All,
                mode: RunMode::Run,
                params: Vec::new(),
                cells: Vec::new(),
            },
            RunRequest::Script {
                text: "UPDATE dbo.target SET n=2\nGO\nSELECT 1".into(),
                target: RunTarget::All,
                mode: RunMode::Run,
                params: Vec::new(),
                cells: Vec::new(),
            },
            RunRequest::TablePage {
                schema: "dbo".into(),
                table: "target".into(),
                page: 0,
                query: TableQuery::default(),
            },
        ] {
            let refused = engine.console_run(tab, request, None).await;
            assert!(
                matches!(refused, Err(RunError::Unsupported { ref what }) if what.contains("Select Auto-commit")),
                "{refused:?}"
            );
            assert_eq!(serde_json::to_value(legacy.snapshot(&*legacy.inner.lock().await)).unwrap(), before);
        }
        let edit = TableChanges {
            schema: "missing".into(),
            table: "target".into(),
            inserts: vec![],
            updates: vec![],
            deletes: vec![],
        };
        assert!(matches!(engine.table_apply(tab, edit.clone(), None).await, Err(ApplyError::Unsupported { .. })));
        engine
            .console_configure(
                tab,
                ConsoleConfig {
                    connection_id: connection.id,
                    database: "shop".into(),
                    schema: None,
                    mode: TxnMode::AutoCommit,
                },
            )
            .await
            .unwrap();
        assert!(matches!(engine.table_apply(tab, edit, None).await, Err(ApplyError::Unsupported { .. })));
        assert_eq!(refuser.accepted.load(Ordering::SeqCst), 0);
        assert!(engine.shared.instance(connection.id).is_none());
        assert!(matches!(legacy.inner.lock().await.session, SessionSlot::Empty));
        assert_eq!(engine.boot().workspace.tabs[0].console.mode, TxnMode::Manual);
    }

    #[tokio::test]
    async fn mssql_count_revalidates_the_tab_after_awaiting_its_catalog() {
        for change in ["target", "close", "disconnect", "legacy mode"] {
            let (engine, _dir) = engine().await;
            let refuser = Refuser::start();
            let config = engine
                .save_connection(ConnectionDraft {
                    database_kind: DatabaseKind::SqlServer,
                    host: "127.0.0.1".into(),
                    port: refuser.port,
                    ..draft("count setup race")
                })
                .await
                .unwrap();
            let db = DatabaseRef { connection_id: config.id, database: config.database.clone() };
            let catalog = Arc::new(Catalog {
                database_kind: DatabaseKind::SqlServer,
                db: db.clone(),
                version: 1,
                default_schema: Some("dbo".into()),
                schemas: Vec::new(),
                extensions: Vec::new(),
            });
            let home = DbSlot::new(db.clone());
            home.set(CatalogSlot::Ready(catalog.clone()));
            let instance = Arc::new(Instance::new(
                config.clone(),
                ServerInfo { version: "count fixture".into(), version_num: 0, encoding: "UTF8".into() },
                db.database.clone(),
                vec![connection::DatabaseInfo { name: db.database.clone(), system: false, access: DbAccess::Ok }],
                ConnectionView::initial(),
                home,
            ));
            engine.shared.set_connection(config.id, ConnSlot::Connected(instance.clone()));
            let tab = TabId(uuid::Uuid::new_v4());
            let c = ConsoleConfig {
                connection_id: config.id,
                database: db.database.clone(),
                schema: None,
                mode: TxnMode::AutoCommit,
            };
            engine.console_configure(tab, c.clone()).await.unwrap();
            let slot = instance.slot(&db.database).unwrap();
            let meta = slot.meta.lock().await;
            slot.set(CatalogSlot::Opening);
            let query = TableQuery {
                filters: vec![ColumnFilter { column: "id".into(), op: FilterOp::Eq, value: "1".into() }],
                ..Default::default()
            };
            let mut count = Box::pin(engine.exact_row_count(tab, db, "dbo".into(), "unused_count_table".into(), query));
            assert!(futures_util::poll!(count.as_mut()).is_pending(), "count must wait for the catalog");
            match change {
                "target" => {
                    engine
                        .console_configure(tab, ConsoleConfig { database: "unconfigured_count_target".into(), ..c })
                        .await
                        .unwrap();
                }
                "close" => {
                    engine.console_close(tab, true).await.unwrap();
                }
                "disconnect" => {
                    engine.disconnect(config.id, true).await.unwrap();
                }
                "legacy mode" => {
                    engine.shared.console(tab).unwrap().inner.lock().await.config.mode = TxnMode::Manual;
                }
                _ => unreachable!(),
            }
            slot.set(CatalogSlot::Ready(catalog));
            drop(meta);
            let result = tokio::time::timeout(Duration::from_secs(5), count).await.unwrap();
            match change {
                "disconnect" => assert!(matches!(result, Err(CountError::NotConnected)), "{result:?}"),
                "legacy mode" => assert!(
                    matches!(result, Err(CountError::Failed { ref message }) if message.contains("native Auto-commit")),
                    "{result:?}"
                ),
                _ => assert!(
                    matches!(result, Err(CountError::Failed { ref message }) if message.contains("target changed")),
                    "{result:?}"
                ),
            }
            assert_eq!(refuser.accepted.load(Ordering::SeqCst), 0, "stale count must not open a native session");
            engine.shutdown().await;
        }
    }

    #[cfg(feature = "mssql-tests")]
    #[tokio::test]
    async fn legacy_mssql_manual_requests_never_reach_a_committing_trigger() {
        use futures_util::FutureExt;
        use std::panic::AssertUnwindSafe;

        async fn rows(engine: &Engine, tab: TabId, text: String) -> Vec<Vec<Option<String>>> {
            let accepted = engine
                .console_run(
                    tab,
                    RunRequest::Script {
                        text,
                        target: RunTarget::All,
                        mode: RunMode::Run,
                        params: Vec::new(),
                        cells: Vec::new(),
                    },
                    None,
                )
                .await
                .unwrap();
            let state = tokio::time::timeout(Duration::from_secs(15), async {
                loop {
                    let state = engine.boot().consoles.into_iter().find(|s| s.tab_id == tab).unwrap();
                    if matches!(state.run, RunState::Finished { run_id, .. } if run_id == accepted.run_id) {
                        break state;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            assert!(matches!(state.run, RunState::Finished { outcome: RunOutcome::Ok, .. }), "{:?}", state.run);
            let RunState::Finished { statements, .. } = state.run else { unreachable!() };
            statements
                .iter()
                .flat_map(|s| &s.outputs)
                .filter_map(|o| match o {
                    RunOutput::Rows { result, .. } => Some(engine.result_page(result.r#ref, 0, 100).unwrap().rows),
                    _ => None,
                })
                .flatten()
                .collect()
        }

        let (engine, _dir) = engine().await;
        let connection = engine
            .save_connection(ConnectionDraft {
                database_kind: DatabaseKind::SqlServer,
                host: std::env::var("L1_DATA_TEST_MSSQL_HOST").unwrap_or_else(|_| "localhost".into()),
                port: std::env::var("L1_DATA_TEST_MSSQL_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(1434),
                user: "sa".into(),
                ssl_mode: SslMode::Require,
                password: PasswordInput::Set(
                    std::env::var("L1_DATA_TEST_MSSQL_PASSWORD").unwrap_or_else(|_| "L1Data_dev_Str0ng".into()),
                ),
                ..draft("Legacy Manual trigger refusal")
            })
            .await
            .unwrap();
        let tab = TabId(uuid::Uuid::new_v4());
        let observer = TabId(uuid::Uuid::new_v4());
        for tab in [tab, observer] {
            engine
                .console_configure(
                    tab,
                    ConsoleConfig {
                        connection_id: connection.id,
                        database: "shop".into(),
                        schema: None,
                        mode: TxnMode::AutoCommit,
                    },
                )
                .await
                .unwrap();
        }
        let name = format!("l1_legacy_refusal_{}", uuid::Uuid::new_v4().simple());
        let audit = format!("{name}_audit");
        let trigger = format!("{name}_trigger");
        let sequence = format!("{name}_sequence");
        let outcome = AssertUnwindSafe(async {
            rows(&engine, observer, format!("CREATE TABLE dbo.[{name}](id int PRIMARY KEY,n int); INSERT INTO dbo.[{name}] VALUES(1,1); CREATE TABLE dbo.[{audit}](n int); INSERT INTO dbo.[{audit}] VALUES(1); CREATE SEQUENCE dbo.[{sequence}] AS bigint START WITH 1")).await;
            rows(&engine, observer, format!("CREATE TRIGGER dbo.[{trigger}] ON dbo.[{name}] AFTER UPDATE AS BEGIN SET NOCOUNT ON; DECLARE @attempt bigint = NEXT VALUE FOR dbo.[{sequence}]; WHILE @@TRANCOUNT > 0 COMMIT TRANSACTION; BEGIN TRANSACTION; UPDATE dbo.[{audit}] SET n=99; END")).await;
            assert_eq!(rows(&engine, observer, format!("SELECT NEXT VALUE FOR dbo.[{sequence}]")).await, vec![vec![Some("1".into())]]);
            rows(&engine, tab, "SELECT @@TRANCOUNT".into()).await;
            let console = engine.shared.console(tab).unwrap();
            let before = {
                let mut inner = console.inner.lock().await;
                inner.config.mode = TxnMode::Manual;
                serde_json::to_value(console.publish(&mut inner, &engine.shared)).unwrap()
            };
            for request in [
                RunRequest::Script { text: format!("UPDATE dbo.[{name}] SET n=2 WHERE id=1"), target: RunTarget::All, mode: RunMode::Run, params: Vec::new(), cells: Vec::new() },
                RunRequest::Script { text: format!("UPDATE dbo.[{name}] SET n=2 WHERE id=1\nGO\nSELECT 1"), target: RunTarget::All, mode: RunMode::Run, params: Vec::new(), cells: Vec::new() },
                RunRequest::TablePage { schema: "dbo".into(), table: name.clone(), page: 0, query: TableQuery::default() },
            ] {
                assert!(matches!(engine.console_run(tab, request, None).await, Err(RunError::Unsupported { .. })));
                assert_eq!(serde_json::to_value(console.snapshot(&*console.inner.lock().await)).unwrap(), before);
            }
            let edit = TableChanges {
                schema: "dbo".into(), table: name.clone(), inserts: vec![], deletes: vec![],
                updates: vec![RowUpdate { key: vec![CellWrite { column: "id".into(), value: Some("1".into()) }], set: vec![CellWrite { column: "n".into(), value: Some("3".into()) }] }],
            };
            assert!(matches!(engine.table_apply(tab, edit, None).await, Err(ApplyError::Unsupported { .. })));
            assert_eq!(serde_json::to_value(console.snapshot(&*console.inner.lock().await)).unwrap(), before);
            assert_eq!(rows(&engine, observer, format!("SELECT n FROM dbo.[{name}]; SELECT n FROM dbo.[{audit}]; SELECT NEXT VALUE FOR dbo.[{sequence}]")).await,
                vec![vec![Some("1".into())], vec![Some("1".into())], vec![Some("2".into())]]);
            engine.console_configure(tab, ConsoleConfig { connection_id: connection.id, database: "shop".into(), schema: None, mode: TxnMode::AutoCommit }).await.unwrap();
            assert_eq!(rows(&engine, tab, "SELECT @@TRANCOUNT; SELECT XACT_STATE()".into()).await, vec![vec![Some("0".into())], vec![Some("0".into())]]);
        }).catch_unwind().await;
        let _ = engine.console_rollback(tab).await;
        engine.console_close(tab, true).await.unwrap();
        rows(&engine, observer, format!("DROP TABLE IF EXISTS dbo.[{name}]; DROP TABLE IF EXISTS dbo.[{audit}]; DROP SEQUENCE IF EXISTS dbo.[{sequence}]")).await;
        assert_eq!(rows(&engine, observer, format!("SELECT COUNT_BIG(*) FROM sys.objects WHERE name IN (N'{name}',N'{audit}',N'{trigger}',N'{sequence}')")).await, vec![vec![Some("0".into())]]);
        engine.shutdown().await;
        if let Err(panic) = outcome {
            std::panic::resume_unwind(panic);
        }
    }

    #[cfg(feature = "pg-tests")]
    #[tokio::test]
    async fn a_task_that_panics_holding_the_session_fails_its_run_and_frees_the_tab() {
        async fn finished(engine: &Engine, tab: TabId, run_id: RunId) -> ConsoleState {
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    let state = engine.boot().consoles.into_iter().find(|s| s.tab_id == tab).unwrap();
                    if matches!(state.run, RunState::Finished { run_id: r, .. } if r == run_id) {
                        break state;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("the run ends")
        }
        let select = |n: u8| RunRequest::Script {
            text: format!("select {n}"),
            target: RunTarget::All,
            mode: RunMode::Run,
            params: Vec::new(),
            cells: Vec::new(),
        };

        let (engine, _dir) = engine().await;
        let url =
            std::env::var("L1_DATA_TEST_PG").unwrap_or_else(|_| "postgres://l1data:l1data@localhost:5433/shop".into());
        let saved = engine.save_connection(draft_from_url(&url, "panic").unwrap()).await.unwrap();
        let tab = TabId(uuid::Uuid::new_v4());
        engine
            .console_configure(
                tab,
                ConsoleConfig {
                    connection_id: saved.id,
                    database: saved.database.clone(),
                    schema: None,
                    mode: TxnMode::AutoCommit,
                },
            )
            .await
            .unwrap();
        let opened = engine.console_run(tab, select(1), None).await.unwrap();
        finished(&engine, tab, opened.run_id).await;

        let s = &engine.shared;
        let console = s.console(tab).unwrap();
        let run_id = s.next_run_id();
        let session = {
            let mut inner = console.inner.lock().await;
            let SessionSlot::Idle(idle) = &inner.session else { panic!("the first run left its session idle") };
            let target = writes::Target {
                saved: &saved,
                opened: idle.opened(),
                console: &inner.config,
                txn: inner.txn,
                unasked: &inner.unasked,
            };
            let checked = writes::check(&target, &["select 1"], None).unwrap();
            let (session, _stop) = console.take_session(&mut inner, run_id, Work::Run(checked)).unwrap();
            let range = DocRange { from: DocOffset(0), to: DocOffset(0) };
            inner.run = RunState::Running {
                run_id,
                started_at_ms: console::now_ms(),
                statements: [StmtStatus::Running, StmtStatus::Pending]
                    .into_iter()
                    .zip(0..)
                    .map(|(status, index)| StatementRun { index, range, status, outputs: Vec::new() })
                    .collect(),
            };
            console.publish(&mut inner, s);
            session
        };
        s.spawn_holding(console.clone(), run_id, async move {
            let _held = session;
            panic!("boom");
        });

        let state = finished(&engine, tab, run_id).await;
        let RunState::Finished { outcome: RunOutcome::Failed, statements, .. } = &state.run else {
            panic!("{:?}", state.run)
        };
        assert!(
            matches!(&statements[0].status, StmtStatus::Failed { error } if error.message.ends_with(": boom")),
            "{:?}",
            statements[0].status
        );
        assert!(matches!(statements[1].status, StmtStatus::Skipped), "{:?}", statements[1].status);
        assert!(matches!(state.session, SessionState::Lost { .. }) && state.work.is_none(), "{state:?}");

        let next = engine.console_run(tab, select(2), None).await.expect("the tab accepts another run");
        let state = finished(&engine, tab, next.run_id).await;
        assert!(matches!(state.run, RunState::Finished { outcome: RunOutcome::Ok, .. }), "{:?}", state.run);
        assert!(matches!(state.session, SessionState::Open { .. }), "{:?}", state.session);
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn a_view_edit_while_disconnected_is_refused() {
        let (engine, _dir) = engine().await;
        let config = engine.save_connection(draft("x")).await.unwrap();
        let edit = ViewEdit::Database { name: "reporting".into(), shown: true };
        assert!(matches!(engine.edit_view(config.id, edit).await, Err(CatalogError::NotConnected)));
        assert!(engine.shared.store.view(config.id).unwrap().is_none(), "nothing was persisted");
    }

    /// A listener that accepts and drops every connection, so a session open fails at once. The
    /// count tells whether a task reached the network.
    struct Refuser {
        port: u16,
        accepted: Arc<AtomicU64>,
    }

    impl Refuser {
        fn start() -> Self {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let accepted = Arc::new(AtomicU64::new(0));
            let count = accepted.clone();
            std::thread::spawn(move || {
                for conn in listener.incoming() {
                    drop(conn);
                    count.fetch_add(1, Ordering::SeqCst);
                }
            });
            Self { port, accepted }
        }
    }

    /// A connected instance whose home catalog is Ready and whose other databases point at
    /// `refuser`, without a server: every network path these tests take fails fast.
    async fn fake_connected(
        engine: &Arc<Engine>,
        refuser: &Refuser,
        databases: &[&str],
    ) -> (ConnectionId, Arc<Instance>) {
        let config = engine
            .save_connection(ConnectionDraft {
                host: "127.0.0.1".into(),
                port: refuser.port,
                password: PasswordInput::Clear,
                ..draft("fake")
            })
            .await
            .unwrap();
        let home = config.database.clone();
        let db = DatabaseRef { connection_id: config.id, database: home.clone() };
        let slot = DbSlot::new(db.clone());
        slot.set(CatalogSlot::Ready(Arc::new(Catalog {
            database_kind: crate::model::DatabaseKind::Postgres,
            db,
            version: 1,
            default_schema: Some("public".into()),
            schemas: Vec::new(),
            extensions: Vec::new(),
        })));
        let list = std::iter::once(home.as_str())
            .chain(databases.iter().copied())
            .map(|name| connection::DatabaseInfo { name: name.into(), system: false, access: DbAccess::Ok })
            .collect();
        let server = ServerInfo { version: "16.0".into(), version_num: 160000, encoding: "UTF8".into() };
        let instance = Arc::new(Instance::new(config.clone(), server, home, list, ConnectionView::initial(), slot));
        engine.shared.set_connection(config.id, ConnSlot::Connected(instance.clone()));
        (config.id, instance)
    }

    fn probe_of(instance: &Instance, name: &str) -> Option<SchemaCount> {
        instance.probes.lock().unwrap().get(name).cloned()
    }

    async fn wait_until(what: &str, done: impl Fn() -> bool) {
        let started = std::time::Instant::now();
        while !done() {
            assert!(started.elapsed() < Duration::from_secs(5), "{what}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Tauri runs a sync command inline on the main thread, outside its tokio runtime. The
    /// engine's fire-and-forget entry points must work from there.
    #[tokio::test]
    async fn probe_databases_works_from_a_thread_without_a_runtime() {
        let (engine, _dir) = engine().await;
        let refuser = Refuser::start();
        let (id, instance) = fake_connected(&engine, &refuser, &["x"]).await;
        let caller = engine.clone();
        let joined = std::thread::spawn(move || caller.probe_databases(id, vec!["x".into()])).join();
        assert!(joined.is_ok(), "probe_databases panicked off the runtime");
        wait_until("the probe never finished", || {
            matches!(probe_of(&instance, "x"), Some(SchemaCount::Unavailable { .. }))
        })
        .await;
        assert_eq!(refuser.accepted.load(Ordering::SeqCst), 1);
    }

    /// Probe bookkeeping and the connection snapshot take the instance's locks from opposite
    /// ends unless both follow the documented order. Both paths hammer the same instance; a
    /// deadlock shows as a timeout.
    #[tokio::test]
    async fn probes_and_snapshots_never_deadlock() {
        let (engine, _dir) = engine().await;
        let refuser = Refuser::start();
        let (id, _instance) = fake_connected(&engine, &refuser, &["x"]).await;
        let deadline = std::time::Instant::now() + Duration::from_millis(500);
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        for worker in 0..2 {
            let (engine, done_tx) = (engine.clone(), done_tx.clone());
            std::thread::spawn(move || {
                while std::time::Instant::now() < deadline {
                    if worker == 0 {
                        engine.probe_databases(id, vec!["x".into()]);
                    } else {
                        engine.shared.republish_connection(id);
                    }
                }
                done_tx.send(()).unwrap();
            });
        }
        drop(done_tx);
        for _ in 0..2 {
            assert!(done_rx.recv_timeout(Duration::from_secs(5)).is_ok(), "a worker is deadlocked");
        }
    }

    /// Up to four probes run at once; the rest wait for a permit. A disconnect while they wait
    /// must not let them open sessions afterwards.
    #[tokio::test]
    async fn probes_queued_behind_the_gate_open_no_session_after_a_disconnect() {
        let (engine, _dir) = engine().await;
        let refuser = Refuser::start();
        let (id, instance) = fake_connected(&engine, &refuser, &["x"]).await;
        let held = instance.probe_gate.acquire_many(connection::PROBE_PERMITS as u32).await.unwrap();
        engine.probe_databases(id, vec!["x".into()]);
        assert_eq!(probe_of(&instance, "x"), Some(SchemaCount::Probing));
        engine.disconnect(id, false).await.unwrap();
        drop(held);
        wait_until("the queued probe never took its permit", || {
            instance.probe_gate.available_permits() == connection::PROBE_PERMITS
        })
        .await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(refuser.accepted.load(Ordering::SeqCst), 0, "a probe connected after the disconnect");
    }

    /// Another thread's reconcile that read the view before an edit must not undo the edit's
    /// slot once the edit's own reconcile has run.
    #[tokio::test]
    async fn a_concurrent_reconcile_cannot_undo_a_shown_database() {
        let (engine, _dir) = engine().await;
        let refuser = Refuser::start();
        let (id, instance) = fake_connected(&engine, &refuser, &["x"]).await;
        let deadline = std::time::Instant::now() + Duration::from_millis(500);
        let churn = {
            let engine = engine.clone();
            std::thread::spawn(move || {
                while std::time::Instant::now() < deadline {
                    engine.shared.reconcile(id);
                }
            })
        };
        let known = Known { home: &instance.home, databases: &[], catalogs: &HashMap::new() };
        let shown = ConnectionView::initial().apply(&ViewEdit::Database { name: "x".into(), shown: true }, &known);
        let hidden = ConnectionView::initial();
        let mut lost = 0;
        while std::time::Instant::now() < deadline {
            for (view, want_slot) in [(&shown, true), (&hidden, false)] {
                *instance.view.write().unwrap() = view.clone();
                engine.shared.reconcile(id);
                if instance.slot("x").is_some() != want_slot {
                    lost += 1;
                }
            }
        }
        churn.join().unwrap();
        assert_eq!(lost, 0, "a stale reconcile undid an edit {lost} times");
    }
}
