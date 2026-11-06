use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use serde_json::{Value, json};

use super::reply;
use crate::connection::{ConnectParams, connect_error_text};
use crate::model::{AssistantAccess, ApprovalReason, Catalog, ConnStatus, DatabaseKind, DatabaseRef, KeyKind, Permission, RelKind, Relation, SchemaInfo, ToolResult, ToolStatus, ToolUse};
use crate::access::AccessLease;
use crate::provider::{ModelResult, ToolSpec};
use crate::session::{CancelHandle, Session, QueryEvent};
use crate::{Shared, sql};

pub(crate) static TOOLS: [ToolSpec; 3] = [
    ToolSpec {
        name: "describe_tables",
        description: "Columns, keys, and indexes of up to 10 tables, as `name` or `schema.name`. Never needs approval.",
        input_schema: || json!({ "type": "object", "properties": { "tables": { "type": "array", "items": { "type": "string" }, "minItems": 1, "maxItems": 10 } }, "required": ["tables"], "additionalProperties": false }),
    },
    ToolSpec {
        name: "search_catalog",
        description: "Tables, columns, and functions whose name contains the query, up to 50. Never needs approval.",
        input_schema: || json!({ "type": "object", "properties": { "query": { "type": "string", "minLength": 1 } }, "required": ["query"], "additionalProperties": false }),
    },
    ToolSpec {
        name: "run_query",
        description: "Runs one read-only statement (SELECT, WITH, VALUES, TABLE, SHOW, EXPLAIN) in a READ ONLY transaction with a 30 s timeout and returns up to 50 rows. The user may have to approve it; `purpose` is shown to them.",
        input_schema: || json!({ "type": "object", "properties": { "sql": { "type": "string", "minLength": 1 }, "purpose": { "type": "string" } }, "required": ["sql", "purpose"], "additionalProperties": false }),
    },
];

pub(crate) const TOOL_ROWS: usize = 50;
pub(crate) const SCHEMA_REFUSAL: &str = "Schema only access does not allow queries. Use the schema tools and suggest SQL for the user to run.";

pub(crate) fn offered(access: AssistantAccess) -> &'static [ToolSpec] {
    if access == AssistantAccess::SchemaOnly { &TOOLS[..2] } else { &TOOLS }
}
pub(crate) const TOOL_CELL_CHARS: usize = 200;
pub(crate) const TOOL_BYTES: usize = 8 << 10;
pub(crate) const QUERY_TIMEOUT: Duration = Duration::from_secs(30);
const LOCK_TIMEOUT_MS: u32 = 5_000;
const MAX_TABLES: usize = 10;
const MAX_MATCHES: usize = 50;

pub(crate) const REFUSED_WRITE: &str = "Only SELECT, WITH, VALUES, TABLE, SHOW, and EXPLAIN run here. Put a write in a ```sql block for the user to run.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ParsedCall {
    DescribeTables { tables: Vec<String> },
    SearchCatalog { query: String },
    RunQuery { sql: String, purpose: String },
    Invalid { name: String, message: String },
}

pub(crate) fn parse_call(name: &str, input: &Value) -> ParsedCall {
    let invalid = |message: &str| ParsedCall::Invalid { name: name.to_owned(), message: message.to_owned() };
    let text = |key: &str| input[key].as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned);
    match name {
        "describe_tables" => {
            let tables: Vec<String> = input["tables"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned).collect();
            if tables.is_empty() { invalid("describe_tables needs `tables`, a list of 1 to 10 names.") } else { ParsedCall::DescribeTables { tables: tables.into_iter().take(MAX_TABLES).collect() } }
        }
        "search_catalog" => match text("query") {
            Some(query) => ParsedCall::SearchCatalog { query },
            None => invalid("search_catalog needs `query`."),
        },
        "run_query" => match text("sql") {
            Some(sql) => ParsedCall::RunQuery { sql, purpose: text("purpose").unwrap_or_default() },
            None => invalid("run_query needs `sql`."),
        },
        _ => invalid(&format!("There is no tool named {name}.")),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Ask {
    Allow,
    Refuse(String),
    User(ApprovalReason),
}

pub(crate) const MSSQL_REFUSAL: &str = "SQL Server model queries are disabled because server-enforced read-only transactions are unavailable. Use the schema tools and suggest T-SQL for the user to run.";

pub(crate) fn policy(call: &ParsedCall, permission: Permission, database_kind: Option<DatabaseKind>, access: AssistantAccess) -> Ask {
    if access == AssistantAccess::SchemaOnly && matches!(call, ParsedCall::RunQuery { .. }) {
        return Ask::Refuse(SCHEMA_REFUSAL.into());
    }
    if matches!(call,ParsedCall::RunQuery { .. }) && database_kind == Some(DatabaseKind::SqlServer) {
        return Ask::Refuse(MSSQL_REFUSAL.into());
    }
    match call {
        ParsedCall::DescribeTables { .. } | ParsedCall::SearchCatalog { .. } => Ask::Allow,
        ParsedCall::Invalid { message, .. } => Ask::Refuse(message.clone()),
        ParsedCall::RunQuery { sql, .. } => match sql::vet(sql) {
            sql::Vet::NotARead => Ask::Refuse(REFUSED_WRITE.into()),
            sql::Vet::Unvetted(functions) => Ask::User(ApprovalReason::Unvetted { functions }),
            sql::Vet::Clear => match permission {
                Permission::AskFirst => Ask::User(ApprovalReason::AskFirst),
                Permission::Auto => Ask::Allow,
            },
        },
    }
}

pub(crate) fn describe(name: &str, input: &Value, catalog: Option<&Catalog>, kind: DatabaseKind) -> ToolUse {
    match parse_call(name, input) {
        ParsedCall::DescribeTables { tables } => ToolUse::DescribeTables { tables },
        ParsedCall::SearchCatalog { query } => ToolUse::SearchCatalog { query },
        ParsedCall::RunQuery { sql, purpose } => {
            let relations = reply::uses(&sql, catalog, kind);
            ToolUse::RunQuery { sql, purpose, relations }
        }
        ParsedCall::Invalid { name, .. } => ToolUse::Other { name },
    }
}

pub(crate) struct Executed {
    pub for_model: ModelResult,
    pub status: ToolStatus,
}

impl Executed {
    pub(crate) fn failed(message: impl Into<String>) -> Self {
        let message = message.into();
        Self { for_model: ModelResult { text: message.clone(), is_error: true }, status: ToolStatus::Failed { message } }
    }
}

pub(crate) async fn execute(shared: &Arc<Shared>, db: Option<&DatabaseRef>, call: ParsedCall, access: AccessLease) -> Executed {
    if shared.access.with(access, || ()).is_none() {
        return Executed::failed("Assistant access changed. Start a new turn.");
    }
    if access.mode == AssistantAccess::SchemaOnly && matches!(call, ParsedCall::RunQuery { .. }) {
        return Executed::failed(SCHEMA_REFUSAL);
    }
    let executed = execute_allowed(shared, db, call, access.mode).await;
    if access.mode == AssistantAccess::SchemaOnly && executed.for_model.is_error {
        return Executed::failed("Schema information is unavailable. Ask the user to check the connection and table names.");
    }
    executed
}

async fn execute_allowed(shared: &Arc<Shared>, db: Option<&DatabaseRef>, call: ParsedCall, access: AssistantAccess) -> Executed {
    let Some(db) = db else { return Executed::failed("No database is selected. Ask the user to open a tab on a connection.") };
    let Some(instance) = shared.instance(db.connection_id) else { return Executed::failed(not_connected(shared, db)) };
    match call {
        ParsedCall::Invalid { message, .. } => Executed::failed(message),
        ParsedCall::SearchCatalog { query } => match shared.catalog(db).await {
            Ok(catalog) => search_catalog(&catalog, &query, access),
            Err(e) => Executed::failed(format!("The catalog of {} is not loaded: {e:?}", db.database)),
        },
        ParsedCall::DescribeTables { tables } => match describe_tables(shared, db, &tables, access).await {
            Ok(executed) => executed,
            Err(message) => Executed::failed(message),
        },
        ParsedCall::RunQuery { sql, .. } => {
            if instance.config.database_kind != DatabaseKind::Postgres { return Executed::failed(MSSQL_REFUSAL); }
            let params = match shared.params(&instance.config, &db.database) {
                Ok(params) => params,
                Err(e) => return Executed::failed(connect_error_text(&e)),
            };
            match run_query(shared, &params, &sql, QUERY_TIMEOUT).await {
                Ok(rows) => Executed { for_model: ModelResult { text: rows_text(&rows), is_error: false }, status: ToolStatus::Done { result: ToolResult::Rows { columns: rows.columns, rows: rows.rows, more: rows.more, elapsed_ms: rows.elapsed_ms } } },
                Err(message) => Executed::failed(message),
            }
        }
    }
}

fn not_connected(shared: &Shared, db: &DatabaseRef) -> String {
    let name = shared.config(db.connection_id).map_or_else(|| "The connection".to_owned(), |c| c.name);
    let reason = match shared.connection_state(db.connection_id).map(|s| s.status) {
        Some(ConnStatus::Failed { error }) => connect_error_text(&error),
        Some(ConnStatus::Lost { reason }) => reason,
        Some(ConnStatus::Connecting) => "still connecting".into(),
        _ => "disconnected".into(),
    };
    format!("{name} is not connected: {reason}. Ask the user to connect from the sidebar.")
}

fn search_catalog(catalog: &Catalog, query: &str, access: AssistantAccess) -> Executed {
    let needle = query.to_lowercase();
    let hit = |name: &str| name.to_lowercase().contains(&needle);
    let mut lines = Vec::new();
    for schema in &catalog.schemas {
        for r in &schema.relations {
            if hit(&r.name) {
                lines.push(format!("{} {}.{}", kind_word(r.kind), schema.name, r.name));
            }
            for c in &r.columns {
                if hit(&c.name) {
                    lines.push(format!("column {}.{}.{} {}", schema.name, r.name, c.name, c.type_display));
                }
            }
        }
        for f in &schema.functions {
            if hit(&f.name) {
                if access == AssistantAccess::Full {
                    lines.push(format!("function {}.{}({}) → {}", schema.name, f.name, f.args, f.returns));
                } else {
                    lines.push(format!("function {}.{} → {}", schema.name, f.name, f.returns));
                }
            }
        }
    }
    let matches = lines.len();
    let mut text = if matches == 0 { format!("Nothing in the catalog contains {query:?}.") } else { lines[..matches.min(MAX_MATCHES)].join("\n") };
    if matches > MAX_MATCHES {
        text.push_str(&format!("\n…{} more; narrow the query.", matches - MAX_MATCHES));
    }
    Executed { for_model: ModelResult { text: cap(text), is_error: false }, status: ToolStatus::Done { result: ToolResult::Found { matches: matches.min(u16::MAX as usize) as u16 } } }
}

fn kind_word(kind: RelKind) -> &'static str {
    match kind {
        RelKind::Table | RelKind::PartitionedTable => "table",
        RelKind::View => "view",
        RelKind::MaterializedView => "materialized view",
        RelKind::ForeignTable => "foreign table",
    }
}

async fn describe_tables(shared: &Arc<Shared>, db: &DatabaseRef, tables: &[String], access: AssistantAccess) -> Result<Executed, String> {
    let catalog = shared.catalog(db).await.map_err(|e| format!("The catalog of {} is not loaded: {e:?}", db.database))?;
    let (instance, slot) = shared.slot(db).map_err(|e| format!("The catalog of {} is not loaded: {e:?}", db.database))?;
    let mut meta = slot.meta.lock().await;
    let session = shared.session_in(&instance, &slot, &mut meta).await.map_err(|e| connect_error_text(&e))?;
    let mut found = Vec::new();
    let mut missing = Vec::new();
    let mut text = String::new();
    for name in tables {
        let Some((schema, relation)) = find_relation(&catalog, name) else {
            missing.push(name.clone());
            continue;
        };
        let detail = session.detail(db.clone(), relation.oid, catalog.version).await.map_err(|e| e.message)?;
        let qualified = format!("{schema}.{}", relation.name);
        let rows = if access == AssistantAccess::Full { relation.row_estimate.map(|n| format!(", ~{n} rows")).unwrap_or_default() } else { String::new() };
        text.push_str(&format!("{qualified} ({}{rows})\n", kind_word(relation.kind)));
        let fks: Vec<(String, String)> = detail
            .iter()
            .flat_map(|d| d.keys.iter())
            .filter_map(|k| match &k.kind {
                KeyKind::Foreign { ref_schema, ref_table, ref_columns, .. } => Some(k.columns.iter().zip(ref_columns).map(move |(c, r)| (c.clone(), format!("{ref_schema}.{ref_table}({r})")))),
                _ => None,
            })
            .flatten()
            .collect();
        for c in &relation.columns {
            let mut line = format!("  {} {}", c.name, c.type_display);
            if c.not_null {
                line.push_str(" NOT NULL");
            }
            if c.pk {
                line.push_str(" PK");
            }
            for (_, target) in fks.iter().filter(|(col, _)| col == &c.name) {
                line.push_str(&format!(" FK → {target}"));
            }
            text.push_str(&line);
            text.push('\n');
        }
        if let Some(d) = &detail
            && !d.indexes.is_empty()
        {
            let indexes: Vec<String> = d
                .indexes
                .iter()
                .map(|i| {
                    let mut s = if access == AssistantAccess::Full { format!("{} {}", i.name, i.keys) } else { i.name.clone() };
                    if i.unique && !i.primary {
                        s.push_str(" unique");
                    }
                    if access == AssistantAccess::Full && let Some(p) = &i.predicate {
                        s.push_str(&format!(" WHERE {p}"));
                    }
                    s
                })
                .collect();
            text.push_str(&format!("  indexes: {}\n", indexes.join(", ")));
        }
        found.push(qualified);
    }
    if !missing.is_empty() {
        text.push_str(&format!("Not found: {}. Use search_catalog to find the name.\n", missing.join(", ")));
    }
    Ok(Executed { for_model: ModelResult { text: cap(text), is_error: found.is_empty() }, status: ToolStatus::Done { result: ToolResult::Described { found, missing } } })
}

fn find_relation<'a>(catalog: &'a Catalog, name: &str) -> Option<(&'a str, &'a Relation)> {
    let unquote = |s: &str| s.trim_matches('"').to_owned();
    let (schema, table) = match name.split_once('.') {
        Some((s, t)) => (Some(unquote(s)), unquote(t)),
        None => (None, unquote(name)),
    };
    let in_schema = |s: &'a SchemaInfo| s.relations.iter().find(|r| r.name == table).map(|r| (s.name.as_str(), r));
    match schema {
        Some(schema) => catalog.schemas.iter().find(|s| s.name == schema).and_then(in_schema),
        None => {
            let default = catalog.default_schema.as_deref();
            catalog.schemas.iter().filter(|s| Some(s.name.as_str()) == default).find_map(in_schema).or_else(|| catalog.schemas.iter().filter(|s| !s.system).chain(catalog.schemas.iter().filter(|s| s.system)).find_map(in_schema))
        }
    }
}

#[derive(Debug)]
pub(crate) struct RowsOut {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
    pub more: bool,
    pub elapsed_ms: u32,
}

/// Fresh session, READ ONLY transaction, Postgres's Parse as the single-statement gate, text
/// output. Dropping the session discards the transaction and ends a result the engine stopped
/// reading; a cancel request goes out when the future is dropped before the query finished.
pub(crate) async fn run_query(shared: &Shared, params: &ConnectParams, sql: &str, statement_timeout: Duration) -> Result<RowsOut, String> {
    let mut session = Session::open_assistant(params, &shared.timezone).await.map_err(|e| connect_error_text(&e))?;
    let mut guard = CancelOnDrop { shared, handle: Some(session.cancel_handle()) };
    session
        .batch(&format!("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY; SET LOCAL statement_timeout = {}; SET LOCAL lock_timeout = {LOCK_TIMEOUT_MS}", statement_timeout.as_millis()))
        .await
        .map_err(|e| query_error_text(&e))?;
    let mut columns = Vec::new();
    let started = Instant::now();
    let mut stream = session.query(sql);
    let mut rows: Vec<Vec<Option<String>>> = Vec::new();
    let mut bytes = 0;
    let mut more = false;
    while let Some(message) = stream.next().await {
        let event = message.map_err(|e| query_error_text(&e))?;
        if let QueryEvent::Columns(cols) = event { columns = cols.into_iter().map(|c| c.name).collect(); continue; }
        let QueryEvent::Row(row) = event else { continue };
        let cells: Vec<Option<String>> = row.iter().map(|c| c.as_ref().map(|c| super::context::cut(c, TOOL_CELL_CHARS))).collect();
        let size = row_bytes(&cells);
        if rows.len() == TOOL_ROWS || (!rows.is_empty() && bytes + size > TOOL_BYTES) {
            more = true;
            break;
        }
        let cells = if size > TOOL_BYTES { fit(cells, TOOL_BYTES) } else { cells };
        bytes += row_bytes(&cells);
        rows.push(cells);
    }
    if !more {
        guard.handle = None;
    }
    Ok(RowsOut { columns, rows, more, elapsed_ms: started.elapsed().as_millis().min(u32::MAX as u128) as u32 })
}

struct CancelOnDrop<'a> {
    shared: &'a Shared,
    handle: Option<CancelHandle>,
}

impl Drop for CancelOnDrop<'_> {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            self.shared.spawn(async move { handle.cancel().await });
        }
    }
}

fn query_error_text(e: &crate::model::QueryError) -> String {
    let mut text = e.message.clone();
    if let Some(detail) = &e.detail { text.push_str(&format!("\nDETAIL: {detail}")); }
    if let Some(hint) = &e.hint { text.push_str(&format!("\nHINT: {hint}")); }
    if let Some(position) = e.position { text.push_str(&format!("\nat character {}", position.0 + 1)); }
    text
}

fn rows_text(rows: &RowsOut) -> String {
    let mut text = rows.columns.join("\t");
    text.push('\n');
    for row in &rows.rows {
        let line: Vec<&str> = row.iter().map(|c| c.as_deref().unwrap_or("NULL")).collect();
        text.push_str(&line.join("\t"));
        text.push('\n');
    }
    if rows.more {
        text.push_str(&format!("{}+ rows; more not shown", rows.rows.len()));
    } else {
        text.push_str(&format!("{} rows", rows.rows.len()));
    }
    text.push_str(&format!(" · {} ms", rows.elapsed_ms));
    cap(text)
}

fn row_bytes(cells: &[Option<String>]) -> usize {
    cells.iter().map(|c| c.as_ref().map_or(4, String::len) + 1).sum()
}

/// A first row that alone would cross the budget, each cell cut to an equal share of it.
fn fit(cells: Vec<Option<String>>, budget: usize) -> Vec<Option<String>> {
    let share = (budget / cells.len().max(1)).saturating_sub(1);
    cells.into_iter().map(|cell| cell.map(|text| cut_bytes(&text, share))).collect()
}

fn cap(text: String) -> String {
    cut_bytes(&text, TOOL_BYTES)
}

/// At most `max` bytes, the ellipsis included, cut at a char boundary.
fn cut_bytes(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max.saturating_sub('…'.len_utf8());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    fn policy(call: &ParsedCall, permission: Permission) -> Ask { super::policy(call,permission,Some(DatabaseKind::Postgres), AssistantAccess::Full) }
    #[test]
    fn schema_search_omits_free_form_function_arguments() {
        let catalog = Catalog {
            database_kind: DatabaseKind::Postgres,
            db: DatabaseRef { connection_id: crate::model::ConnectionId(uuid::Uuid::nil()), database: "shop".into() },
            version: 1, default_schema: Some("public".into()), extensions: Vec::new(),
            schemas: vec![SchemaInfo { name: "public".into(), quoted: "public".into(), system: false, table_count: 0, relations: Vec::new(), sequences: Vec::new(), types: Vec::new(), synonyms: Vec::new(), functions: vec![crate::model::FunctionInfo { oid: 1, name: "lookup".into(), quoted: "lookup".into(), args: "secret text DEFAULT 'DEFAULT_CANARY'".into(), returns: "bigint".into(), kind: crate::model::FnKind::Function, insert_text: "lookup()".into() }] }],
        };
        let schema = search_catalog(&catalog, "lookup", AssistantAccess::SchemaOnly).for_model.text;
        assert_eq!(schema, "function public.lookup → bigint");
        assert!(search_catalog(&catalog, "lookup", AssistantAccess::Full).for_model.text.contains("DEFAULT_CANARY"));
    }
    #[tokio::test]
    async fn schema_only_refuses_query_at_policy_and_execution_without_a_database() {
        let dir = tempfile::tempdir().unwrap();
        let engine = crate::Engine::open(crate::EngineConfig { data_dir: dir.path().into(), keys: Arc::new(crate::MemoryKeyStore::default()), agent_path: crate::AgentPath::Fixed("".into()), az_binary: None, user_known_hosts: None, updates: Arc::new(crate::ScriptedTransport::named("off").unwrap()) }).await.unwrap();
        let access = *engine.shared.access.lock();
        let call = parse_call("run_query", &json!({ "sql": "select 'DATA_CANARY'" }));
        for permission in [Permission::Auto, Permission::AskFirst] {
            assert_eq!(super::policy(&call, permission, None, AssistantAccess::SchemaOnly), Ask::Refuse(SCHEMA_REFUSAL.into()));
        }
        assert_eq!(offered(AssistantAccess::SchemaOnly).iter().map(|s| s.name).collect::<Vec<_>>(), ["describe_tables", "search_catalog"]);
        assert_eq!(execute(&engine.shared, None, call.clone(), access).await.for_model.text, SCHEMA_REFUSAL);
        engine.set_settings(crate::model::Settings { assistant_access: AssistantAccess::Full, ..engine.settings() });
        let stale = execute(&engine.shared, None, call, access).await;
        assert!(stale.for_model.is_error && stale.for_model.text.contains("access changed"));
    }
    #[test]
    fn sql_server_queries_are_refused_before_approval_in_every_permission_mode() {
        let call = parse_call("run_query", &json!({"sql":"SELECT 1"}));
        for permission in [Permission::Auto,Permission::AskFirst] {
            assert_eq!(super::policy(&call,permission,Some(DatabaseKind::SqlServer), AssistantAccess::Full),Ask::Refuse(MSSQL_REFUSAL.into()));
        }
        assert_eq!(super::policy(&parse_call("search_catalog",&json!({"query":"orders"})),Permission::Auto,Some(DatabaseKind::SqlServer), AssistantAccess::Full),Ask::Allow);
    }

    #[cfg(feature = "mssql-tests")]
    #[tokio::test]
    async fn sql_server_execution_boundary_refuses_queries_but_schema_tools_work() {
        let dir = tempfile::tempdir().unwrap();
        let engine = crate::Engine::open(crate::EngineConfig { data_dir: dir.path().into(), keys: Arc::new(crate::MemoryKeyStore::default()), agent_path: crate::AgentPath::Fixed("".into()), az_binary: None, user_known_hosts: None, updates: Arc::new(crate::ScriptedTransport::named("off").unwrap()) }).await.unwrap();
        let mut draft = crate::draft_from_url("mssql://sa@localhost:1434/shop?sslmode=require", "tools test").unwrap();
        draft.host = std::env::var("L1_DATA_TEST_MSSQL_HOST").unwrap_or(draft.host);
        draft.port = std::env::var("L1_DATA_TEST_MSSQL_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(draft.port);
        draft.password = crate::model::PasswordInput::Set(std::env::var("L1_DATA_TEST_MSSQL_PASSWORD").unwrap_or_else(|_| "L1Data_dev_Str0ng".into()));
        let config = engine.save_connection(draft).await.unwrap();
        engine.connect(config.id).await.unwrap();
        let db = DatabaseRef { connection_id: config.id, database: "shop".into() };
        engine.set_settings(crate::model::Settings { assistant_access: AssistantAccess::Full, ..engine.settings() });
        let access = *engine.shared.access.lock();
        let refused = execute(&engine.shared, Some(&db), parse_call("run_query", &json!({"sql":"SELECT 1"})), access).await;
        assert_eq!(refused.for_model.text, MSSQL_REFUSAL);
        assert!(refused.for_model.is_error);
        for (name, input) in [("search_catalog",json!({"query":"orders"})),("describe_tables",json!({"tables":["dbo.orders"]}))] {
            let result = execute(&engine.shared, Some(&db), parse_call(name, &input), access).await;
            assert!(!result.for_model.is_error, "{}", result.for_model.text);
            assert!(matches!(result.status, ToolStatus::Done { .. }));
            assert!(result.for_model.text.contains("orders"));
        }
    }

    #[test]
    fn calls_parse_once_and_bad_input_is_named() {
        assert_eq!(parse_call("describe_tables", &json!({ "tables": ["orders", " customers "] })), ParsedCall::DescribeTables { tables: vec!["orders".into(), "customers".into()] });
        assert_eq!(parse_call("describe_tables", &json!({ "tables": [] })), ParsedCall::Invalid { name: "describe_tables".into(), message: "describe_tables needs `tables`, a list of 1 to 10 names.".into() });
        assert_eq!(parse_call("search_catalog", &json!({ "query": "reg" })), ParsedCall::SearchCatalog { query: "reg".into() });
        assert_eq!(parse_call("run_query", &json!({ "sql": "select 1" })), ParsedCall::RunQuery { sql: "select 1".into(), purpose: String::new() });
        assert_eq!(parse_call("run_query", &json!({ "purpose": "x" })), ParsedCall::Invalid { name: "run_query".into(), message: "run_query needs `sql`.".into() });
        assert_eq!(parse_call("Bash", &json!({})), ParsedCall::Invalid { name: "Bash".into(), message: "There is no tool named Bash.".into() });
    }

    #[test]
    fn policy_refuses_a_write_before_anyone_is_asked_and_asks_per_permission() {
        let write = parse_call("run_query", &json!({ "sql": "delete from orders", "purpose": "clean up" }));
        assert_eq!(policy(&write, Permission::Auto), Ask::Refuse(REFUSED_WRITE.into()));
        let read = parse_call("run_query", &json!({ "sql": "select count(*) from orders", "purpose": "count" }));
        assert_eq!(policy(&read, Permission::AskFirst), Ask::User(ApprovalReason::AskFirst));
        assert_eq!(policy(&read, Permission::Auto), Ask::Allow);
        let unvetted = parse_call("run_query", &json!({ "sql": r#"select U&"pg\005fcancel_backend"(0), pg_terminate_backend(1)"#, "purpose": "" }));
        let reason = ApprovalReason::Unvetted { functions: vec!["U& escape".into(), r"pg\005fcancel_backend".into(), "pg_terminate_backend".into()] };
        assert_eq!(policy(&unvetted, Permission::Auto), Ask::User(reason.clone()));
        assert_eq!(policy(&unvetted, Permission::AskFirst), Ask::User(reason), "Ask first names what Auto would have asked about");
        let into = parse_call("run_query", &json!({ "sql": "select * into backup from orders", "purpose": "" }));
        assert_eq!(policy(&into, Permission::Auto), Ask::User(ApprovalReason::Unvetted { functions: vec!["SELECT INTO".into()] }));
        assert_eq!(policy(&parse_call("describe_tables", &json!({ "tables": ["a"] })), Permission::AskFirst), Ask::Allow);
        assert_eq!(policy(&parse_call("nope", &json!({})), Permission::Auto), Ask::Refuse("There is no tool named nope.".into()));
    }

    #[test]
    fn describe_builds_the_card_with_uses() {
        let tool = describe("run_query", &json!({ "sql": "select * from orders o join customers c on c.id = o.customer_id", "purpose": "join" }), None, DatabaseKind::Postgres);
        assert_eq!(tool, ToolUse::RunQuery { sql: "select * from orders o join customers c on c.id = o.customer_id".into(), purpose: "join".into(), relations: vec!["orders".into(), "customers".into()] });
        assert_eq!(describe("Bash", &json!({}), None, DatabaseKind::Postgres), ToolUse::Other { name: "Bash".into() });
    }

    #[test]
    fn rows_text_is_tsv_with_null_and_a_count_line() {
        let rows = RowsOut { columns: vec!["id".into(), "name".into()], rows: vec![vec![Some("1".into()), None]], more: false, elapsed_ms: 27 };
        assert_eq!(rows_text(&rows), "id\tname\n1\tNULL\n1 rows · 27 ms");
        let more = RowsOut { more: true, ..rows };
        assert!(rows_text(&more).ends_with("1+ rows; more not shown · 27 ms"));
        let long = cap("x".repeat(TOOL_BYTES + 10));
        assert_eq!((long.len(), long.ends_with('…')), (TOOL_BYTES, true), "the cap holds the ellipsis within the budget");
        assert_eq!(cut_bytes("héllo", 4), "h…", "the cut lands on a char boundary");
        assert_eq!(fit(vec![Some("x".repeat(100)), None, Some("y".repeat(3))], 40), vec![Some(format!("{}…", "x".repeat(9))), None, Some("yyy".into())], "each cell gets an equal share");
    }
}

#[cfg(all(test, feature = "pg-tests"))]
mod pg_tests {
    use super::*;
    use crate::model::PasswordInput;
    use crate::{AgentPath, Engine, EngineConfig, MemoryKeyStore, draft_from_url};

    const DEFAULT_URL: &str = "postgres://l1data:l1data@localhost:5433/shop";

    async fn engine() -> (Arc<Engine>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::open(EngineConfig { data_dir: dir.path().to_owned(), keys: Arc::new(MemoryKeyStore::default()), agent_path: AgentPath::Fixed("".into()), az_binary: None, user_known_hosts: None, updates: Arc::new(crate::ScriptedTransport::named("off").unwrap()) }).await.unwrap();
        (engine, dir)
    }

    fn params() -> ConnectParams {
        let url = std::env::var("L1_DATA_TEST_PG").unwrap_or_else(|_| DEFAULT_URL.into());
        let draft = draft_from_url(&url, "t").unwrap();
        let password = match &draft.password {
            PasswordInput::Set(p) => Some(p.clone()),
            _ => None,
        };
        ConnectParams::from_draft(&draft, password, None, None)
    }

    async fn query(shared: &Shared, sql: &str) -> Result<RowsOut, String> {
        run_query(shared, &params(), sql, QUERY_TIMEOUT).await
    }

    async fn scalar(shared: &Shared, sql: &str) -> String {
        query(shared, sql).await.unwrap().rows[0][0].clone().unwrap()
    }

    #[tokio::test]
    async fn postgres_alone_stops_each_write_past_the_lexer() {
        let (engine, _dir) = engine().await;
        let s = &engine.shared;
        let before = scalar(s, "select count(*) from public.orders").await;
        let insert = query(s, "insert into public.orders (number, customer_id, total) values ('X-1', 1, 1) returning id").await.unwrap_err();
        assert_eq!(insert, "cannot execute INSERT in a read-only transaction");
        let smuggled = query(s, "select 1; commit; drop table public.orders").await.unwrap_err();
        assert_eq!(smuggled, "cannot insert multiple commands into a prepared statement");
        let nextval = query(s, "select nextval(pg_get_serial_sequence('public.customers', 'id'))").await.unwrap_err();
        assert_eq!(nextval, "cannot execute nextval() in a read-only transaction");
        let same_text = query(s, "select set_config('default_transaction_read_only', 'off', false); update public.orders set status = status").await.unwrap_err();
        assert_eq!(same_text, "cannot insert multiple commands into a prepared statement");
        assert_eq!(scalar(s, "select set_config('default_transaction_read_only', 'off', false)").await, "off", "the setting changes for a session that is then dropped");
        let next_call = query(s, "update public.orders set status = status where false").await.unwrap_err();
        assert_eq!(next_call, "cannot execute UPDATE in a read-only transaction");
        assert_eq!(scalar(s, "select count(*) from public.orders").await, before);
    }

    #[tokio::test]
    async fn a_session_advisory_lock_dies_with_the_call() {
        let (engine, _dir) = engine().await;
        let s = &engine.shared;
        assert_eq!(scalar(s, "select pg_advisory_lock(424242)::text").await, "");
        let started = Instant::now();
        loop {
            let held = scalar(s, "select count(*) from pg_locks where locktype = 'advisory' and objid = 424242").await;
            if held == "0" {
                break;
            }
            assert!(started.elapsed() < Duration::from_secs(5), "the lock outlived the call");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    #[tokio::test]
    async fn the_statement_timeout_the_row_cap_and_the_cell_cut_hold() {
        let (engine, _dir) = engine().await;
        let s = &engine.shared;
        let slow = run_query(s, &params(), "select pg_sleep(40)", Duration::from_secs(1)).await.unwrap_err();
        assert_eq!(slow, "canceling statement due to statement timeout");
        let out = query(s, "select g from generate_series(1, 1000) g").await.unwrap();
        assert_eq!((out.rows.len(), out.more, out.columns.clone()), (TOOL_ROWS, true, vec!["g".into()]));
        assert_eq!(out.rows[49][0].as_deref(), Some("50"));
        let wide = query(s, "select repeat('x', 500) as wide, null::text as missing").await.unwrap();
        assert_eq!(wide.rows[0][0].as_ref().unwrap().chars().count(), TOOL_CELL_CHARS);
        assert!(wide.rows[0][0].as_ref().unwrap().ends_with('…'));
        assert_eq!(wide.rows[0][1], None);
        let heavy = query(s, "select repeat('y', 500) as wide from generate_series(1, 1000) g").await.unwrap();
        assert!(heavy.rows.len() < TOOL_ROWS && heavy.more, "the 8 KB budget stops a wide result before the row cap: {} rows", heavy.rows.len());
        let few = query(s, "select g from generate_series(1, 3) g").await.unwrap();
        assert_eq!((few.rows.len(), few.more), (3, false));
        let explain = query(s, "explain select 1").await.unwrap();
        assert_eq!(explain.columns, vec!["QUERY PLAN".to_owned()]);
    }

    #[tokio::test]
    async fn no_row_crosses_the_byte_budget_and_a_first_row_that_would_is_cut_to_fit() {
        let (engine, _dir) = engine().await;
        let s = &engine.shared;
        let bytes_of = |out: &RowsOut| out.rows.iter().flatten().map(|c| c.as_ref().map_or(4, String::len) + 1).sum::<usize>();
        let columns = |n: usize| (1..=n).map(|i| format!("repeat('x', 300) as c{i}")).collect::<Vec<_>>().join(", ");
        let stacked = query(s, &format!("select {} from generate_series(1, 3)", columns(20))).await.unwrap();
        assert_eq!((stacked.rows.len(), stacked.more), (2, true), "the third row would cross the budget");
        assert!(bytes_of(&stacked) <= TOOL_BYTES, "{} bytes", bytes_of(&stacked));
        let wide = query(s, &format!("select {}", columns(60))).await.unwrap();
        assert_eq!((wide.rows.len(), wide.more), (1, false), "the first row is kept");
        assert!(bytes_of(&wide) <= TOOL_BYTES, "{} bytes", bytes_of(&wide));
        assert!(wide.rows[0].iter().all(|c| c.as_deref().is_some_and(|c| c.ends_with('…'))), "each cell is cut to fit");
        assert!(rows_text(&wide).len() <= TOOL_BYTES, "{} bytes for the model", rows_text(&wide).len());
        let heavy = query(s, "select repeat('y', 500) as wide from generate_series(1, 1000) g").await.unwrap();
        assert!(bytes_of(&heavy) <= TOOL_BYTES && heavy.more, "{} bytes", bytes_of(&heavy));
    }

    async fn test_rows(session: &mut Session, sql: &str) -> Vec<Vec<String>> {
        let mut stream = session.query(sql);
        let mut rows = Vec::new();
        while let Some(event) = stream.next().await {
            if let QueryEvent::Row(row) = event.unwrap() { rows.push(row.into_iter().map(Option::unwrap).collect()); }
        }
        rows
    }

    #[tokio::test]
    async fn every_vetted_name_is_a_pg_catalog_function_with_no_volatile_overload_and_the_sizes_are_volatile() {
        let (engine, _dir) = engine().await;
        let mut session = Session::open_assistant(&params(), &engine.shared.timezone).await.unwrap();
        let names: Vec<&str> = sql::VETTED.iter().chain(sql::VETTED_SIZES).copied().collect();
        let rows = test_rows(&mut session, "select proname, bool_and(provolatile <> 'v') from pg_proc where pronamespace = 'pg_catalog'::regnamespace group by proname").await;
        let found: std::collections::BTreeMap<String, bool> = rows.iter().map(|r| (r[0].clone(), r[1] == "t")).collect();
        let missing: Vec<&str> = names.iter().copied().filter(|n| !found.contains_key(*n)).collect();
        assert_eq!(missing, Vec::<&str>::new(), "not in pg_catalog");
        let volatile: Vec<&str> = sql::VETTED.iter().copied().filter(|n| !found[*n]).collect();
        assert_eq!(volatile, Vec::<&str>::new(), "VETTED names with a volatile overload");
        let stable_sizes: Vec<&str> = sql::VETTED_SIZES.iter().copied().filter(|n| found[*n]).collect();
        assert_eq!(stable_sizes, Vec::<&str>::new(), "VETTED_SIZES is for the volatile size helpers only");
    }

    #[tokio::test]
    async fn every_vetted_operator_is_a_pg_catalog_operator_with_no_volatile_implementation() {
        let (engine, _dir) = engine().await;
        let mut session = Session::open_assistant(&params(), &engine.shared.timezone).await.unwrap();
        let symbols: Vec<&str> = sql::VETTED_OPERATORS.to_vec();
        let rows = test_rows(&mut session, "select o.oprname, bool_and(p.provolatile <> 'v') from pg_operator o join pg_proc p on p.oid = o.oprcode where o.oprnamespace = 'pg_catalog'::regnamespace group by o.oprname").await;
        let found: std::collections::BTreeMap<String, bool> = rows.iter().map(|r| (r[0].clone(), r[1] == "t")).collect();
        let missing: Vec<&str> = symbols.iter().copied().filter(|s| !found.contains_key(*s)).collect();
        assert_eq!(missing, Vec::<&str>::new(), "not in pg_catalog");
        let volatile: Vec<&str> = symbols.iter().copied().filter(|s| !found[*s]).collect();
        assert_eq!(volatile, Vec::<&str>::new(), "VETTED_OPERATORS symbols with a volatile implementation");
    }

    #[tokio::test]
    async fn the_keyword_lists_match_the_server() {
        let (engine, _dir) = engine().await;
        let mut session = Session::open_assistant(&params(), &engine.shared.timezone).await.unwrap();
        let rows = test_rows(&mut session, "select word, catcode::text from pg_get_keywords() order by word").await;
        let of = |codes: &[&str]| rows.iter().filter(|r| codes.contains(&r[1].as_str())).map(|r| r[0].clone()).collect::<Vec<_>>();
        assert_eq!(of(&["R"]), sql::RESERVED, "rerun dev/pg-keywords.sh against PostgreSQL 16");
        assert_eq!(of(&["T"]), sql::TYPE_OR_FUNCTION_NAME, "rerun dev/pg-keywords.sh against PostgreSQL 16");
        // The lists stay PostgreSQL 16's. A word from a newer server in NEVER_A_FUNCTION would let a
        // user's function of that name on 16 pass as syntax, while a word missing from it only asks first.
        let syntax = of(&["R", "C"]);
        let callable: Vec<&&str> = sql::NEVER_A_FUNCTION.iter().filter(|word| !syntax.iter().any(|s| s == *word)).collect();
        assert_eq!(callable, Vec::<&&str>::new(), "NEVER_A_FUNCTION words this server lets name a function");
        let version: i32 = test_rows(&mut session, "select current_setting('server_version_num')").await[0][0].parse().unwrap();
        if (160000..170000).contains(&version) {
            assert_eq!(syntax, sql::NEVER_A_FUNCTION, "rerun dev/pg-keywords.sh");
            assert_eq!(of(&["T", "U"]), sql::OTHER_KEYWORDS, "rerun dev/pg-keywords.sh");
        }
    }
}
