use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use specta::Type;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
pub struct ConnectionId(pub Uuid);

/// Minted by the frontend (`crypto.randomUUID()`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
pub struct TabId(pub Uuid);

/// Engine-wide monotonic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Type)]
pub struct RunId(pub u64);

/// UTF-16 code-unit offset into the editor document as sent in `RunRequest::Script.text`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct DocOffset(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct DocRange {
    pub from: DocOffset,
    pub to: DocOffset,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
pub enum DatabaseKind {
    #[default]
    Postgres,
    SqlServer,
}

impl<'de> Deserialize<'de> for ConnectionConfig {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Stored {
            #[serde(default)]
            database_kind: DatabaseKind,
            id: ConnectionId,
            name: String,
            host: String,
            port: u16,
            database: String,
            user: String,
            ssl_mode: SslMode,
            ca_cert_path: Option<String>,
            has_password: bool,
            color: Option<String>,
            #[serde(default)]
            ssh: Option<SshTunnel>,
            #[serde(default)]
            environment: Option<Environment>,
            #[serde(default)]
            sql_server_auth: Option<SqlServerAuth>,
        }
        let Stored { database_kind, id, name, host, port, database, user, ssl_mode, ca_cert_path, has_password, color, ssh, environment, sql_server_auth } = Stored::deserialize(d)?;
        Ok(Self { database_kind, id, name, host, port, database, user, ssl_mode, ca_cert_path, has_password, color, ssh, environment, sql_server_auth })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
pub enum SslMode {
    Disable,
    Prefer,
    Require,
    VerifyCa,
    VerifyFull,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionConfig {
    pub database_kind: DatabaseKind,
    pub id: ConnectionId,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub database: String,
    pub user: String,
    pub ssl_mode: SslMode,
    pub ca_cert_path: Option<String>,
    pub has_password: bool,
    pub color: Option<String>,
    /// Sessions reach the database through this SSH server. `host` and `port` above are then
    /// resolved and dialed by that server.
    pub ssh: Option<SshTunnel>,
    pub environment: Option<Environment>,
    /// `None` is a SQL login, as on every connection saved before this field existed.
    pub sql_server_auth: Option<SqlServerAuth>,
}

/// How a SQL Server connection signs in. `EntraAzureCli` sends an access token from the user's
/// `az login` and saves no password. `Unknown` is a method a newer build saved. Connect and save
/// refuse it, so the row keeps its method until the user picks one this build knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
pub enum SqlServerAuth {
    SqlLogin,
    EntraAzureCli,
    Unknown,
}

impl<'de> Deserialize<'de> for SqlServerAuth {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(match serde_json::Value::deserialize(d)?.as_str() {
            Some("SqlLogin") => Self::SqlLogin,
            Some("EntraAzureCli") => Self::EntraAzureCli,
            _ => Self::Unknown,
        })
    }
}

/// What the user says the connection points at. The UI marks `Production`, and the engine asks before a write runs on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
pub enum Environment {
    Local,
    Staging,
    Production,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SshTunnel {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: SshAuth,
    /// A password for `Password`, or a key passphrase for `Key`, is sealed in the vault.
    pub has_secret: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum SshAuth {
    Password,
    /// An OpenSSH private key file, read at each dial. A leading `~` is the home directory.
    Key { path: String },
    /// `None` is the agent at `SSH_AUTH_SOCK`, or the OpenSSH agent pipe on Windows.
    Agent { socket: Option<String> },
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SshTunnelDraft {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: SshAuth,
    /// The password or key passphrase. `Keep` keeps the saved one only while the auth kind is
    /// unchanged, so a password is never tried as a passphrase.
    pub secret: PasswordInput,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub enum PasswordInput {
    Keep,
    Set(String),
    Clear,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionDraft {
    pub database_kind: DatabaseKind,
    /// `None` creates a new connection.
    pub id: Option<ConnectionId>,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub database: String,
    pub user: String,
    pub ssl_mode: SslMode,
    pub ca_cert_path: Option<String>,
    pub password: PasswordInput,
    pub color: Option<String>,
    pub ssh: Option<SshTunnelDraft>,
    pub environment: Option<Environment>,
    pub sql_server_auth: Option<SqlServerAuth>,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    /// `"16.4"`
    pub version: String,
    pub version_num: u32,
    pub encoding: String,
}

/// One database on one saved connection. Postgres catalogs and OIDs are per database, so every
/// catalog lookup, refresh, and count takes one of these. `database` is `pg_database.datname`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseRef {
    pub connection_id: ConnectionId,
    pub database: String,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum ConnStatus {
    Disconnected,
    Connecting,
    /// The home session is open and `pg_database` was read. Catalogs load after this, each with
    /// its own `CatalogStatus`, so one unreachable database never fails the instance.
    Connected { server: ServerInfo, instance: InstanceState },
    Failed { error: ConnectError },
    /// The home database's metadata session died after being `Connected`.
    Lost { reason: String },
}

/// Every database of the instance, with the state of its catalog. The connection snapshot is the
/// single source of catalog versions.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct InstanceState {
    pub configured_database: String,
    /// `ConnectionConfig.database` as the server spells it.
    pub home: String,
    /// The database picker lists system databases only while this is on.
    pub show_system_databases: bool,
    /// Every row of `pg_database`, sorted by name.
    pub databases: Vec<DatabaseState>,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseState {
    pub name: String,
    /// `datistemplate OR NOT datallowconn`. `postgres` is an ordinary database.
    pub system: bool,
    pub access: DbAccess,
    /// Whether the tree shows this database, resolved from the stored view. Never true without
    /// `DbAccess::Ok`.
    pub shown: bool,
    /// The schema picker lists system schemas only while this is on.
    pub show_system_schemas: bool,
    /// The schemas the tree shows under this database, in name order. Empty until the catalog
    /// serves a version.
    pub shown_schemas: Vec<String>,
    /// Non-system schema count for the database picker: from the catalog when one is served,
    /// else from `connection_probe_databases`.
    pub schema_count: SchemaCount,
    pub catalog: CatalogStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum SchemaCount {
    Unknown,
    Probing,
    Known { count: u32 },
    /// The probe session failed; `message` is the server's or the transport's text.
    Unavailable { message: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
pub enum DbAccess {
    Ok,
    /// `datallowconn` is false.
    NoConnections,
    /// `has_database_privilege(datname, 'CONNECT')` is false.
    NoPrivilege,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum CatalogStatus {
    /// No metadata session is wanted for this database.
    Idle,
    /// The first load is in flight. A reload keeps `Ready` with the old version.
    Loading,
    /// `version` comes from one engine-wide counter, so it rises across every database and every
    /// reconnect. The UI fetches `catalog_get` when it is newer than what it holds.
    Ready { version: u64 },
    /// The last load failed. `version` is the last good catalog's, which `catalog_get` still
    /// serves, so the UI keeps it until a reload succeeds.
    Failed { message: String, version: Option<u64> },
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionState {
    pub id: ConnectionId,
    pub rev: u64,
    pub status: ConnStatus,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum ConnectError {
    Network { message: String },
    Tls { message: String },
    Auth { message: String },
    MissingPassword,
    Server(QueryError),
    Keychain { message: String },
    /// The SSH tunnel failed before the database was reached. The variants above are the database's.
    Ssh { error: SshError },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum SshError {
    Network { message: String },
    /// No known_hosts file knows this server. `trust` names the key the engine saw, for
    /// `ssh_trust_host_key`; it expires after ten minutes.
    HostKeyUnknown { host: String, port: u16, algorithm: String, fingerprint: String, trust: HostKeyId },
    /// No known_hosts file holds this key, but one holds another key of its algorithm for the server, at `file:line`.
    HostKeyChanged { host: String, port: u16, algorithm: String, fingerprint: String, known_at: String },
    /// The key is on an `@revoked` line, at `file:line`.
    HostKeyRevoked { host: String, port: u16, algorithm: String, fingerprint: String, revoked_at: String },
    /// A known_hosts file exists but cannot be read or parsed, so the key cannot be checked.
    KnownHosts { message: String },
    Auth { message: String },
    /// Password auth with no saved password, or an encrypted key with no passphrase.
    MissingSecret,
    Key { message: String },
    Agent { message: String },
    /// The SSH server refused or failed to open the connection to the database.
    Forward { message: String },
}

/// Engine-wide monotonic. Names one host key the engine observed and is holding for a trust decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
pub struct HostKeyId(pub u64);

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct Catalog {
    pub database_kind: DatabaseKind,
    pub db: DatabaseRef,
    pub version: u64,
    /// The first existing non-system entry of `current_schemas(false)` on this database's
    /// metadata session, else `public` when it exists, else the first non-system schema by name.
    /// `None` only when the database has no non-system schema. "Default only" in the schema
    /// picker means this.
    pub default_schema: Option<String>,
    /// Every schema except `pg_toast` and temp schemas, in name order. A system schema carries
    /// its relations and types only while the tree shows it, and never its functions, sequences,
    /// or synonyms.
    pub schemas: Vec<SchemaInfo>,
    /// By extension name. Empty on SQL Server, and no query runs there.
    pub extensions: Vec<ExtensionInfo>,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SchemaInfo {
    pub name: String,
    /// `name` as SQL writes it, quoted only when it must be.
    pub quoted: String,
    /// `information_schema` or a `pg_` prefix, which Postgres reserves for its own schemas.
    pub system: bool,
    /// Tables, partitioned tables, and foreign tables, which the tree lists as tables. Counted by
    /// the schema query itself, so it is right for a system schema whose `relations` are not loaded.
    pub table_count: u32,
    pub relations: Vec<Relation>,
    pub functions: Vec<FunctionInfo>,
    /// User schemas only, by name. Includes identity and serial sequences.
    pub sequences: Vec<SequenceInfo>,
    /// Enums, domains, composites, and ranges in every loaded schema.
    /// Base and pseudo types too, when this schema's relations loaded.
    pub types: Vec<TypeInfo>,
    /// SQL Server only, by name. Empty on PostgreSQL, and no query runs there.
    pub synonyms: Vec<SynonymInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
pub enum RelKind {
    Table,
    PartitionedTable,
    View,
    MaterializedView,
    ForeignTable,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct Relation {
    pub qualified_name: String,
    pub oid: u32,
    pub name: String,
    /// `name` as SQL writes it, quoted only when it must be.
    pub quoted: String,
    pub kind: RelKind,
    /// `None` when `reltuples = -1` (never analyzed).
    pub row_estimate: Option<i64>,
    /// In `attnum` order.
    pub columns: Vec<ColumnInfo>,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ColumnInfo {
    pub writable: bool,
    pub attnum: i16,
    pub name: String,
    /// `name` as SQL writes it, quoted only when it must be.
    pub quoted: String,
    pub type_display: String,
    pub category: TypeCategory,
    pub not_null: bool,
    pub pk: bool,
    /// Part of any foreign key.
    pub fk: bool,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct FunctionInfo {
    pub oid: u32,
    pub name: String,
    /// `name` as a call writes it, quoted only when it must be.
    pub quoted: String,
    pub args: String,
    pub returns: String,
    pub kind: FnKind,
    pub insert_text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
pub enum FnKind {
    Function,
    Procedure,
    Aggregate,
    Window,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SequenceInfo {
    pub oid: u32,
    pub name: String,
    pub quoted: String,
    pub type_name: String,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TypeInfo {
    pub oid: u32,
    pub name: String,
    pub quoted: String,
    pub kind: TypeKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TypeField {
    pub name: String,
    pub type_name: String,
}

/// The facts a type row draws. PostgreSQL `typtype` letters stay in the loader.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum TypeKind {
    Enum {
        labels: Vec<String>,
    },
    Domain {
        base: String,
    },
    Composite {
        fields: Vec<TypeField>,
    },
    Range {
        subtype: String,
    },
    /// PostgreSQL `typtype` `b` or `p` in a system schema whose relations loaded.
    Builtin,
    /// SQL Server `CREATE TYPE name FROM ...`. `base` is `display()` of the system type.
    Alias {
        base: String,
        nullable: bool,
    },
    Table,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SynonymInfo {
    pub oid: u32,
    pub name: String,
    pub quoted: String,
    /// `sys.synonyms.base_object_name`, as stored. The row's detail.
    pub target: String,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionInfo {
    pub oid: u32,
    pub name: String,
    pub quoted: String,
    pub version: String,
}

/// What a table row shows past its columns, read by `relation_detail` when the row first opens.
/// It is not in `Catalog`, so a DDL refresh never scans constraints, indexes, and triggers.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct RelationDetail {
    pub db: DatabaseRef,
    pub oid: u32,
    /// The catalog version the detail is at least as new as.
    pub catalog_version: u64,
    /// In `attnum` order.
    pub defaults: Vec<ColumnDefault>,
    /// Primary, then foreign, unique, and check, each by name.
    pub keys: Vec<KeyInfo>,
    /// Primary, then unique, then the rest, each by name.
    pub indexes: Vec<IndexInfo>,
    /// By name.
    pub triggers: Vec<TriggerInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct MapCursor {
    pub db: DatabaseRef,
    pub root: u32,
    pub catalog_version: u64,
    pub last_constraint: u32,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct MapRelation {
    pub oid: u32,
    pub schema: String,
    pub name: String,
    pub columns: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct MapColumnPair {
    pub source: String,
    pub target: String,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct MapForeignKey {
    pub id: u32,
    pub name: String,
    pub source: u32,
    pub target: u32,
    pub columns: Vec<MapColumnPair>,
    pub enabled: bool,
    pub validated: bool,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct MapNeighborhood {
    pub db: DatabaseRef,
    pub catalog_version: u64,
    pub root: MapRelation,
    pub relations: Vec<MapRelation>,
    pub foreign_keys: Vec<MapForeignKey>,
    pub after: Option<MapCursor>,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum MapError {
    Catalog { error: CatalogError },
    Gone,
    Stale,
    Failed { message: String },
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum MapResolution {
    CatalogMatch { oid: u32 },
    Tentative { oid: u32 },
    Unresolved { reason: String },
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct MapOccurrence {
    pub id: u32,
    pub name: String,
    pub alias: Option<String>,
    pub range: DocRange,
    pub resolution: MapResolution,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct MapQueryJoin {
    pub source: u32,
    pub target: u32,
    pub columns: MapColumnPair,
    pub range: DocRange,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct MapQuery {
    pub db: DatabaseRef,
    pub catalog_version: u64,
    pub statement: Option<DocRange>,
    pub occurrences: Vec<MapOccurrence>,
    pub joins: Vec<MapQueryJoin>,
    pub incomplete: bool,
}

/// An object the tree can script. `oid` is the catalog id of a relation, routine, sequence, type,
/// synonym, or extension. An index or trigger is named within its relation, as `RelationDetail` lists it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum ScriptTarget {
    Relation { oid: u32 },
    Routine { oid: u32 },
    Index { relation_oid: u32, name: String },
    Trigger { relation_oid: u32, name: String },
    Sequence { oid: u32 },
    Type { oid: u32 },
    Synonym { oid: u32 },
    Extension { oid: u32 },
}

/// The DDL that creates one object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ObjectScript {
    /// The tab title: the object's name and `.sql`.
    pub title: String,
    /// Starts with one `-- L1 Data could not script: ...` line per warning.
    pub text: String,
    /// The properties of the object that `text` does not recreate.
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ColumnDefault {
    pub column: String,
    pub value: DefaultValue,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum DefaultValue {
    /// `pg_get_expr` of the default, with the cast dropped from a lone literal: `'pending'`, `now()`.
    Expr { sql: String },
    /// `GENERATED ALWAYS` (`always`) or `BY DEFAULT AS IDENTITY`.
    Identity { always: bool },
    /// `GENERATED ALWAYS AS (sql) STORED`, or `VIRTUAL` on PG 18.
    Generated { sql: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct KeyInfo {
    pub name: String,
    /// In key order.
    pub columns: Vec<String>,
    pub kind: KeyKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum KeyKind {
    Primary,
    Unique,
    Foreign { ref_schema: String, ref_table: String, ref_columns: Vec<String>, on_delete: FkAction, on_update: FkAction },
    /// `definition` is `pg_get_constraintdef`: `CHECK ((qty > 0))`.
    Check { definition: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
pub enum FkAction {
    NoAction,
    Restrict,
    Cascade,
    SetNull,
    SetDefault,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct IndexInfo {
    pub name: String,
    /// `pg_am.amname`: btree, hash, gin, gist, brin.
    pub method: String,
    pub unique: bool,
    pub primary: bool,
    /// False while `CREATE INDEX CONCURRENTLY` builds it or after that failed.
    pub valid: bool,
    /// The key list cut from `pg_get_indexdef`: `(created_at DESC)`, `(a) INCLUDE (b)`.
    pub keys: String,
    /// The predicate of a partial index, without redundant parentheses: `status <> 'paid'::text`.
    pub predicate: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TriggerInfo {
    pub name: String,
    pub timing: TriggerTiming,
    /// In INSERT, UPDATE, DELETE, TRUNCATE order.
    pub events: Vec<TriggerEvent>,
    pub per_row: bool,
    pub enabled: bool,
    pub implementation: TriggerImplementation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum TriggerImplementation {
    Function { name: String },
    SqlModule { definition: Option<String> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
pub enum TriggerTiming {
    Before,
    After,
    InsteadOf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
pub enum TriggerEvent {
    Insert,
    Update,
    Delete,
    Truncate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
pub enum TxnMode {
    AutoCommit,
    Manual,
}

#[derive(Debug, Clone, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ConsoleConfig {
    pub connection_id: ConnectionId,
    /// A change resets the session like a connection change does.
    pub database: String,
    pub schema: Option<String>,
    pub mode: TxnMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
/// `Aborted` permits only rollback. `Open` reports presence without known committability.
pub enum TxnState {
    Idle,
    Active,
    Aborted,
    Open,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum SessionState {
    None,
    Open { backend_pid: i32 },
    Lost { reason: String, transaction: TransactionLoss },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
pub enum TransactionLoss {
    None,
    RolledBack,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
pub enum ConsoleWork {
    Run,
    Count,
    Transaction,
    Edit,
    Probe,
    /// An `All` export streams the result's source again. Cancellable like a run.
    Export,
    Import,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ConsoleState {
    pub txn_depth: Option<u32>,
    pub tab_id: TabId,
    pub rev: u64,
    pub config: ConsoleConfigView,
    pub session: SessionState,
    pub work: Option<ConsoleWork>,
    /// `Some` exactly while `work` is `Some(Export)`: both derive from the one value the session slot holds.
    pub export: Option<ExportProgress>,
    pub import: Option<ImportProgress>,
    pub txn: TxnState,
    pub run: RunState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ExportProgress {
    pub result: ResultRef,
    pub format: ExportFormat,
    /// Rows encoded so far.
    pub rows: u64,
    /// Bytes handed to the file so far.
    pub bytes: u64,
    /// Writing failed while SQL Server is still sending. The export reads to the end so the session and
    /// its transaction survive; the file is not written. Esc stops it at once and resets the session.
    pub write_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ConsoleConfigView {
    pub connection_id: ConnectionId,
    pub database: String,
    pub schema: Option<String>,
    pub mode: TxnMode,
}

impl ConsoleConfigView {
    pub fn db(&self) -> DatabaseRef {
        DatabaseRef { connection_id: self.connection_id, database: self.database.clone() }
    }
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum RunState {
    Idle,
    Running {
        run_id: RunId,
        started_at_ms: i64,
        statements: Vec<StatementRun>,
    },
    Finished {
        run_id: RunId,
        elapsed_ms: u32,
        statements: Vec<StatementRun>,
        outcome: RunOutcome,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
pub enum RunOutcome {
    Ok,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct StatementRun {
    pub outputs: Vec<RunOutput>,
    pub index: u16,
    pub range: DocRange,
    pub status: StmtStatus,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum StmtStatus {
    Pending,
    Preparing,
    Running,
    Finished,
    Failed { error: QueryError },
    Cancelled,
    /// An earlier statement failed or was cancelled.
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
pub enum PlanFormat {
    Json,
    Xml,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum RunOutput {
    Rows { result: ResultMeta, state: RowsState },
    Command { tag: String, affected_rows: Option<u64>, elapsed_ms: u32 },
    Plan { result: ResultMeta, format: PlanFormat },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
pub enum RowsState { Streaming, Complete, Failed, Cancelled }

#[derive(Debug, Clone, Deserialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum TableOrderEdit { Inspect, Cycle { column: String, additive: bool } }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TableSortKey { pub column: String, pub desc: bool }

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TableOrderView { pub sql: String, pub keys: Option<Vec<TableSortKey>> }

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TableScript {
    pub text: String,
    /// Browse documents after trimming outer whitespace. Includes the canonical text.
    pub accepted_texts: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
pub enum RunMode {
    Run,
    Explain,
    ExplainAnalyze,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct Param {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ParamValue {
    pub name: String,
    pub value: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum RunRequest {
    Script { text: String, target: RunTarget, mode: RunMode, params: Vec<ParamValue> },
    TablePage {
        schema: String,
        table: String,
        page: u32,
        #[serde(default)]
        query: TableQuery,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TableQuery {
    /// SQL after `ORDER BY`, e.g. `"created_at" DESC`; empty means primary key order.
    pub order_by: String,
    pub filters: Vec<ColumnFilter>,
    /// A boolean SQL expression ANDed with `filters`; empty means none.
    pub r#where: String,
}

// Hand-written so saved tabs missing a field still load. `#[serde(default)]` would do it too, but specta
// would then mark every TableQuery field optional in the TS bindings.
impl<'de> Deserialize<'de> for TableQuery {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Default, Deserialize)]
        #[serde(default, rename_all = "camelCase")]
        struct Stored {
            order_by: String,
            filters: Vec<ColumnFilter>,
            r#where: String,
        }
        let Stored { order_by, filters, r#where } = Stored::deserialize(d)?;
        Ok(TableQuery { order_by, filters, r#where })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ColumnFilter {
    pub column: String,
    pub op: FilterOp,
    /// Compared as a Postgres literal; ignored by `IsNull` and `NotNull`.
    pub value: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
pub enum FilterOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Contains,
    StartsWith,
    IsNull,
    NotNull,
}

#[derive(Debug, Clone, Copy, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum RunTarget {
    Cursor { offset: DocOffset },
    Selection { from: DocOffset, to: DocOffset },
    All,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct RunAccepted {
    pub run_id: RunId,
    pub statements: Vec<DocRange>,
}

/// The statements of a script and the one a run at the cursor would pick.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ScriptOutline {
    pub unit_kind: ExecutionUnitKind,
    pub statements: Vec<DocRange>,
    pub at_cursor: Option<u16>,
}

/// What the editor may offer at a cursor, read from the statement under it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum CompletionContext {
    /// Inside a string, a comment, a quoted name, a number, or a parameter.
    None,
    Complete {
        /// The start of the word being typed, or the cursor when none is.
        from: DocOffset,
        expect: Expect,
        /// The names before a `.` that ends at `from`.
        qualifier: Option<Qualifier>,
        /// The FROM items of the query at the cursor, then those of each enclosing query.
        relations: Vec<ScopeRelation>,
        /// The CTE names in scope, as written.
        ctes: Vec<String>,
        /// The keywords that can come next, upper case, in the order to show them.
        keywords: Vec<String>,
        /// The dialect's built-in functions, when `expect` is `Expression` or `JoinCondition`.
        functions: Vec<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum Expect {
    /// Only `keywords`, which may be empty, as after a relation's alias.
    Keyword,
    /// Right after a relation, where the word typed is most likely its alias, so `keywords` show only on request.
    Alias,
    /// A table, view, or CTE: after `FROM`, `JOIN`, `INTO`, or `UPDATE`.
    Relation,
    /// Columns and functions: a select list, `WHERE`, `ON` once a condition has begun, `SET`.
    Expression,
    /// Right after the `ON` of the join to `relations[target]`.
    JoinCondition { target: u16 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum Qualifier {
    /// The alias or name of `relations[index]`.
    Relation { index: u16 },
    /// Names that match no relation in scope, such as a schema. Unquoted PostgreSQL names are folded to lower case.
    Name { parts: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ScopeRelation {
    /// How the statement refers to it, as written: its alias, else its name. `None` for a subquery without an alias.
    pub reference: Option<String>,
    pub source: RelationSource,
    /// The names a column alias list gives the first columns, as in `orders o(a, b)`, as SQL writes them.
    pub column_aliases: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum RelationSource {
    /// A catalog table or view. Unquoted PostgreSQL names are folded to lower case.
    Table { schema: Option<String>, name: String },
    /// A CTE, a subquery, or a table function: the columns its select list names, as SQL writes them, plus every
    /// column of each `star` table.
    Derived { columns: Vec<String>, star: Vec<TableName> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TableName {
    pub schema: Option<String>,
    pub name: String,
}

/// Replace `from..to` with `insert`: the formatted statements and the original text between them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct FormatEdit {
    pub from: DocOffset,
    pub to: DocOffset,
    pub insert: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
pub enum ExecutionUnitKind { Statement, Batch }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(tag = "kind")]
pub enum FormatError {
    Unsupported,
    /// No statement at the cursor or in the selection.
    NothingToFormat,
    /// The laid-out text would lex differently from the original, so the engine refused it.
    Unsafe,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ResultRef {
    pub tab_id: TabId,
    pub run_id: RunId,
    pub index: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ResultCell {
    pub row: u32,
    pub col: u16,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Type)]
#[serde(tag = "kind")]
pub enum ResultFindDirection {
    First,
    Next { anchor: ResultCell },
    Previous { anchor: ResultCell },
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ResultFindTarget {
    pub cell: ResultCell,
    pub ordinal: u64,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ResultFind {
    pub count: u64,
    pub target: Option<ResultFindTarget>,
    pub row_count: u32,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ResultMeta {
    pub database_kind: DatabaseKind,
    pub r#ref: ResultRef,
    /// The database the rows came from. Column sources are its OIDs, so PK lookups key on this,
    /// not on the tab's current target, which a retarget can change under an old result.
    pub db: DatabaseRef,
    pub columns: Vec<ColumnMeta>,
    /// Rows stored and pageable.
    pub row_count: u32,
    /// Rows the server sent; greater than `row_count` when truncated.
    pub rows_seen: u64,
    pub truncated: Option<TruncateReason>,
    pub elapsed_ms: u32,
    /// What `ExportScope::All` would run for this result. Decided once, when the result was created.
    pub all_rows: AllRows,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum AllRows {
    /// A table page: every row of the table under the page's filters and order.
    Table,
    /// A read statement, run again exactly as the run sent it.
    Statement,
    /// A PostgreSQL read that calls functions or operators L1 Data cannot vouch for. `READ ONLY` stops
    /// writes, but not a call that acts outside the transaction, so the user confirms before it runs again.
    Confirm { calls: Vec<String> },
    /// The statement may write, change the transaction, or reach another server, so it never runs twice.
    Refused { reason: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
pub enum TruncateReason {
    RowLimit,
    ByteLimit,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ColumnMeta {
    pub name: String,
    pub r#type: SqlTypeRef,
    pub source: Option<ColumnSource>,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SqlTypeRef {
    /// `"bigint"`, `"integer[]"`, `"order_status"`.
    pub display: String,
    pub category: TypeCategory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
pub enum TypeCategory {
    /// int2, int4, int8, oid.
    Integer,
    /// numeric, real, double precision, money.
    Numeric,
    Text,
    Bool,
    Temporal,
    Json,
    Binary,
    Uuid,
    Network,
    Array,
    Enum,
    Composite,
    Range,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ColumnSource {
    pub table_oid: u32,
    pub attnum: i16,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct RowPage {
    pub start: u32,
    pub rows: Vec<Vec<Option<String>>>,
    pub cut: Vec<(u32, u16)>,
}

/// Inclusive indexes into the stored rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct RowSpan {
    pub start: u32,
    pub end: u32,
}

/// Inclusive indexes into the stored columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ColSpan {
    pub start: u16,
    pub end: u16,
}

/// `count` includes NULL. `non_null` counts every `Some`, including `""`.
/// `Numeric` is present only when sum, avg, min, and max all exist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum CellAggregate {
    Counts { count: u64, non_null: u64 },
    Numeric { count: u64, non_null: u64, sum: String, avg: String, min: String, max: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ChartSeries {
    /// One label per point: the x column's text as the grid shows it (NULL is None),
    /// or the 1-based row number when no x column was given.
    pub labels: Vec<Option<String>>,
    /// The y column per point. None for NULL, or for text that is not a finite number.
    pub values: Vec<Option<f64>>,
    /// True when the result stores more rows than were read.
    pub capped: bool,
}

/// The chart point to open the rows behind. `Probe` asks only whether the result allows it.
#[derive(Debug, Clone, Deserialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum DrillPoint {
    Probe,
    /// A stored row of the result, which is the chart's point at that index. `title` names the
    /// result's tab in the comment the SQL starts with.
    Row { row: u32, title: String },
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum DrillError {
    Gone,
    OutOfRange,
    /// The result's SQL is not a `SELECT` with `FROM` and `GROUP BY`, so a point stands for no group of rows.
    Ungrouped,
    /// The result's SQL cannot be rewritten to read exactly the rows behind a point.
    Unsupported { reason: String },
}

/// What one stored column holds. Every stored row is read.
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ColumnProfile {
    /// Stored rows read.
    pub rows: u64,
    pub nulls: u64,
    /// Exact count of distinct non-null texts.
    pub distinct: u64,
    pub summary: ProfileSummary,
}

#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(tag = "kind")]
pub enum ProfileSummary {
    /// An integer or numeric column, not `money` or `smallmoney`, whose every non-null text parses.
    /// `min`, `mean`, and `max` are exact decimal text, as the selection totals print them.
    /// The histogram spans the finite values only, and is empty when there are none.
    Numeric { min: String, mean: String, max: String, histogram: Vec<HistogramBin> },
    /// Every other column: at most five of the most frequent non-null values, cut like a page cell.
    Values { top: Vec<ValueCount> },
}

/// Values from `lo` up to `hi`. The last bin also holds `hi`.
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
pub struct HistogramBin {
    pub lo: f64,
    pub hi: f64,
    pub count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
pub struct ValueCount {
    pub value: String,
    pub count: u64,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct QueryError {
    pub severity: String,
    pub sqlstate: Option<String>,
    pub message: String,
    pub detail: Option<String>,
    pub hint: Option<String>,
    pub context: Option<String>,
    pub position: Option<DocOffset>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
pub enum ExportFormat {
    /// RFC 4180 with CRLF and a header row, NULL as an empty field, as `COPY … CSV` writes it.
    Csv,
    /// An array of objects keyed by column name, one per line. Values are typed by `TypeCategory`.
    Json,
    /// A GitHub table.
    Markdown,
    /// One `INSERT` per row, quoted for the result's dialect.
    SqlInsert,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Type)]
pub enum ExportScope {
    /// The rows the engine holds for the result, capped by the row and size limits.
    Loaded,
    /// Every row: the result's source runs again on its tab's session and streams to the file.
    All,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ExportSummary {
    pub rows: u64,
    pub bytes: u64,
    /// The loaded rows stopped at the row or size limit. Always false for `All`.
    pub truncated: bool,
}

/// One line of a tab's output log: timestamped history of what the session ran and what the
/// server said. Statement outcomes also live in `ConsoleState.run`; that snapshot is the current
/// status, this log is the history. `run_id` is the run, commit, rollback, or apply that held the
/// session; run ids share one counter, so a later entry never carries a lower id.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct OutputEntry {
    pub at_ms: i64,
    pub run_id: RunId,
    pub body: OutputBody,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum OutputBody {
    /// Logged when the statement starts, before its notices and its result line.
    Statement {
        statement: u16,
        text: String,
    },
    Rows {
        statement: u16,
        rows: u64,
        elapsed_ms: u32,
        truncated: bool,
    },
    Command {
        statement: u16,
        tag: String,
        elapsed_ms: u32,
    },
    Notice {
        severity: String,
        message: String,
        detail: Option<String>,
    },
    /// `statement` is `None` for a failure no statement owns, such as `SET search_path`.
    Error {
        statement: Option<u16>,
        error: QueryError,
    },
    Info {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    pub database_kind: DatabaseKind,
    pub id: i64,
    pub at_ms: i64,
    pub connection_id: ConnectionId,
    pub connection_name: String,
    pub database: String,
    pub sql: String,
    pub elapsed_ms: u32,
    pub outcome: HistoryOutcome,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum HistoryOutcome {
    Rows { rows: u64 },
    Command { tag: String },
    Error { message: String },
    Cancelled,
}

#[derive(Debug, Clone, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct HistoryQuery {
    pub text: Option<String>,
    pub connection_id: Option<ConnectionId>,
    pub before_id: Option<i64>,
    pub limit: u16,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct HistoryPage {
    pub entries: Vec<HistoryEntry>,
    pub has_more: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
pub enum HistoryFilter {
    All,
    Failed,
    Writes,
}

#[derive(Debug, Clone, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct HistoryBrowseQuery {
    pub text: Option<String>,
    pub filter: HistoryFilter,
    pub before_id: Option<i64>,
    pub limit: u16,
    pub revision: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct HistoryCounts {
    pub all: u32,
    pub failed: u32,
    pub writes: u32,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct HistoryRun {
    pub id: i64,
    pub at_ms: i64,
    pub connection_name: String,
    pub elapsed_ms: u32,
    pub outcome: HistoryOutcome,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct HistoryGroupSummary {
    pub database_kind: DatabaseKind,
    pub id: i64,
    pub latest_id: i64,
    pub latest_at_ms: i64,
    pub connection_id: ConnectionId,
    pub database: String,
    pub title: String,
    pub excerpt: String,
    pub run_count: u32,
    pub failed_count: u32,
    pub is_write: bool,
    pub representative: HistoryRun,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct HistoryBrowsePage {
    pub groups: Vec<HistoryGroupSummary>,
    pub counts: HistoryCounts,
    pub has_more: bool,
    pub revision: u64,
}

#[derive(Debug, Clone, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct HistoryGroupQuery {
    pub id: i64,
    pub before_id: Option<i64>,
    pub limit: u16,
    pub revision: u64,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct HistoryGroupPage {
    pub database_kind: DatabaseKind,
    pub sql: String,
    pub runs: Vec<HistoryRun>,
    pub has_more: bool,
    pub max_elapsed_ms: u32,
    pub revision: u64,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum HistoryError {
    Stale,
    Unavailable { message: String },
}

/// Rowid of `saved_queries`. AUTOINCREMENT, so a deleted id is never reissued and a late click on
/// a deleted entry cannot act on a newer one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
pub struct SavedQueryId(pub i64);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SavedQuery {
    pub id: SavedQueryId,
    pub name: String,
    pub sql: String,
    pub database_kind: DatabaseKind,
    /// Derived on read, never stored: the SQL after its leading comments, cut to 180 characters.
    pub excerpt: String,
    pub created_at_ms: i64,
    /// Set at creation and by `saved_query_used`. An edit leaves it alone, so an edit never
    /// reorders the list.
    pub used_at_ms: i64,
}

/// Every readable saved query, most recently used first. `rev` rises with every write that
/// changes a row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SavedQueries {
    pub rev: u64,
    pub items: Vec<SavedQuery>,
}

#[derive(Debug, Clone, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SavedQueryEdit {
    /// Blank means "name it from the SQL", the default a new query gets.
    pub name: String,
    pub sql: String,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SaveQueryReply {
    pub id: SavedQueryId,
    /// The same text in the same dialect was already saved. `id` is that entry, and nothing was
    /// written.
    pub already_saved: bool,
    pub queries: SavedQueries,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum SavedQueryError {
    /// The cursor is on no statement, or the selection or the edited SQL is blank.
    NothingToSave,
    TooLong { limit_bytes: u32 },
    /// An edit of an id that no longer exists. Delete and use never return it.
    NotFound,
    Store { message: String },
}

/// Owned by the frontend, stored verbatim by the engine (single writer: the UI).
#[derive(Debug, Clone, Default, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct Workspace {
    pub tabs: Vec<TabSnapshot>,
    pub active: Option<TabId>,
    pub layout: Layout,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TabSnapshot {
    pub id: TabId,
    pub title: String,
    /// `None` only in workspaces written before table tabs saved their text; the frontend seeds the browse query.
    pub text: Option<String>,
    pub console: ConsoleConfigSnapshot,
    pub table: Option<TableSnapshot>,
    /// `None` for a scratch tab. A file tab's `text` is its buffer, saved or not.
    pub file: Option<FileSnapshot>,
}

/// The table a tab opened from the sidebar browses, in the tab's console database.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TableSnapshot {
    pub schema: String,
    pub table: String,
    pub page: u32,
    pub query: TableQuery,
}

// Hand-written so workspaces saved while tabs were a `Sql` or `Table` enum still load. `Legacy` is tagged by
// `kind` and tried first, so `Current` must never gain a field named `kind`.
impl<'de> Deserialize<'de> for TabSnapshot {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(tag = "kind", rename_all_fields = "camelCase")]
        enum Legacy {
            Sql {
                id: TabId,
                title: String,
                text: String,
                console: ConsoleConfigSnapshot,
            },
            Table {
                id: TabId,
                connection_id: ConnectionId,
                database: Option<String>,
                schema: String,
                table: String,
                page: u32,
                #[serde(default)]
                query: TableQuery,
            },
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Current {
            id: TabId,
            title: String,
            text: Option<String>,
            console: ConsoleConfigSnapshot,
            table: Option<TableSnapshot>,
            /// Parsed on its own, so a `file` a later build wrote drops the binding and keeps the tab. A tab that
            /// failed to parse would fail the workspace, which `boot()` replaces with an empty one.
            file: Option<serde_json::Value>,
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Saved {
            Legacy(Legacy),
            Current(Current),
        }
        Ok(match Saved::deserialize(d)? {
            Saved::Current(Current { id, title, text, console, table, file }) => {
                Self { id, title, text, console, table, file: file.and_then(FileSnapshot::lenient) }
            }
            Saved::Legacy(Legacy::Sql { id, title, text, console }) => Self { id, title, text: Some(text), console, table: None, file: None },
            Saved::Legacy(Legacy::Table { id, connection_id, database, schema, table, page, query }) => Self {
                id,
                title: table.clone(),
                text: None,
                console: ConsoleConfigSnapshot { connection_id: Some(connection_id), database, schema: None, mode: TxnMode::AutoCommit },
                table: Some(TableSnapshot { schema, table, page, query }),
                file: None,
            },
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct FileSnapshot {
    pub binding: FileBinding,
    /// The tab's `text` differed from the file when this snapshot was taken.
    pub dirty: bool,
}

impl FileSnapshot {
    fn lenient(value: serde_json::Value) -> Option<Self> {
        serde_json::from_value(value).inspect_err(|e| eprintln!("dropped a tab's file binding this build cannot read: {e}")).ok()
    }
}

/// Made only by the engine (`file_open`, `file_save`); the frontend stores it whole and hands it back whole.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct FileBinding {
    /// The canonical folder joined with the file's own name: `..` and folder symlinks collapse, a symlink keeps its name.
    pub path: String,
    pub stamp: FileStamp,
    pub format: FileFormat,
}

/// How the file's bytes differ from the LF, BOM-less text the editor holds. A save writes them back, with every
/// line break as CRLF when the file's first break was.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct FileFormat {
    pub bom: bool,
    pub crlf: bool,
}

/// Opaque outside `files.rs`, which makes it from the file's bytes; everything else only compares two for
/// equality. A stamp this build does not make never equals a fresh one, so the file reads as changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct FileStamp(pub String);

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ConsoleConfigSnapshot {
    pub connection_id: Option<ConnectionId>,
    /// `None` only in workspaces written before tabs carried a database; the frontend resolves it to the
    /// connection's configured database, the only one such a tab could have used.
    pub database: Option<String>,
    pub schema: Option<String>,
    pub mode: TxnMode,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct Layout {
    pub sidebar: bool,
    pub results: bool,
    pub right_panel: Option<RightPanel>,
    pub results_height: u16,
    pub sidebar_width: u16,
    pub right_panel_width: u16,
    pub assistant_chat: Option<ChatId>,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            sidebar: true,
            results: true,
            right_panel: Some(RightPanel::Assistant),
            results_height: 320,
            sidebar_width: 280,
            right_panel_width: 341,
            assistant_chat: None,
        }
    }
}

// Hand-written so workspaces saved before the panel widths or the assistant chat existed still load,
// and a saved Schema or History panel, which older builds offered, opens the assistant.
// `#[serde(default)]` would do it too, but specta would then mark every Layout field optional in the TS bindings.
impl<'de> Deserialize<'de> for Layout {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        enum SavedPanel {
            Assistant,
            Schema,
            History,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Saved {
            sidebar: bool,
            results: bool,
            right_panel: Option<SavedPanel>,
            results_height: u16,
            sidebar_width: Option<u16>,
            right_panel_width: Option<u16>,
            assistant_chat: Option<ChatId>,
        }
        let s = Saved::deserialize(d)?;
        let default = Self::default();
        Ok(Self {
            sidebar: s.sidebar,
            results: s.results,
            right_panel: s.right_panel.map(|_| RightPanel::Assistant),
            results_height: s.results_height,
            sidebar_width: s.sidebar_width.unwrap_or(default.sidebar_width),
            right_panel_width: s.right_panel_width.unwrap_or(default.right_panel_width),
            assistant_chat: s.assistant_chat,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
pub enum RightPanel {
    Assistant,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
pub enum ColorScheme {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    Light,
    Dark,
}

/// A user theme: one card in the picker, holding every variant it was created or imported with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct CustomTheme {
    pub id: String,
    pub name: String,
    pub variants: Vec<CustomThemeVariant>,
}

/// `colors` maps the frontend's token names to CSS colours; the engine only stores it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct CustomThemeVariant {
    pub id: String,
    pub name: String,
    pub appearance: Appearance,
    pub colors: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
pub enum Density {
    Compact,
    #[default]
    Standard,
    Spacious,
}

/// `family` is a name the frontend's font registry resolves to a CSS stack; unknown names pass through.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct FontSetting {
    pub family: String,
    pub size: u16,
}

impl FontSetting {
    fn interface_default() -> Self {
        Self { family: "Inter".into(), size: 13 }
    }

    fn code_default() -> Self {
        Self { family: "JetBrains Mono".into(), size: 13 }
    }
}

/// `System` follows the operating system's reduce-motion accessibility setting.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
pub enum ReduceMotion {
    #[default]
    System,
    On,
    Off,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
pub enum AssistantAccess {
    #[default]
    SchemaOnly,
    Full,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub assistant_access: AssistantAccess,
    pub max_rows: u32,
    pub max_result_bytes: u64,
    pub table_page_size: u32,
    pub stop_on_error: bool,
    pub color_scheme: ColorScheme,
    /// Theme id painted while the scheme resolves to light.
    pub light_theme: String,
    /// Theme id painted while the scheme resolves to dark.
    pub dark_theme: String,
    pub custom_themes: Vec<CustomTheme>,
    pub interface_font: FontSetting,
    /// Used by the SQL editor and the result grid.
    pub code_font: FontSetting,
    pub density: Density,
    /// 0 turns panel animation off.
    pub panel_motion_ms: u16,
    /// Reduced motion snaps panels open and closed and turns menu and dialog movement into a fade.
    pub reduce_motion: ReduceMotion,
    /// Shade each colored connection in the sidebar tree instead of drawing its dot.
    pub tint_connections: bool,
    /// One per `ProviderKind::ALL`, in that order.
    pub providers: Vec<ProviderConfig>,
    /// Seconds between background provider status checks; 0 turns them off.
    pub provider_health_check_secs: u32,
}

const DEFAULT_PANEL_MOTION_MS: u16 = 150;
const DEFAULT_PROVIDER_HEALTH_CHECK_SECS: u32 = 300;

const DEFAULT_LIGHT_THEME: &str = "l1-light";
const DEFAULT_DARK_THEME: &str = "l1-dark";

fn provider_configs(saved: Vec<ProviderConfig>) -> Vec<ProviderConfig> {
    ProviderKind::ALL
        .iter()
        .map(|&kind| saved.iter().find(|c| c.provider == kind).cloned().unwrap_or_else(|| default_config(kind)))
        .collect()
}

// Hand-written for the same reason as `TableQuery`: settings saved by older builds still load
// (`theme` became `colorScheme`; the theme, font, density, motion, reduce-motion, tint, and provider fields came later), and the TS binding keeps every field required.
impl<'de> Deserialize<'de> for Settings {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Stored {
            #[serde(default)]
            assistant_access: AssistantAccess,
            max_rows: u32,
            max_result_bytes: u64,
            table_page_size: u32,
            stop_on_error: bool,
            #[serde(default, alias = "theme")]
            color_scheme: ColorScheme,
            light_theme: Option<String>,
            dark_theme: Option<String>,
            #[serde(default)]
            custom_themes: Vec<CustomTheme>,
            interface_font: Option<FontSetting>,
            code_font: Option<FontSetting>,
            #[serde(default)]
            density: Density,
            panel_motion_ms: Option<u16>,
            #[serde(default)]
            reduce_motion: ReduceMotion,
            #[serde(default)]
            tint_connections: bool,
            providers: Option<Vec<ProviderConfig>>,
            provider_health_check_secs: Option<u32>,
        }
        let s = Stored::deserialize(d)?;
        Ok(Settings {
            assistant_access: s.assistant_access,
            max_rows: s.max_rows,
            max_result_bytes: s.max_result_bytes,
            table_page_size: s.table_page_size,
            stop_on_error: s.stop_on_error,
            color_scheme: s.color_scheme,
            light_theme: s.light_theme.unwrap_or_else(|| DEFAULT_LIGHT_THEME.into()),
            dark_theme: s.dark_theme.unwrap_or_else(|| DEFAULT_DARK_THEME.into()),
            custom_themes: s.custom_themes,
            interface_font: s.interface_font.unwrap_or_else(FontSetting::interface_default),
            code_font: s.code_font.unwrap_or_else(FontSetting::code_default),
            density: s.density,
            panel_motion_ms: s.panel_motion_ms.unwrap_or(DEFAULT_PANEL_MOTION_MS),
            reduce_motion: s.reduce_motion,
            tint_connections: s.tint_connections,
            providers: provider_configs(s.providers.unwrap_or_default()),
            provider_health_check_secs: s.provider_health_check_secs.unwrap_or(DEFAULT_PROVIDER_HEALTH_CHECK_SECS),
        })
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            assistant_access: AssistantAccess::SchemaOnly,
            max_rows: 200_000,
            max_result_bytes: 512 * 1024 * 1024,
            table_page_size: 500,
            stop_on_error: true,
            color_scheme: ColorScheme::System,
            light_theme: DEFAULT_LIGHT_THEME.into(),
            dark_theme: DEFAULT_DARK_THEME.into(),
            custom_themes: Vec::new(),
            interface_font: FontSetting::interface_default(),
            code_font: FontSetting::code_default(),
            density: Density::Standard,
            panel_motion_ms: DEFAULT_PANEL_MOTION_MS,
            reduce_motion: ReduceMotion::System,
            tint_connections: false,
            providers: provider_configs(Vec::new()),
            provider_health_check_secs: DEFAULT_PROVIDER_HEALTH_CHECK_SECS,
        }
    }
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct BootSnapshot {
    pub connections: Vec<ConnectionConfig>,
    /// Saved connections that do not parse, such as ones a newer build saved. They are not in `connections`.
    pub unreadable_connections: u32,
    pub live: Vec<ConnectionState>,
    pub consoles: Vec<ConsoleState>,
    pub workspace: Workspace,
    pub settings: Settings,
    pub providers: Vec<ProviderState>,
    /// Stored chats, newest first, at most 200.
    pub chats: Vec<ChatState>,
    pub updates: UpdateState,
    pub saved_queries: SavedQueries,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum AppEvent {
    ConnectionChanged(ConnectionState),
    /// Throttled to at most 10/s per console while streaming.
    ConsoleChanged(ConsoleState),
    ConsoleOutput {
        tab_id: TabId,
        entries: Vec<OutputEntry>,
    },
    HistoryAppended(HistoryEntry),
    ProviderChanged(ProviderState),
    ChatChanged(ChatState),
    /// Coalesced to at most 20/s per chat while a text block streams.
    ChatEntryChanged(EntrySnapshot),
    UpdateChanged(UpdateState),
    SavedQueriesChanged(SavedQueries),
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum SaveError {
    Invalid { message: String },
    Keychain { message: String },
    Store { message: String },
}

/// A console whose transaction would be lost by a disconnect, a close, or a restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct OpenTxn {
    pub tab_id: TabId,
    pub has_writes: bool,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum DisconnectError {
    OpenTransactions { tabs: Vec<OpenTxn> },
    Store { message: String },
}

/// The release line that update checks follow. `Settings` never holds it, because the webview
/// writes `Settings` whole and a stale copy there would switch the track back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
pub enum UpdateTrack {
    Stable,
    Nightly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct UpdateState {
    pub rev: u64,
    pub track: UpdateTrack,
    pub current_version: String,
    pub status: UpdateStatus,
    /// When the last check came back. A failed check leaves it as it was.
    pub checked_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum UpdateStatus {
    /// Nothing checks. The track can still be chosen.
    Disabled { reason: UpdatesOff },
    Idle,
    Checking,
    UpToDate,
    /// `downgrade` means the release is older than the running build, which happens only for a
    /// nightly build on the Stable track. `changes` lists each release after the running build up
    /// to `version`, newest first.
    Available { version: String, downgrade: bool, changes: Vec<ReleaseChanges> },
    /// `percent` is `None` while the server sends no length.
    Downloading { version: String, downgrade: bool, percent: Option<u8>, changes: Vec<ReleaseChanges> },
    /// Verified against the built-in key. The engine holds the bytes.
    Downloaded { version: String, downgrade: bool, changes: Vec<ReleaseChanges> },
    Installing { version: String },
    Failed { failure: UpdateFailure },
}

/// What one release changed, as the feed lists it. Display only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseChanges {
    pub version: String,
    pub items: Vec<ChangeItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ChangeItem {
    pub title: String,
    pub author: Option<String>,
    pub pr: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
pub enum UpdatesOff {
    DevBuild,
    /// On Linux only the AppImage can replace itself.
    NotAppImage,
    UnsupportedTarget,
    UnknownVersion,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum UpdateFailure {
    Offline,
    BadSignature,
    /// A mounted disk image or a translocated app cannot replace itself.
    ReadOnlyLocation,
    PermissionDenied,
    Other { message: String },
}

/// Everything a restart would end, as of the call. The frontend sends it back as what the user
/// accepted, and the engine installs only if that still covers what a restart would end.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct RestartImpact {
    /// Tabs with a run, count, commit, rollback, or staged-edit apply in flight.
    pub running: Vec<TabId>,
    pub transactions: Vec<OpenTxn>,
    /// Chats with a running assistant turn.
    pub chats: Vec<ChatId>,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum InstallReply {
    /// Nothing was installed. Show `impact`, then call again with it as the acceptance.
    Confirm { impact: RestartImpact },
    Restarting,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(tag = "kind")]
pub enum UpdateError {
    /// The update is not in the state the action names. The live snapshot already shows where it is.
    Stale,
    /// A track switch while a download or an install runs.
    Busy,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum CatalogError {
    NotConnected,
    /// Not in `pg_database` as last read.
    UnknownDatabase { database: String },
    /// Neither the home database, a shown database, nor the database of a console. Consoles get
    /// their catalog through the console path, so a frontend bug cannot open a session per database.
    NotShown { database: String },
    /// The metadata session or the load failed. `CatalogStatus::Failed` carries the same message.
    Failed { message: String },
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum DetailError {
    Catalog { error: CatalogError },
    /// The object was dropped after the catalog the caller holds was read.
    Gone,
    Failed { message: String },
}

/// One edit to a connection's stored view, sent by the pickers. Each is an intent that the engine
/// resolves against its own current database list and catalogs, so two quick clicks cannot lose
/// an update the way a selection computed from a stale snapshot would. Every edit is idempotent.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum ViewEdit {
    Databases { preset: Preset },
    /// One checkbox of the database picker. Showing a database whose stored schema selection
    /// resolves to no schema resets that selection to `DefaultOnly`.
    Database { name: String, shown: bool },
    ShowSystemDatabases { show: bool },
    Schemas { database: String, preset: Preset },
    /// One checkbox of the schema picker.
    Schema { database: String, name: String, shown: bool },
    ShowSystemSchemas { database: String, show: bool },
}

/// The quick actions of a picker. `None` is an explicit empty selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Type)]
pub enum Preset {
    All,
    DefaultOnly,
    None,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(tag = "kind")]
pub enum ConfigureError {
    /// Mode or connection change requested while a transaction is open.
    TransactionOpen,
    Busy,
    UnknownConnection,
    Unsupported { what: String },
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum RunError {
    Busy,
    NoStatement,
    NotConfigured,
    /// `what` names the construct, e.g. `"COPY FROM STDIN"`.
    Unsupported { what: String },
    Connect { error: ConnectError },
    /// WHERE or ORDER BY isn't a single expression, e.g. it smuggles in a second statement.
    BadFilter,
    ConfirmWrites { confirmation: WriteConfirmation },
}

/// Opaque. It names the exact statements a guarded action sends, the connection both as saved and as the tab's
/// session opened it, and the tab's database, schema, mode, and transaction state. The engine makes one when it
/// refuses a write on Production and accepts it back only while all of that is unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct WriteDigest(String);

impl WriteDigest {
    pub(crate) fn new(hex: String) -> Self {
        Self(hex)
    }
}

/// The engine ran nothing. Ask the user, then send the same request again with `digest`.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WriteConfirmation {
    /// The connection's name.
    pub connection: String,
    pub database: String,
    pub unit: ExecutionUnitKind,
    pub sends: WriteList,
    /// The writes that ran before the tab's session counted as Production, in a transaction the engine has not seen
    /// end. Any run on the tab, and Commit, can commit them.
    pub holds: WriteList,
    pub digest: WriteDigest,
    /// True when the saved connection or the opened session is Production.
    pub on_production: bool,
}

#[derive(Debug, Clone, Default, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WriteList {
    pub count: u32,
    /// The first three statements, or SQL Server batches, trimmed and cut at 2,000 characters.
    pub statements: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(tag = "kind")]
pub enum TxnError {
    Busy,
    NoTransaction,
    RolledBack,
    Query(QueryError),
    /// Commit would keep writes that ran before the tab's session counted as Production. Nothing was sent.
    ConfirmWrites { confirmation: WriteConfirmation },
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(tag = "kind")]
pub enum ResultError {
    Gone,
    OutOfRange,
    NotAPlan { what: String },
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    pub statements: Vec<StatementPlan>,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct StatementPlan {
    pub planning_time_ms: Option<f64>,
    pub execution_time_ms: Option<f64>,
    /// Empty when the server sent no statement text. PostgreSQL plans leave it empty.
    pub statement_text: String,
    pub root: PlanNode,
    pub properties: Vec<PlanProperty>,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct PlanNode {
    pub operation: String,
    pub detail: String,
    pub startup_cost: f64,
    pub total_cost: f64,
    pub plan_rows: f64,
    pub actual_rows: Option<f64>,
    pub loops: Option<f64>,
    pub total_time_ms: Option<f64>,
    pub self_time_ms: Option<f64>,
    pub diverges: bool,
    pub buffers: Option<PlanBuffers>,
    pub properties: Vec<PlanProperty>,
    pub children: Vec<PlanNode>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct PlanProperty {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct PlanBuffers {
    pub shared_hit: i64,
    pub shared_read: i64,
    pub shared_dirtied: i64,
    pub shared_written: i64,
    pub local_hit: i64,
    pub local_read: i64,
    pub local_dirtied: i64,
    pub local_written: i64,
    pub temp_read: i64,
    pub temp_written: i64,
}

/// Staged edits to one table, applied together by `table_apply`. Rows to update or delete are found by primary key.
#[derive(Debug, Clone, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TableChanges {
    pub schema: String,
    pub table: String,
    /// Columns left out take their default.
    pub inserts: Vec<Vec<CellWrite>>,
    pub updates: Vec<RowUpdate>,
    /// Each entry is the primary key of a row to delete.
    pub deletes: Vec<Vec<CellWrite>>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct CellWrite {
    pub column: String,
    /// Postgres text input for the column's type; `None` is NULL.
    pub value: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct RowUpdate {
    pub key: Vec<CellWrite>,
    pub set: Vec<CellWrite>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
pub enum ChangeKind {
    Insert,
    Update,
    Delete,
}

/// Points at one entry of `TableChanges`, by its list and index there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ChangeRef {
    pub kind: ChangeKind,
    pub index: u32,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ApplySummary {
    /// False when the changes wait in the tab's open transaction (manual mode) for Commit.
    pub committed: bool,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum ApplyError {
    Busy,
    NotConfigured,
    Unsupported { what: String },
    Connect { error: ConnectError },
    /// The tab's transaction already failed; roll it back first.
    TransactionAborted,
    /// Updates and deletes need the table's full primary key.
    NoPrimaryKey,
    Failed { change: ChangeRef, error: QueryError },
    /// An update or delete matched `rows` rows instead of one, so the row changed underneath.
    RowMismatch { change: ChangeRef, rows: u64 },
    /// Opening or committing the transaction failed.
    Transaction { error: QueryError },
    OutcomeUnknown { error: QueryError },
    ConfirmWrites { confirmation: WriteConfirmation },
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum CountError {
    NotConnected,
    Failed { message: String },
}

/// Every variant leaves the file at the chosen path as it was.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum ExportError {
    /// A newer run replaced the result, or its tab closed.
    Gone,
    /// The file could not be created, written, synced, or moved into place.
    Io { message: String },
    /// `All` for a result whose `allRows` is `Refused`.
    NotRerunnable,
    /// The tab's session is held by a run, a count, a transaction command, an edit, or another export.
    Busy,
    /// The tab now targets another connection or database than the one the rows came from.
    TargetChanged,
    /// The tab's transaction failed; only a rollback runs until it ends.
    TransactionAborted,
    Connect { error: ConnectError },
    /// The source failed on the server when it ran again.
    Query { error: QueryError },
    /// The source returned other columns, or fewer result sets, than when it first ran.
    ShapeChanged,
    /// Esc stopped the export before the file was in place.
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ImportSourceId(pub Uuid);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ImportTarget {
    pub db: DatabaseRef,
    pub schema: String,
    pub table: String,
    pub oid: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ImportRequest {
    pub source: ImportSourceId,
    pub header: bool,
    pub mapping: Vec<Option<String>>,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ImportPreview {
    pub source: ImportSourceId,
    pub target: ImportTarget,
    pub bytes: u64,
    pub rows: u64,
    pub fields: Vec<String>,
    pub columns: Vec<ColumnInfo>,
    pub mapping: Vec<Option<String>>,
    pub sample: Vec<Vec<Option<String>>>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ImportProgress {
    pub source: ImportSourceId,
    pub target: ImportTarget,
    pub bytes: u64,
    pub total_bytes: u64,
    pub rows: u64,
    pub total_rows: u64,
    pub finishing: bool,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ImportSummary {
    pub backend_pid: i32,
    pub rows: u64,
    pub committed: bool,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum ImportError {
    NotConfigured,
    Unsupported,
    Busy,
    Gone,
    TargetChanged,
    TransactionAborted,
    Cancelled,
    Io { message: String },
    Csv { line: u64, record: u64, message: String },
    Mapping { message: String },
    Connect { error: ConnectError },
    Query { error: QueryError },
    OutcomeUnknown { error: QueryError },
    ConfirmWrites { confirmation: WriteConfirmation },
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct FileOpened {
    pub binding: FileBinding,
    pub name: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct FileSaved {
    pub binding: FileBinding,
    pub name: String,
}

#[derive(Debug, Clone, Deserialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum SaveTarget {
    /// Refused with `Changed` when the file's bytes no longer match `binding.stamp`, unless they match
    /// `overwrite`, the `disk` stamp of the `Changed` the user agreed to overwrite.
    Bound { binding: FileBinding, overwrite: Option<FileStamp> },
    /// A path from the save dialog, which already asked before replacing a file. No stamp check. Refused with
    /// `OpenInAnotherTab` when it is the same file as one of `taken`, the paths the other tabs are bound to.
    New { path: String, format: Option<FileFormat>, taken: Vec<String> },
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum FileReadError {
    NotFound,
    NotAFile,
    PermissionDenied,
    /// `offset` counts from the start of the file, BOM included.
    NotUtf8 { offset: u64 },
    TooLarge { bytes: u64, limit: u64 },
    Io { message: String },
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum FileWriteError {
    /// The file changed on disk; nothing was written. `disk` is the stamp of what is there now.
    Changed { disk: FileStamp },
    /// The folder the file goes in does not exist.
    NotFound,
    NotAFile,
    PermissionDenied,
    /// `path` is the other tab's, which may name the file differently: a symlink, a hard link, another case.
    OpenInAnotherTab { path: String },
    Io { message: String },
}

/// Minted by the frontend (`crypto.randomUUID()`), like `TabId`. A chat reaches the store with its
/// first message; until then it is a draft held in memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
pub struct ChatId(pub Uuid);

/// Engine-wide monotonic. Lives only while the turn that asked for it runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
pub struct ApprovalId(pub u64);

/// The provider's id for one tool use (`toolu_…` for Claude). Opaque outside the codec.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
pub struct ToolCallId(pub String);

/// A model as its provider names it on its command line (`opus`). Valid only against the last probe.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
pub struct ModelId(pub String);

/// An effort level as its provider names it (`high`). Valid only for a model that lists it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
pub struct EffortId(pub String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Type)]
pub enum ProviderKind {
    Claude,
}

impl ProviderKind {
    pub const ALL: [ProviderKind; 1] = [ProviderKind::Claude];
}

pub fn default_config(kind: ProviderKind) -> ProviderConfig {
    match kind {
        ProviderKind::Claude => ProviderConfig {
            provider: kind,
            enabled: true,
            display_name: "Claude".into(),
            binary_path: String::new(),
            config_dir: String::new(),
            launch_args: String::new(),
        },
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ProviderConfig {
    pub provider: ProviderKind,
    pub enabled: bool,
    pub display_name: String,
    /// Empty means the provider's binary name, looked up on the login shell's PATH. `~` expands.
    pub binary_path: String,
    /// Empty means the inherited environment variable, else the provider's default directory.
    pub config_dir: String,
    /// Extra arguments as typed. The provider drops the flags the engine owns.
    pub launch_args: String,
}

impl<'de> Deserialize<'de> for ProviderConfig {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Stored {
            provider: ProviderKind,
            enabled: Option<bool>,
            display_name: Option<String>,
            binary_path: Option<String>,
            config_dir: Option<String>,
            launch_args: Option<String>,
        }
        let s = Stored::deserialize(d)?;
        let default = default_config(s.provider);
        Ok(Self {
            provider: s.provider,
            enabled: s.enabled.unwrap_or(default.enabled),
            display_name: s.display_name.unwrap_or(default.display_name),
            binary_path: s.binary_path.unwrap_or(default.binary_path),
            config_dir: s.config_dir.unwrap_or(default.config_dir),
            launch_args: s.launch_args.unwrap_or(default.launch_args),
        })
    }
}

/// What never changes for a kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInfo {
    pub default_name: String,
    pub default_binary: String,
    /// The environment variable that names the config directory: `CLAUDE_CONFIG_DIR`.
    pub config_env: String,
    pub config_dir_default: String,
    pub sign_in_command: String,
    /// Launch-argument flags the engine ignores, listed under the field.
    pub owned_flags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ProviderState {
    /// Not `kind`: `AppEvent` is tagged on `kind`.
    pub provider: ProviderKind,
    pub rev: u64,
    pub info: ProviderInfo,
    /// The engine-owned flags in the saved launch arguments, as typed, which it drops.
    pub ignored_flags: Vec<String>,
    /// A probe runs. `outcome` stays on screen meanwhile.
    pub checking: bool,
    pub checked_at_ms: Option<i64>,
    /// `None` until the first probe finishes.
    pub outcome: Option<ProbeOutcome>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum ProbeOutcome {
    Ready { version: String, plan: Option<String>, models: Vec<ModelOption> },
    SignedOut { version: String },
    NotFound { binary: String },
    /// Found, but the version check, the sign-in check, or the handshake failed or timed out.
    Broken { version: Option<String>, message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ModelOption {
    pub id: ModelId,
    pub label: String,
    pub description: String,
    /// Empty when the model takes no effort setting.
    pub efforts: Vec<EffortOption>,
    /// New chats start on it.
    pub recommended: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct EffortOption {
    pub id: EffortId,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct LimitsReport {
    /// When the last read finished.
    pub checked_at_ms: i64,
    /// One per `ProviderKind::ALL`, in that order.
    pub sources: Vec<SourceLimits>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SourceLimits {
    pub source: ProviderKind,
    /// The provider's display name.
    pub name: String,
    pub outcome: LimitsOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum LimitsOutcome {
    /// `plan` is the short tier the header shows after the name: "Max".
    Ready { plan: Option<String>, windows: Vec<LimitWindow> },
    /// Signed in with no subscription limits: an API key, Bedrock, Vertex.
    NoLimits { plan: Option<String> },
    SignedOut { sign_in_command: String },
    NotFound { binary: String },
    /// The CLI started but the exchange failed, timed out, or returned a shape we cannot read.
    Failed { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct LimitWindow {
    /// Stable within a source: "session", "weekly_all", "weekly_scoped:Opus".
    pub id: String,
    /// "Session", "Weekly", "Weekly · Opus".
    pub label: String,
    /// Remaining, 0..=100.
    pub left_percent: u8,
    pub resets_at_ms: Option<i64>,
    pub level: LimitLevel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
pub enum LimitLevel {
    Normal,
    Low,
    Exhausted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
pub enum Permission {
    /// Every `run_query` waits for Run or Deny.
    AskFirst,
    /// A read that calls only vetted built-in functions runs without asking. Anything else asks,
    /// with `ApprovalReason::Unvetted` naming what.
    Auto,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ChatOptions {
    pub provider: ProviderKind,
    pub model: ModelId,
    pub effort: Option<EffortId>,
    pub permission: Permission,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ChatState {
    pub id: ChatId,
    /// From one engine-wide counter that every chat and entry snapshot shares.
    pub rev: u64,
    /// The first line of the first message, cut at 60 chars. Empty for a draft.
    pub title: String,
    pub updated_at_ms: i64,
    pub options: ChatOptions,
    pub status: ChatStatus,
    /// Entries are numbered `0..entry_count`.
    pub entry_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum ChatStatus {
    /// Opened, never sent to. Not stored, not listed.
    Draft,
    Idle,
    /// `turn` is the seq of the user entry that started it.
    Running { turn: u32, started_at_ms: i64, phase: TurnPhase },
    /// A tombstone, so the live store drops the chat by the same keep-higher rule.
    Deleted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
pub enum TurnPhase {
    Starting,
    Working,
    AwaitingApproval,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ChatView {
    pub state: ChatState,
    /// Every entry in seq order, the running turn's as of now.
    pub entries: Vec<EntrySnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct EntrySnapshot {
    pub chat_id: ChatId,
    pub seq: u32,
    pub rev: u64,
    /// The seq of the user entry that opened this entry's turn. A user entry names itself.
    pub turn: u32,
    pub entry: Entry,
}

/// Stored verbatim in `chat_entries.json`. A stored row this build cannot parse loads as `Unreadable`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum Entry {
    User { at_ms: i64, text: String, context: Option<SentContext>, attachment: Option<Attachment> },
    Text { blocks: Vec<ReplyBlock>, done: bool },
    Tool { call: ToolCallId, tool: ToolUse, status: ToolStatus },
    /// Shown when the engine had to change course, such as a lost provider session.
    Notice { text: String },
    /// Closes every turn.
    End { at_ms: i64, outcome: TurnOutcome, suggestions: Vec<String> },
    Unreadable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SentContext {
    pub database_kind: Option<DatabaseKind>,
    pub db: Option<DatabaseRef>,
    /// As named at send, so the chip reads right after a rename or a delete.
    pub connection_name: Option<String>,
    pub tab_title: String,
}

impl<'de> Deserialize<'de> for SentContext {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Stored {
            database_kind: Option<DatabaseKind>,
            db: Option<DatabaseRef>,
            connection_name: Option<String>,
            tab_title: String,
        }
        let Stored { database_kind, db, connection_name, tab_title } = Stored::deserialize(d)?;
        Ok(Self { database_kind: database_kind.or(db.as_ref().map(|_| DatabaseKind::Postgres)), db, connection_name, tab_title })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    pub tab_title: String,
    pub columns: Vec<String>,
    /// What the model saw: at most 100 rows, cells cut at 200 chars.
    pub rows: Vec<Vec<Option<String>>>,
    pub total_rows: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum ReplyBlock {
    Markdown { text: String },
    /// A fenced SQL block. `relations` and `vetted` are filled when the fence closes.
    Sql { sql: String, relations: Vec<String>, vetted: bool, complete: bool },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum ToolUse {
    DescribeTables { tables: Vec<String> },
    SearchCatalog { query: String },
    RunQuery { sql: String, purpose: String, relations: Vec<String> },
    /// A tool the engine does not serve. Always refused.
    Other { name: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum ToolStatus {
    Running,
    AwaitingApproval { approval: ApprovalId, reason: ApprovalReason },
    Done { result: ToolResult },
    /// `message` is what the model got.
    Failed { message: String },
    /// `stopped` is true when Stop denied it, false when the user pressed Deny.
    Denied { stopped: bool },
    /// The turn ended before an answer.
    Expired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum ApprovalReason {
    AskFirst,
    /// The query uses what the engine cannot vouch for as read-only: the called functions off
    /// its allowlist, as written, plus `U& escape` or `SELECT INTO`. Auto asks for these too.
    Unvetted { functions: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum ToolResult {
    Described { found: Vec<String>, missing: Vec<String> },
    Found { matches: u16 },
    /// The preview the model saw. `more` is true when the query had more rows.
    Rows { columns: Vec<String>, rows: Vec<Vec<Option<String>>>, more: bool, elapsed_ms: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum TurnOutcome {
    Completed,
    Stopped,
    /// The app quit or crashed mid-turn. Written when the chat next opens.
    Lost,
    Failed { error: AgentError },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum AgentError {
    NotFound { binary: String },
    SignedOut,
    /// `detail` is the end of its stderr.
    Exited { code: Option<i32>, detail: String },
    /// An API failure, a rate limit, an overload.
    Provider { message: String },
}

#[derive(Debug, Clone, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SendRequest {
    pub text: String,
    pub tab: Option<TabContext>,
}

/// The active tab when the user pressed Send. The engine reads SQL out of `text` itself.
#[derive(Debug, Clone, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TabContext {
    pub tab_id: TabId,
    pub title: String,
    pub db: Option<DatabaseRef>,
    pub text: String,
    /// UTF-16 offsets, as for `RunRequest::Script`.
    pub target: RunTarget,
    /// The result the tab shows, when the attach button is on.
    pub attach: Option<ResultRef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TurnAccepted {
    /// The seq of the stored user entry.
    pub turn: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Type)]
pub enum Decision {
    Run,
    Deny,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum SendError {
    Busy,
    EmptyMessage,
    ProviderOff { provider: ProviderKind },
    /// The chat's model left the provider's list since it was picked.
    UnknownModel { model: ModelId },
    /// A newer run replaced the attached result.
    AttachmentGone,
    Store { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum ChatError {
    UnknownModel { model: ModelId },
    UnsupportedEffort { effort: EffortId },
    Store { message: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(tag = "kind")]
pub enum ApproveError {
    /// Answered already, denied by Stop, expired, or another chat's.
    NotPending,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn struct_variant_fields_are_camel_case_and_tags_keep_variant_names() {
        let err = CatalogError::UnknownDatabase { database: "reporting".into() };
        assert_eq!(serde_json::to_value(err).unwrap(), json!({ "kind": "UnknownDatabase", "database": "reporting" }));
        let status = CatalogStatus::Ready { version: 3 };
        assert_eq!(serde_json::to_value(status).unwrap(), json!({ "kind": "Ready", "version": 3 }));
    }

    #[test]
    fn tabs_saved_before_they_carried_a_database_load_with_none() {
        let tab: TabSnapshot = serde_json::from_value(json!({
            "kind": "Table", "id": "00000000-0000-0000-0000-000000000001", "connectionId": "00000000-0000-0000-0000-000000000002",
            "schema": "public", "table": "orders", "page": 0
        }))
        .unwrap();
        assert_eq!(tab.console.database, None);
        let console: ConsoleConfigSnapshot =
            serde_json::from_value(json!({ "connectionId": "00000000-0000-0000-0000-000000000002", "schema": null, "mode": "Manual" })).unwrap();
        assert_eq!((console.database, console.mode), (None, TxnMode::Manual));
    }

    const TAB: &str = "00000000-0000-0000-0000-000000000001";
    const CONN: &str = "00000000-0000-0000-0000-000000000002";

    fn uuid(s: &str) -> Uuid {
        s.parse().unwrap()
    }

    #[test]
    fn an_old_table_tab_loads_as_an_auto_commit_tab_bound_to_its_table() {
        let tab: TabSnapshot = serde_json::from_value(json!({
            "kind": "Table", "id": TAB, "connectionId": CONN, "database": "shop",
            "schema": "public", "table": "orders", "page": 2, "query": { "orderBy": "id", "filters": [], "where": "" }
        }))
        .unwrap();
        assert_eq!((tab.id, tab.title.as_str(), tab.text), (TabId(uuid(TAB)), "orders", None));
        assert_eq!(
            (tab.console.connection_id, tab.console.database.as_deref(), tab.console.schema, tab.console.mode),
            (Some(ConnectionId(uuid(CONN))), Some("shop"), None, TxnMode::AutoCommit)
        );
        let query = TableQuery { order_by: "id".into(), ..Default::default() };
        assert_eq!(tab.table, Some(TableSnapshot { schema: "public".into(), table: "orders".into(), page: 2, query }));
    }

    #[test]
    fn an_old_sql_tab_loads_with_its_text_and_no_table() {
        let tab: TabSnapshot = serde_json::from_value(json!({
            "kind": "Sql", "id": TAB, "title": "q.sql", "text": "select 1",
            "console": { "connectionId": CONN, "database": "reporting", "schema": null, "mode": "Manual" }
        }))
        .unwrap();
        assert_eq!((tab.title.as_str(), tab.text.as_deref(), tab.table), ("q.sql", Some("select 1"), None));
        assert_eq!((tab.console.database.as_deref(), tab.console.mode), (Some("reporting"), TxnMode::Manual));
    }

    #[test]
    fn a_tab_round_trips_its_text_mode_and_table() {
        let value = json!({
            "id": TAB, "title": "orders", "text": "select * from orders",
            "console": { "connectionId": CONN, "database": "shop", "schema": null, "mode": "Manual" },
            "table": { "schema": "public", "table": "orders", "page": 1, "query": { "orderBy": "", "filters": [], "where": "total > 0" } },
            "file": null
        });
        let tab: TabSnapshot = serde_json::from_value(value.clone()).unwrap();
        assert_eq!((tab.text.as_deref(), tab.console.mode), (Some("select * from orders"), TxnMode::Manual));
        assert_eq!(tab.table.as_ref().map(|t| (t.page, t.query.r#where.as_str())), Some((1, "total > 0")));
        assert_eq!(serde_json::to_value(&tab).unwrap(), value);

        let plain = json!({
            "id": TAB, "title": "Untitled-1.sql", "text": "",
            "console": { "connectionId": null, "database": null, "schema": null, "mode": "AutoCommit" }, "table": null, "file": null
        });
        let tab: TabSnapshot = serde_json::from_value(plain.clone()).unwrap();
        assert_eq!(serde_json::to_value(&tab).unwrap(), plain);
        assert_eq!(tab.table, None);
    }

    fn file_tab(file: serde_json::Value) -> serde_json::Value {
        json!({
            "id": TAB, "title": "report.sql", "text": "select 1;\n",
            "console": { "connectionId": CONN, "database": "shop", "schema": null, "mode": "AutoCommit" }, "table": null,
            "file": file
        })
    }

    fn binding_json() -> serde_json::Value {
        json!({ "path": "/Users/a/report.sql", "stamp": "11:4a5e", "format": { "bom": true, "crlf": false } })
    }

    #[test]
    fn a_tab_saved_before_files_existed_loads_as_scratch() {
        let mut old = file_tab(json!(null));
        assert_eq!(serde_json::from_value::<TabSnapshot>(old.clone()).unwrap().file, None);
        old.as_object_mut().unwrap().remove("file");
        let tab: TabSnapshot = serde_json::from_value(old).unwrap();
        assert_eq!((tab.title.as_str(), tab.text.as_deref(), tab.file), ("report.sql", Some("select 1;\n"), None));
    }

    #[test]
    fn a_file_tab_round_trips() {
        let value = file_tab(json!({ "binding": binding_json(), "dirty": true }));
        let tab: TabSnapshot = serde_json::from_value(value.clone()).unwrap();
        let binding = FileBinding {
            path: "/Users/a/report.sql".into(),
            stamp: FileStamp("11:4a5e".into()),
            format: FileFormat { bom: true, crlf: false },
        };
        assert_eq!(tab.file, Some(FileSnapshot { binding, dirty: true }));
        assert_eq!(serde_json::to_value(&tab).unwrap(), value);
    }

    #[test]
    fn a_file_binding_this_build_cannot_read_drops_to_scratch_and_keeps_the_tab() {
        for file in [json!({ "binding": { "path": 3 } }), json!("x")] {
            let tab: TabSnapshot = serde_json::from_value(file_tab(file.clone())).unwrap();
            assert_eq!((tab.title.as_str(), tab.text.as_deref(), tab.file), ("report.sql", Some("select 1;\n"), None), "{file}");
        }
        let good = file_tab(json!({ "binding": binding_json(), "dirty": false }));
        let mut bad = file_tab(json!("x"));
        bad["id"] = json!(CONN);
        let workspace: Workspace =
            serde_json::from_value(json!({ "tabs": [bad, good], "active": TAB, "layout": serde_json::to_value(Layout::default()).unwrap() })).unwrap();
        let files: Vec<_> = workspace.tabs.iter().map(|t| t.file.as_ref().map(|f| f.binding.path.as_str())).collect();
        assert_eq!(files, vec![None, Some("/Users/a/report.sql")]);
    }

    #[test]
    fn a_file_binding_with_fields_from_a_later_build_still_loads() {
        let mut binding = binding_json();
        binding["format"]["encoding"] = json!("utf-16");
        let tab: TabSnapshot = serde_json::from_value(file_tab(json!({ "binding": binding, "dirty": false, "watched": true }))).unwrap();
        let file = tab.file.expect("the binding survives fields this build does not know");
        assert_eq!((file.binding.format, file.dirty), (FileFormat { bom: true, crlf: false }, false));
    }

    /// Stable's `TabSnapshot` and `Workspace` from before file tabs. Update this copy only when a release changes
    /// `Current`.
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct StableTab {
        id: TabId,
        title: String,
        text: Option<String>,
        console: ConsoleConfigSnapshot,
        table: Option<TableSnapshot>,
    }

    #[derive(Deserialize)]
    struct StableWorkspace {
        tabs: Vec<StableTab>,
        active: Option<TabId>,
        layout: Layout,
    }

    #[test]
    fn the_previous_stable_build_loads_a_workspace_this_build_wrote() {
        let tab: TabSnapshot = serde_json::from_value(file_tab(json!({ "binding": binding_json(), "dirty": true }))).unwrap();
        let workspace = Workspace { tabs: vec![tab], active: Some(TabId(uuid(TAB))), layout: Layout::default() };
        let stable: StableWorkspace = serde_json::from_value(serde_json::to_value(&workspace).unwrap()).unwrap();
        let tab = &stable.tabs[0];
        assert_eq!((tab.id, tab.title.as_str(), tab.text.as_deref()), (TabId(uuid(TAB)), "report.sql", Some("select 1;\n")));
        assert_eq!((tab.console.connection_id, &tab.table), (Some(ConnectionId(uuid(CONN))), &None));
        assert_eq!((stable.active, stable.layout.sidebar), (Some(TabId(uuid(TAB))), true));
    }

    #[test]
    fn file_errors_are_tagged_by_kind_with_camel_case_fields() {
        assert_eq!(serde_json::to_value(FileReadError::TooLarge { bytes: 5, limit: 4 }).unwrap(), json!({ "kind": "TooLarge", "bytes": 5, "limit": 4 }));
        assert_eq!(serde_json::to_value(FileReadError::NotUtf8 { offset: 3 }).unwrap(), json!({ "kind": "NotUtf8", "offset": 3 }));
        let changed = FileWriteError::Changed { disk: FileStamp("1:ab".into()) };
        assert_eq!(serde_json::to_value(changed).unwrap(), json!({ "kind": "Changed", "disk": "1:ab" }));
        assert_eq!(serde_json::to_value(FileWriteError::NotFound).unwrap(), json!({ "kind": "NotFound" }));
        let taken = FileWriteError::OpenInAnotherTab { path: "/a.sql".into() };
        assert_eq!(serde_json::to_value(taken).unwrap(), json!({ "kind": "OpenInAnotherTab", "path": "/a.sql" }));
    }

    #[test]
    fn save_targets_deserialize_from_the_wire() {
        let bound: SaveTarget = serde_json::from_value(json!({ "kind": "Bound", "binding": binding_json(), "overwrite": null })).unwrap();
        assert!(matches!(&bound, SaveTarget::Bound { binding, overwrite: None } if binding.path == "/Users/a/report.sql" && binding.format.bom), "{bound:?}");
        let consent: SaveTarget = serde_json::from_value(json!({ "kind": "Bound", "binding": binding_json(), "overwrite": "2:cd" })).unwrap();
        assert!(matches!(&consent, SaveTarget::Bound { overwrite: Some(FileStamp(disk)), .. } if disk == "2:cd"), "{consent:?}");
        let new: SaveTarget = serde_json::from_value(json!({ "kind": "New", "path": "/a.sql", "format": null, "taken": ["/b.sql"] })).unwrap();
        assert!(matches!(&new, SaveTarget::New { path, format: None, taken } if path == "/a.sql" && taken == &["/b.sql"]), "{new:?}");
    }

    #[test]
    fn settings_saved_before_theme_existed_load_with_default_themes() {
        let s: Settings = serde_json::from_value(json!({
            "maxRows": 10, "maxResultBytes": 20, "tablePageSize": 30, "stopOnError": false
        }))
        .unwrap();
        assert_eq!((s.max_rows, s.table_page_size, s.stop_on_error, s.color_scheme), (10, 30, false, ColorScheme::System));
        assert_eq!((s.light_theme.as_str(), s.dark_theme.as_str(), s.custom_themes.len()), ("l1-light", "l1-dark", 0));
        assert_eq!(
            (s.interface_font, s.code_font, s.density, s.panel_motion_ms),
            (
                FontSetting { family: "Inter".into(), size: 13 },
                FontSetting { family: "JetBrains Mono".into(), size: 13 },
                Density::Standard,
                150
            )
        );
        assert!(!s.tint_connections);
        assert_eq!((s.reduce_motion, Settings::default().reduce_motion), (ReduceMotion::System, ReduceMotion::System));
        assert_eq!(s.assistant_access, AssistantAccess::SchemaOnly);
        assert_eq!(Settings::default().assistant_access, AssistantAccess::SchemaOnly);
        let full = Settings { assistant_access: AssistantAccess::Full, ..Settings::default() };
        let round: Settings = serde_json::from_value(serde_json::to_value(full).unwrap()).unwrap();
        assert_eq!(round.assistant_access, AssistantAccess::Full);
    }

    #[test]
    fn settings_round_trip_appearance_fields() {
        let s = Settings {
            interface_font: FontSetting { family: "System".into(), size: 15 },
            code_font: FontSetting { family: "Menlo".into(), size: 11 },
            density: Density::Compact,
            panel_motion_ms: 0,
            reduce_motion: ReduceMotion::On,
            tint_connections: true,
            ..Settings::default()
        };
        let value = serde_json::to_value(&s).unwrap();
        assert_eq!(value["interfaceFont"], json!({ "family": "System", "size": 15 }));
        assert_eq!((value["density"].clone(), value["panelMotionMs"].clone()), (json!("Compact"), json!(0)));
        assert_eq!((value["reduceMotion"].clone(), value["tintConnections"].clone()), (json!("On"), json!(true)));
        let round: Settings = serde_json::from_value(value).unwrap();
        assert_eq!((round.code_font, round.density, round.panel_motion_ms), (FontSetting { family: "Menlo".into(), size: 11 }, Density::Compact, 0));
        assert_eq!(round.reduce_motion, ReduceMotion::On);
        assert!(round.tint_connections);
        let off: Settings = serde_json::from_value(json!({
            "maxRows": 10, "maxResultBytes": 20, "tablePageSize": 30, "stopOnError": false, "reduceMotion": "Off"
        }))
        .unwrap();
        assert_eq!(off.reduce_motion, ReduceMotion::Off);
    }

    #[test]
    fn settings_saved_with_the_old_theme_key_keep_their_scheme() {
        let s: Settings = serde_json::from_value(json!({
            "maxRows": 10, "maxResultBytes": 20, "tablePageSize": 30, "stopOnError": false, "theme": "Dark"
        }))
        .unwrap();
        assert_eq!((s.color_scheme, s.light_theme.as_str(), s.dark_theme.as_str()), (ColorScheme::Dark, "l1-light", "l1-dark"));
    }

    #[test]
    fn settings_round_trip_custom_themes() {
        let variant = |id: &str, name: &str, appearance, background: &str| CustomThemeVariant {
            id: id.into(),
            name: name.into(),
            appearance,
            colors: BTreeMap::from([("background".into(), background.into()), ("accent".into(), "#e0a040".into())]),
        };
        let custom = CustomTheme {
            id: "custom-1".into(),
            name: "Dusk".into(),
            variants: vec![variant("custom-1-0", "Dusk Day", Appearance::Light, "#fafafa"), variant("custom-1-1", "Dusk Night", Appearance::Dark, "#101018")],
        };
        let s = Settings { color_scheme: ColorScheme::Light, dark_theme: "custom-1-1".into(), custom_themes: vec![custom.clone()], ..Settings::default() };
        let value = serde_json::to_value(&s).unwrap();
        assert_eq!(value["customThemes"][0]["name"], json!("Dusk"));
        assert_eq!(value["customThemes"][0]["variants"][1], json!({
            "id": "custom-1-1", "name": "Dusk Night", "appearance": "dark", "colors": { "accent": "#e0a040", "background": "#101018" }
        }));
        assert_eq!(value["colorScheme"], json!("Light"));
        let round: Settings = serde_json::from_value(value).unwrap();
        assert_eq!((round.color_scheme, round.dark_theme.as_str(), round.custom_themes), (ColorScheme::Light, "custom-1-1", vec![custom]));
    }

    #[test]
    fn run_request_tags_are_camel_case() {
        let req: RunRequest = serde_json::from_value(json!({
            "kind": "tablePage", "schema": "public", "table": "orders", "page": 2
        }))
        .unwrap();
        assert!(matches!(req, RunRequest::TablePage { page: 2, .. }));

        let target: RunTarget =
            serde_json::from_value(json!({ "kind": "cursor", "offset": 7 })).unwrap();
        assert!(matches!(target, RunTarget::Cursor { offset: DocOffset(7) }));
    }

    #[test]
    fn nested_connect_error_keeps_its_own_tag() {
        let err = RunError::Connect {
            error: ConnectError::MissingPassword,
        };
        assert_eq!(
            serde_json::to_value(err).unwrap(),
            json!({ "kind": "Connect", "error": { "kind": "MissingPassword" } })
        );
    }

    #[test]
    fn ssh_errors_keep_their_own_tag_inside_a_connect_error() {
        let unknown = SshError::HostKeyUnknown { host: "bastion".into(), port: 22, algorithm: "ssh-ed25519".into(), fingerprint: "SHA256:x".into(), trust: HostKeyId(3) };
        assert_eq!(
            serde_json::to_value(RunError::Connect { error: ConnectError::Ssh { error: unknown } }).unwrap(),
            json!({ "kind": "Connect", "error": { "kind": "Ssh", "error": { "kind": "HostKeyUnknown", "host": "bastion", "port": 22, "algorithm": "ssh-ed25519", "fingerprint": "SHA256:x", "trust": 3 } } })
        );
        let changed = SshError::HostKeyChanged { host: "b".into(), port: 2222, algorithm: "ssh-rsa".into(), fingerprint: "SHA256:y".into(), known_at: "/k:4".into() };
        assert_eq!(serde_json::to_value(changed).unwrap()["knownAt"], "/k:4");
    }

    #[test]
    fn layout_saved_before_panel_widths_existed_still_loads() {
        let tab: TabSnapshot = serde_json::from_value(json!({
            "kind": "Table", "id": "00000000-0000-0000-0000-000000000001", "connectionId": "00000000-0000-0000-0000-000000000002",
            "schema": "public", "table": "orders", "page": 3, "query": { "sort": [{ "column": "total", "desc": true }], "where": "x" }
        }))
        .unwrap();
        let kept = TableQuery { r#where: "x".into(), ..Default::default() };
        assert_eq!(tab.table.map(|t| (t.page, t.query)), Some((3, kept)), "an old query shape keeps what still fits");

        let layout: Layout = serde_json::from_value(json!({
            "sidebar": false, "results": false, "rightPanel": "History", "resultsHeight": 280
        }))
        .unwrap();
        assert!(!layout.sidebar);
        assert!(!layout.results);
        assert_eq!(layout.right_panel, Some(RightPanel::Assistant), "a removed panel opens the assistant");
        let schema: Layout = serde_json::from_value(json!({ "sidebar": true, "results": true, "rightPanel": "Schema", "resultsHeight": 280 })).unwrap();
        assert_eq!(schema.right_panel, Some(RightPanel::Assistant));
        assert_eq!(layout.results_height, 280);
        assert_eq!(layout.sidebar_width, 280);
        assert_eq!(layout.right_panel_width, 341);
    }

    const CHAT: &str = "00000000-0000-0000-0000-000000000003";

    #[test]
    fn layout_saved_before_the_assistant_chat_existed_loads_with_none() {
        let saved = json!({ "sidebar": true, "results": true, "rightPanel": "Assistant", "resultsHeight": 320, "sidebarWidth": 256, "rightPanelWidth": 341 });
        let layout: Layout = serde_json::from_value(saved.clone()).unwrap();
        assert_eq!(layout.assistant_chat, None);
        let mut with_chat = saved;
        with_chat["assistantChat"] = json!(CHAT);
        let layout: Layout = serde_json::from_value(with_chat).unwrap();
        assert_eq!(layout.assistant_chat, Some(ChatId(uuid(CHAT))));
    }

    fn claude(enabled: bool, display_name: &str, binary_path: &str, config_dir: &str, launch_args: &str) -> ProviderConfig {
        ProviderConfig {
            provider: ProviderKind::Claude,
            enabled,
            display_name: display_name.into(),
            binary_path: binary_path.into(),
            config_dir: config_dir.into(),
            launch_args: launch_args.into(),
        }
    }

    #[test]
    fn settings_saved_before_providers_existed_load_the_claude_default() {
        let s: Settings = serde_json::from_value(json!({
            "maxRows": 10, "maxResultBytes": 20, "tablePageSize": 30, "stopOnError": false
        }))
        .unwrap();
        assert_eq!(s.providers, vec![claude(true, "Claude", "", "", "")]);
        assert_eq!(s.provider_health_check_secs, 300);
    }

    #[test]
    fn a_duplicate_provider_is_dropped_and_the_first_kept() {
        let first = json!({ "provider": "Claude", "enabled": false, "displayName": "Work", "binaryPath": "~/bin/claude", "configDir": "~/.claude-work", "launchArgs": "--debug" });
        let s: Settings = serde_json::from_value(json!({
            "maxRows": 10, "maxResultBytes": 20, "tablePageSize": 30, "stopOnError": false, "providerHealthCheckSecs": 0,
            "providers": [first, { "provider": "Claude", "enabled": true, "displayName": "Second", "binaryPath": "", "configDir": "", "launchArgs": "" }]
        }))
        .unwrap();
        assert_eq!(s.providers, vec![claude(false, "Work", "~/bin/claude", "~/.claude-work", "--debug")]);
        assert_eq!(s.provider_health_check_secs, 0);
        let value = serde_json::to_value(&s).unwrap();
        assert_eq!((value["providers"].clone(), value["providerHealthCheckSecs"].clone()), (json!([first]), json!(0)));
    }

    #[test]
    fn a_provider_config_saved_without_launch_args_loads_with_an_empty_string() {
        let config: ProviderConfig =
            serde_json::from_value(json!({ "provider": "Claude", "enabled": false, "displayName": "Claude", "binaryPath": "/opt/claude", "configDir": "" })).unwrap();
        assert_eq!(config, claude(false, "Claude", "/opt/claude", "", ""));
        let err = serde_json::from_value::<ProviderConfig>(json!({ "enabled": true })).unwrap_err();
        assert_eq!(err.to_string(), "missing field `provider`");
    }

    #[test]
    fn provider_changed_keeps_its_own_tag() {
        let state = ProviderState {
            provider: ProviderKind::Claude,
            rev: 4,
            info: ProviderInfo {
                default_name: "Claude".into(),
                default_binary: "claude".into(),
                config_env: "CLAUDE_CONFIG_DIR".into(),
                config_dir_default: "~/.claude".into(),
                sign_in_command: "claude auth login".into(),
                owned_flags: vec!["--verbose".into()],
            },
            ignored_flags: vec!["--verbose".into()],
            checking: true,
            checked_at_ms: Some(1700),
            outcome: Some(ProbeOutcome::SignedOut { version: "2.1.301".into() }),
        };
        assert_eq!(
            serde_json::to_value(AppEvent::ProviderChanged(state)).unwrap(),
            json!({
                "kind": "ProviderChanged", "provider": "Claude", "rev": 4,
                "info": {
                    "defaultName": "Claude", "defaultBinary": "claude", "configEnv": "CLAUDE_CONFIG_DIR",
                    "configDirDefault": "~/.claude", "signInCommand": "claude auth login", "ownedFlags": ["--verbose"]
                },
                "ignoredFlags": ["--verbose"],
                "checking": true, "checkedAtMs": 1700, "outcome": { "kind": "SignedOut", "version": "2.1.301" }
            })
        );
    }

    #[test]
    fn update_changed_keeps_its_own_tag() {
        let state = |status| UpdateState { rev: 3, track: UpdateTrack::Nightly, current_version: "0.1.1-nightly.20261023.5".into(), status, checked_at_ms: Some(1700) };
        assert_eq!(
            serde_json::to_value(AppEvent::UpdateChanged(state(UpdateStatus::Downloading {
                version: "0.1.0".into(),
                downgrade: true,
                percent: Some(40),
                changes: vec![ReleaseChanges { version: "0.1.0".into(), items: vec![ChangeItem { title: "fix(ui): x".into(), author: None, pr: Some(7) }] }],
            }))).unwrap(),
            json!({
                "kind": "UpdateChanged", "rev": 3, "track": "Nightly", "currentVersion": "0.1.1-nightly.20261023.5", "checkedAtMs": 1700,
                "status": {
                    "kind": "Downloading", "version": "0.1.0", "downgrade": true, "percent": 40,
                    "changes": [{ "version": "0.1.0", "items": [{ "title": "fix(ui): x", "author": null, "pr": 7 }] }]
                }
            })
        );
        assert_eq!(
            serde_json::to_value(state(UpdateStatus::Failed { failure: UpdateFailure::Other { message: "disk full".into() } })).unwrap()["status"],
            json!({ "kind": "Failed", "failure": { "kind": "Other", "message": "disk full" } })
        );
    }

    #[test]
    fn a_restart_impact_round_trips_as_the_acceptance() {
        let tab = TabId(Uuid::from_u128(1));
        let impact = RestartImpact { running: vec![tab], transactions: vec![OpenTxn { tab_id: tab, has_writes: true }], chats: vec![ChatId(Uuid::from_u128(2))] };
        let value = serde_json::to_value(&impact).unwrap();
        assert_eq!(value["transactions"], json!([{ "tabId": "00000000-0000-0000-0000-000000000001", "hasWrites": true }]));
        assert_eq!(serde_json::from_value::<RestartImpact>(value).unwrap(), impact);
    }

    #[test]
    fn a_tool_entry_awaiting_approval_round_trips() {
        let value = json!({
            "kind": "Tool", "call": "toolu_01",
            "tool": { "kind": "RunQuery", "sql": "select pg_sleep(1)", "purpose": "wait", "relations": [] },
            "status": { "kind": "AwaitingApproval", "approval": 7, "reason": { "kind": "Unvetted", "functions": ["pg_sleep"] } }
        });
        let entry: Entry = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(
            entry,
            Entry::Tool {
                call: ToolCallId("toolu_01".into()),
                tool: ToolUse::RunQuery { sql: "select pg_sleep(1)".into(), purpose: "wait".into(), relations: vec![] },
                status: ToolStatus::AwaitingApproval { approval: ApprovalId(7), reason: ApprovalReason::Unvetted { functions: vec!["pg_sleep".into()] } },
            }
        );
        assert_eq!(serde_json::to_value(&entry).unwrap(), value);
    }
}

#[cfg(test)]
mod database_compatibility_tests {
    use super::*;

    #[test]
    fn old_connections_and_sent_context_load_as_postgres_and_unknown_kinds_fail() {
        let mut saved = serde_json::json!({"id":Uuid::nil(), "name":"legacy", "host":"localhost", "port":5432, "database":"shop", "user":"reader", "sslMode":"Prefer", "hasPassword":false});
        let config: ConnectionConfig = serde_json::from_value(saved.clone()).unwrap();
        assert_eq!(config.database_kind, DatabaseKind::Postgres);
        assert_eq!(serde_json::to_value(&config).unwrap()["databaseKind"], "Postgres");
        saved["databaseKind"] = serde_json::json!("SqlServer");
        assert_eq!(serde_json::from_value::<ConnectionConfig>(saved.clone()).unwrap().database_kind, DatabaseKind::SqlServer);
        saved["databaseKind"] = serde_json::json!("unknown");
        assert!(serde_json::from_value::<ConnectionConfig>(saved).is_err());
        let sent: SentContext = serde_json::from_value(serde_json::json!({"db":{"connectionId":Uuid::nil(),"database":"shop"},"connectionName":"old", "tabTitle":"query"})).unwrap();
        assert_eq!(sent.database_kind, Some(DatabaseKind::Postgres));
        let entry: Entry = serde_json::from_value(serde_json::json!({"kind":"User", "atMs":0,"text":"question","context":{"db":{"connectionId":Uuid::nil(),"database":"shop"},"connectionName":"old", "tabTitle":"query"}})).unwrap();
        assert!(matches!(entry, Entry::User { context: Some(SentContext { database_kind: Some(DatabaseKind::Postgres), .. }), .. }));
    }

    #[test]
    fn connections_saved_before_tunnels_existed_load_without_one() {
        let saved = serde_json::json!({"databaseKind":"Postgres", "id":Uuid::nil(), "name":"legacy", "host":"localhost", "port":5432, "database":"shop", "user":"reader", "sslMode":"Prefer", "caCertPath":null, "hasPassword":true, "color":null});
        let config: ConnectionConfig = serde_json::from_value(saved).unwrap();
        assert_eq!(config.ssh, None);
        let tunnel = SshTunnel { host: "bastion".into(), port: 2222, user: "tunnel".into(), auth: SshAuth::Key { path: "~/.ssh/id_ed25519".into() }, has_secret: true };
        let stored = serde_json::to_value(ConnectionConfig { ssh: Some(tunnel.clone()), ..config }).unwrap();
        assert_eq!(stored["ssh"], serde_json::json!({"host":"bastion", "port":2222, "user":"tunnel", "auth":{"kind":"Key", "path":"~/.ssh/id_ed25519"}, "hasSecret":true}));
        assert_eq!(serde_json::from_value::<ConnectionConfig>(stored).unwrap().ssh, Some(tunnel));
    }

    #[test]
    fn connections_saved_before_environments_load_as_not_set() {
        let saved = serde_json::json!({"databaseKind":"Postgres", "id":Uuid::nil(), "name":"legacy", "host":"localhost", "port":5432, "database":"shop", "user":"reader", "sslMode":"Prefer", "caCertPath":null, "hasPassword":true, "color":null, "ssh":null});
        assert_eq!(serde_json::from_value::<ConnectionConfig>(saved).unwrap().environment, None);
    }

    #[test]
    fn a_production_connection_round_trips() {
        let saved = serde_json::json!({"databaseKind":"Postgres", "id":Uuid::nil(), "name":"prod-postgres", "host":"db.internal", "port":5432, "database":"shop", "user":"reader", "sslMode":"VerifyFull", "caCertPath":null, "hasPassword":true, "color":"red", "ssh":null, "environment":"Production", "sqlServerAuth":null});
        let config: ConnectionConfig = serde_json::from_value(saved.clone()).unwrap();
        assert_eq!(config.environment, Some(Environment::Production));
        assert_eq!(serde_json::to_value(&config).unwrap(), saved);
        let unset = serde_json::to_value(ConnectionConfig { environment: None, ..config }).unwrap();
        assert_eq!(unset["environment"], serde_json::Value::Null);
    }

    #[test]
    fn sql_server_connections_saved_before_entra_load_as_sql_login() {
        let saved = serde_json::json!({"databaseKind":"SqlServer", "id":Uuid::nil(), "name":"legacy", "host":"localhost", "port":1433, "database":"shop", "user":"sa", "sslMode":"Require", "caCertPath":null, "hasPassword":true, "color":null, "ssh":null, "environment":null});
        assert_eq!(serde_json::from_value::<ConnectionConfig>(saved).unwrap().sql_server_auth, None);
    }

    #[test]
    fn each_sql_server_auth_round_trips_and_a_newer_one_loads_as_unknown() {
        let mut saved = serde_json::json!({"databaseKind":"SqlServer", "id":Uuid::nil(), "name":"azure", "host":"db.example.net", "port":1433, "database":"shop", "user":"", "sslMode":"VerifyFull", "caCertPath":null, "hasPassword":false, "color":null, "ssh":null, "environment":null, "sqlServerAuth":null});
        for (wire, auth) in [(serde_json::Value::Null, None), ("SqlLogin".into(), Some(SqlServerAuth::SqlLogin)), ("EntraAzureCli".into(), Some(SqlServerAuth::EntraAzureCli))] {
            saved["sqlServerAuth"] = wire;
            let config: ConnectionConfig = serde_json::from_value(saved.clone()).unwrap();
            assert_eq!(config.sql_server_auth, auth);
            assert_eq!(serde_json::to_value(&config).unwrap(), saved);
        }
        for newer in [serde_json::json!("EntraDeviceCode"), serde_json::json!({"EntraServicePrincipal":{"clientId":"x"}}), serde_json::json!(7)] {
            saved["sqlServerAuth"] = newer.clone();
            assert_eq!(serde_json::from_value::<ConnectionConfig>(saved.clone()).unwrap().sql_server_auth, Some(SqlServerAuth::Unknown), "{newer}");
        }
    }

    /// The hand-written reader defaults these fields for old rows; the wire type must still require them.
    #[test]
    fn bindings_require_the_fields_old_rows_default() {
        let bindings = include_str!("../../../src/ipc/bindings.ts");
        let block = |name: &str| {
            let start = bindings.find(&format!("export type {name} = {{")).unwrap_or_else(|| panic!("{name} is not in bindings.ts"));
            &bindings[start..start + bindings[start..].find("};").unwrap()]
        };
        assert!(block("ConnectionConfig").contains("\tssh: SshTunnel | null,"), "{}", block("ConnectionConfig"));
        assert!(block("ConnectionDraft").contains("\tssh: SshTunnelDraft | null,"), "{}", block("ConnectionDraft"));
        assert!(block("ConnectionConfig").contains("\tenvironment: Environment | null,"), "{}", block("ConnectionConfig"));
        assert!(block("ConnectionDraft").contains("\tenvironment: Environment | null,"), "{}", block("ConnectionDraft"));
        assert!(block("ConnectionConfig").contains("\tsqlServerAuth: SqlServerAuth | null,"), "{}", block("ConnectionConfig"));
        assert!(block("ConnectionDraft").contains("\tsqlServerAuth: SqlServerAuth | null,"), "{}", block("ConnectionDraft"));
    }
}
