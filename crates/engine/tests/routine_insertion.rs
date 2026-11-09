#![cfg(any(feature = "mssql-tests", feature = "pg-tests"))]

use engine::model::*;
use engine::{AgentPath, Engine, EngineConfig, MemoryKeyStore, draft_from_url};
use futures_util::FutureExt;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

type Rows = Vec<Vec<Option<String>>>;

struct Harness {
    engine: Arc<Engine>,
    db: DatabaseRef,
    tab: TabId,
    _dir: tempfile::TempDir,
}

impl Harness {
    async fn open(kind: DatabaseKind) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let engine =
            Engine::open(EngineConfig { data_dir: dir.path().into(), keys: Arc::new(MemoryKeyStore::default()), agent_path: AgentPath::Fixed("".into()), az_binary: None, user_known_hosts: None, updates: Arc::new(engine::ScriptedTransport::named("off").unwrap()) })
                .await
                .unwrap();
        let draft = match kind {
            DatabaseKind::Postgres => {
                draft_from_url(&std::env::var("L1_DATA_TEST_PG").unwrap_or_else(|_| "postgres://l1data:l1data@localhost:5433/shop".into()), "Routine insertion")
                    .unwrap()
            }
            DatabaseKind::SqlServer => ConnectionDraft {
                database_kind: kind,
                id: None,
                name: "Routine insertion".into(),
                host: std::env::var("L1_DATA_TEST_MSSQL_HOST").unwrap_or_else(|_| "localhost".into()),
                port: std::env::var("L1_DATA_TEST_MSSQL_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(1434),
                database: "shop".into(),
                user: "sa".into(),
                password: PasswordInput::Set(std::env::var("L1_DATA_TEST_MSSQL_PASSWORD").unwrap_or_else(|_| "L1Data_dev_Str0ng".into())),
                ssl_mode: SslMode::Require,
                ca_cert_path: None,
                color: None,
                ssh: None,
                environment: None,
                sql_server_auth: None,
            },
        };
        let config = engine.save_connection(draft).await.unwrap();
        engine.connect(config.id).await.unwrap();
        let db = DatabaseRef { connection_id: config.id, database: config.database };
        let tab = TabId(uuid::Uuid::new_v4());
        engine
            .console_configure(tab, ConsoleConfig { connection_id: config.id, database: db.database.clone(), schema: None, mode: TxnMode::AutoCommit })
            .await
            .unwrap();
        Self { engine, db, tab, _dir: dir }
    }

    async fn run(&self, sql: &str) -> ConsoleState {
        let mut events = self.engine.subscribe();
        let id = self.engine.console_run(self.tab, RunRequest::Script { text: sql.into(), target: RunTarget::All, mode: RunMode::Run, params: Vec::new(), cells: Vec::new() }, None).await.unwrap().run_id;
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if let AppEvent::ConsoleChanged(state) = events.recv().await.unwrap()
                    && state.tab_id == self.tab
                    && matches!(state.run, RunState::Finished { run_id, .. } if run_id == id)
                {
                    return state;
                }
            }
        })
        .await
        .expect("Routine insertion query did not finish")
    }

    async fn sql(&self, sql: &str) -> Rows {
        let state = self.run(sql).await;
        assert!(matches!(state.run, RunState::Finished { outcome: RunOutcome::Ok, .. }), "{sql}: {:?}", state.run);
        let RunState::Finished { statements, .. } = state.run else { unreachable!() };
        let rows = statements
            .iter()
            .flat_map(|s| &s.outputs)
            .filter_map(|output| match output {
                RunOutput::Rows { result, .. } => Some(self.engine.result_page(result.r#ref, 0, 100).unwrap().rows),
                _ => None,
            })
            .flatten()
            .collect();
        eprintln!("{sql} => {rows:?}");
        rows
    }

    async fn cleanup(&self, statements: Vec<String>, remaining: &str) {
        let mut failures = vec![];
        for sql in statements {
            if AssertUnwindSafe(self.sql(&sql)).catch_unwind().await.is_err() {
                failures.push(sql);
            }
        }
        let checked = AssertUnwindSafe(async {
            let rows = self.sql(remaining).await;
            assert!(!rows.is_empty() && rows.iter().flatten().all(Option::is_none), "Disposable routine objects remained: {rows:?}");
        })
        .catch_unwind()
        .await;
        if checked.is_err() {
            failures.push("Routine absence check failed".into());
        }
        if let Err(error) = self.engine.console_close(self.tab, true).await {
            failures.push(format!("Close failed: {error:?}"));
        }
        if let Err(error) = self.engine.disconnect(self.db.connection_id, true).await {
            failures.push(format!("Disconnect failed: {error:?}"));
        }
        self.engine.shutdown().await;
        assert!(failures.is_empty(), "{failures:?}");
    }
}

fn routine<'a>(catalog: &'a Catalog, schema: &str, name: &str) -> &'a FunctionInfo {
    catalog.schemas.iter().find(|s| s.name == schema).unwrap().functions.iter().find(|f| f.name == name).unwrap()
}

#[cfg(feature = "mssql-tests")]
#[tokio::test]
async fn sql_server_routine_templates_execute_with_quoted_names_and_structured_arguments() {
    let h = Harness::open(DatabaseKind::SqlServer).await;
    let stem = format!("l1_routine_{}", uuid::Uuid::new_v4().simple());
    let schema = format!("{stem}] schema");
    let qualified = format!("[{stem}]] schema]");
    let other_schema = format!("{stem}_other");
    let scalar = "scalar] name";
    let procedure = "procedure] name";
    let outcome = AssertUnwindSafe(async {
        h.sql(&format!("CREATE SCHEMA {qualified}")).await;
        h.sql(&format!("CREATE SCHEMA [{other_schema}]")).await;
        h.sql(&format!("CREATE TYPE {qualified}.[items] AS TABLE (value int)")).await;
        h.sql(&format!("CREATE TYPE {qualified}.[argument,type] FROM int NULL")).await;
        for sql in [
            format!("CREATE FUNCTION {qualified}.[scalar]] name]() RETURNS int AS BEGIN RETURN 42 END"),
            format!("CREATE PROCEDURE {qualified}.[procedure]] name] AS SELECT 43 AS answer"),
            format!("CREATE FUNCTION {qualified}.[table]] name]() RETURNS TABLE AS RETURN (SELECT 44 AS answer)"),
            format!("CREATE FUNCTION {qualified}.[inputs](@first {qualified}.[argument,type], @second int) RETURNS int AS BEGIN RETURN COALESCE(@first,40)+COALESCE(@second,2) END"),
            format!("CREATE PROCEDURE {qualified}.[input_proc] @first int, @second int OUTPUT AS SELECT COALESCE(@first,40)+COALESCE(@second,3) AS answer"),
            format!("CREATE PROCEDURE {qualified}.[tvp] @items {qualified}.[items] READONLY, @input int AS SELECT COUNT(*) AS count, COALESCE(@input,9) AS input FROM @items"),
            format!("CREATE FUNCTION [{other_schema}].[scalar]] name]() RETURNS int AS BEGIN RETURN 45 END"),
        ] { h.sql(&sql).await; }
        h.engine.refresh_catalog(h.db.clone()).await.unwrap();
        let catalog = h.engine.catalog(h.db.clone()).await.unwrap();
        let f = routine(&catalog, &schema, scalar);
        let p = routine(&catalog, &schema, procedure);
        eprintln!("Public Engine catalog scalar {f:?}; procedure {p:?}");
        assert_eq!(f.kind, FnKind::Function);
        assert_eq!(p.kind, FnKind::Procedure);
        assert_eq!(f.insert_text, format!("{qualified}.[scalar]] name]()"));
        assert_eq!(p.insert_text, format!("EXEC {qualified}.[procedure]] name]"));
        assert_eq!(h.sql(&format!("SELECT {}", f.insert_text)).await, vec![vec![Some("42".into())]]);
        assert_eq!(h.sql(&p.insert_text).await, vec![vec![Some("43".into())]]);
        let table = routine(&catalog, &schema, "table] name");
        assert_eq!(table.insert_text, format!("{qualified}.[table]] name]()"));
        assert_eq!(h.sql(&format!("SELECT * FROM {}", table.insert_text)).await, vec![vec![Some("44".into())]]);
        let inputs = routine(&catalog, &schema, "inputs");
        assert_eq!(inputs.args, "@first argument,type, @second int");
        assert_eq!(inputs.insert_text, format!("{qualified}.[inputs](NULL, NULL)"));
        assert_eq!(h.sql(&format!("SELECT {}", inputs.insert_text)).await, vec![vec![Some("42".into())]]);
        let input_proc = routine(&catalog, &schema, "input_proc");
        assert_eq!(input_proc.args, "@first int, @second int OUTPUT");
        assert_eq!(input_proc.insert_text, format!("EXEC {qualified}.[input_proc] NULL, NULL"));
        assert_eq!(h.sql(&input_proc.insert_text).await, vec![vec![Some("43".into())]]);
        let tvp = routine(&catalog, &schema, "tvp");
        assert_eq!(tvp.insert_text, format!("EXEC {qualified}.[tvp] @table_argument_1, NULL"));
        assert_eq!(h.sql(&format!("DECLARE @table_argument_1 {qualified}.[items]; {}", tvp.insert_text)).await, vec![vec![Some("0".into()), Some("9".into())]]);
        let other = routine(&catalog, &other_schema, scalar);
        assert_ne!(other.oid, f.oid);
        assert_eq!(other.insert_text, format!("[{other_schema}].[scalar]] name]()"));
        assert_eq!(h.sql(&format!("SELECT {}", other.insert_text)).await, vec![vec![Some("45".into())]]);
    }).catch_unwind().await;
    h.cleanup(
        vec![
            format!("DROP FUNCTION IF EXISTS {qualified}.[scalar]] name]"),
            format!("DROP PROCEDURE IF EXISTS {qualified}.[procedure]] name]"),
            format!("DROP FUNCTION IF EXISTS {qualified}.[table]] name]"),
            format!("DROP FUNCTION IF EXISTS {qualified}.[inputs]"),
            format!("DROP PROCEDURE IF EXISTS {qualified}.[input_proc]"),
            format!("DROP PROCEDURE IF EXISTS {qualified}.[tvp]"),
            format!("DROP FUNCTION IF EXISTS [{other_schema}].[scalar]] name]"),
            format!("IF TYPE_ID(N'{qualified}.[argument,type]') IS NOT NULL DROP TYPE {qualified}.[argument,type]"),
            format!("IF TYPE_ID(N'{qualified}.[items]') IS NOT NULL DROP TYPE {qualified}.[items]"),
            format!("IF SCHEMA_ID(N'{other_schema}') IS NOT NULL DROP SCHEMA [{other_schema}]"),
            format!("IF SCHEMA_ID(N'{schema}') IS NOT NULL DROP SCHEMA {qualified}"),
        ],
        &format!("SELECT OBJECT_ID(N'{qualified}.[scalar]] name]'), OBJECT_ID(N'{qualified}.[procedure]] name]'), OBJECT_ID(N'{qualified}.[table]] name]'), OBJECT_ID(N'{qualified}.[inputs]'), OBJECT_ID(N'{qualified}.[input_proc]'), OBJECT_ID(N'{qualified}.[tvp]'), OBJECT_ID(N'[{other_schema}].[scalar]] name]'), TYPE_ID(N'{qualified}.[items]'), TYPE_ID(N'{qualified}.[argument,type]'), SCHEMA_ID(N'{schema}'), SCHEMA_ID(N'{other_schema}')"),
    ).await;
    if let Err(error) = outcome {
        std::panic::resume_unwind(error);
    }
}

#[cfg(feature = "pg-tests")]
#[tokio::test]
async fn postgres_keeps_existing_routine_insertion_for_functions_procedures_and_overloads() {
    let h = Harness::open(DatabaseKind::Postgres).await;
    let schema = format!("l1_routine_{}", uuid::Uuid::new_v4().simple());
    let outcome = AssertUnwindSafe(async {
        h.sql(&format!("CREATE SCHEMA {schema}")).await;
        h.sql(&format!("CREATE FUNCTION {schema}.answer() RETURNS int LANGUAGE sql AS 'SELECT 42'")).await;
        h.sql(&format!("CREATE FUNCTION {schema}.answer(value int) RETURNS int LANGUAGE sql AS 'SELECT value'")).await;
        h.sql(&format!("CREATE PROCEDURE {schema}.do_nothing() LANGUAGE sql AS 'SELECT 43'")).await;
        h.engine
            .console_configure(
                h.tab,
                ConsoleConfig { connection_id: h.db.connection_id, database: h.db.database.clone(), schema: Some(schema.clone()), mode: TxnMode::AutoCommit },
            )
            .await
            .unwrap();
        h.engine.refresh_catalog(h.db.clone()).await.unwrap();
        let catalog = h.engine.catalog(h.db.clone()).await.unwrap();
        let routines = &catalog.schemas.iter().find(|s| s.name == schema).unwrap().functions;
        let overloads = routines.iter().filter(|f| f.name == "answer").collect::<Vec<_>>();
        assert_eq!(overloads.len(), 2);
        assert_ne!(overloads[0].oid, overloads[1].oid);
        assert!(overloads.iter().all(|f| f.kind == FnKind::Function && f.insert_text == "answer()"));
        assert!(overloads.iter().any(|f| f.args.is_empty()));
        assert!(overloads.iter().any(|f| f.args == "value integer"));
        assert_eq!(h.sql(&format!("SELECT {}", overloads[0].insert_text)).await, vec![vec![Some("42".into())]]);
        let procedure = routine(&catalog, &schema, "do_nothing");
        assert_eq!(procedure.kind, FnKind::Procedure);
        assert_eq!(procedure.insert_text, "do_nothing()");
        eprintln!("PostgreSQL public Engine catalog routines {routines:?}");
    })
    .catch_unwind()
    .await;
    h.cleanup(vec![format!("DROP SCHEMA IF EXISTS {schema} CASCADE")],
        &format!("SELECT to_regprocedure('{schema}.answer()'), to_regprocedure('{schema}.answer(integer)'), to_regprocedure('{schema}.do_nothing()'), to_regnamespace('{schema}')")
    ).await;
    if let Err(error) = outcome {
        std::panic::resume_unwind(error);
    }
}

#[cfg(feature = "pg-tests")]
#[tokio::test]
async fn postgres_catalog_quotes_only_the_names_that_need_it() {
    let h = Harness::open(DatabaseKind::Postgres).await;
    let schema = format!("L1 Quote {}", uuid::Uuid::new_v4().simple());
    let outcome = AssertUnwindSafe(async {
        h.sql(&format!("CREATE SCHEMA \"{schema}\"")).await;
        h.sql(&format!("CREATE TABLE \"{schema}\".\"Line Items\" (\"order\" int, \"MixedCase\" int, plain int)")).await;
        h.sql(&format!("CREATE FUNCTION \"{schema}\".\"MyFunc\"() RETURNS int LANGUAGE sql AS 'SELECT 7'")).await;
        h.engine.refresh_catalog(h.db.clone()).await.unwrap();
        let catalog = h.engine.catalog(h.db.clone()).await.unwrap();
        let s = catalog.schemas.iter().find(|s| s.name == schema).unwrap();
        let r = s.relations.iter().find(|r| r.name == "Line Items").unwrap();
        let f = routine(&catalog, &schema, "MyFunc");
        assert_eq!((s.quoted.as_str(), r.quoted.as_str()), (format!("\"{schema}\"").as_str(), "\"Line Items\""));
        assert_eq!(r.columns.iter().map(|c| c.quoted.as_str()).collect::<Vec<_>>(), ["\"order\"", "\"MixedCase\"", "plain"]);
        assert_eq!((f.quoted.as_str(), f.insert_text.as_str()), ("\"MyFunc\"", "\"MyFunc\"()"));
        assert_eq!(h.sql(&format!("SELECT {}.{}", s.quoted, f.insert_text)).await, vec![vec![Some("7".into())]]);
        let columns = r.columns.iter().map(|c| format!("t.{}", c.quoted)).collect::<Vec<_>>().join(", ");
        h.sql(&format!("INSERT INTO {}.{} VALUES (1, 2, 3)", s.quoted, r.quoted)).await;
        assert_eq!(h.sql(&format!("SELECT {columns} FROM {}.{} t", s.quoted, r.quoted)).await, vec![vec![Some("1".into()), Some("2".into()), Some("3".into())]]);
    })
    .catch_unwind()
    .await;
    h.cleanup(vec![format!("DROP SCHEMA IF EXISTS \"{schema}\" CASCADE")], &format!("SELECT to_regnamespace('\"{schema}\"')")).await;
    if let Err(error) = outcome {
        std::panic::resume_unwind(error);
    }
}

#[cfg(feature = "mssql-tests")]
#[tokio::test]
async fn sql_server_catalog_brackets_only_the_names_that_need_it() {
    let h = Harness::open(DatabaseKind::SqlServer).await;
    let schema = format!("l1_quote_{}", uuid::Uuid::new_v4().simple());
    let outcome = AssertUnwindSafe(async {
        h.sql(&format!("CREATE SCHEMA [{schema}]")).await;
        h.sql(&format!("CREATE TABLE [{schema}].[order] ([select] int, Plain int, [two words] int)")).await;
        h.engine.refresh_catalog(h.db.clone()).await.unwrap();
        let catalog = h.engine.catalog(h.db.clone()).await.unwrap();
        let s = catalog.schemas.iter().find(|s| s.name == schema).unwrap();
        let r = s.relations.iter().find(|r| r.name == "order").unwrap();
        assert_eq!((s.quoted.as_str(), r.quoted.as_str()), (schema.as_str(), "[order]"));
        assert_eq!(r.columns.iter().map(|c| c.quoted.as_str()).collect::<Vec<_>>(), ["[select]", "Plain", "[two words]"]);
        let columns = r.columns.iter().map(|c| format!("t.{}", c.quoted)).collect::<Vec<_>>().join(", ");
        h.sql(&format!("INSERT INTO {}.{} VALUES (1, 2, 3)", s.quoted, r.quoted)).await;
        assert_eq!(h.sql(&format!("SELECT {columns} FROM {}.{} t", s.quoted, r.quoted)).await, vec![vec![Some("1".into()), Some("2".into()), Some("3".into())]]);
    })
    .catch_unwind()
    .await;
    h.cleanup(
        vec![format!("DROP TABLE IF EXISTS [{schema}].[order]"), format!("IF SCHEMA_ID(N'{schema}') IS NOT NULL DROP SCHEMA [{schema}]")],
        &format!("SELECT SCHEMA_ID(N'{schema}')"),
    )
    .await;
    if let Err(error) = outcome {
        std::panic::resume_unwind(error);
    }
}
