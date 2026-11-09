use crate::assistant::tools::{self, ParsedCall};
use crate::model::*;
use crate::{AgentPath, Engine, EngineConfig, MemoryKeyStore};
use futures_util::FutureExt;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

struct Harness {
    engine: Arc<Engine>,
    _dir: tempfile::TempDir,
}

impl Harness {
    async fn open() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let engine =
            Engine::open(EngineConfig { data_dir: dir.path().into(), keys: Arc::new(MemoryKeyStore::default()), agent_path: AgentPath::Fixed("".into()), az_binary: None, user_known_hosts: None, updates: Arc::new(crate::ScriptedTransport::named("off").unwrap()) })
                .await
                .unwrap();
        Self { engine, _dir: dir }
    }

    async fn save(&self, user: &str, password: String) -> ConnectionId {
        self.engine
            .save_connection(ConnectionDraft {
                database_kind: DatabaseKind::SqlServer,
                id: None,
                name: "SQL Server metadata visibility test".into(),
                host: std::env::var("L1_DATA_TEST_MSSQL_HOST").unwrap_or_else(|_| "localhost".into()),
                port: std::env::var("L1_DATA_TEST_MSSQL_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(1434),
                database: "shop".into(),
                user: user.into(),
                password: PasswordInput::Set(password),
                ssl_mode: SslMode::Require,
                ca_cert_path: None,
                color: None,
                ssh: None,
                environment: None,
                sql_server_auth: None,
            })
            .await
            .unwrap()
            .id
    }

    async fn connect(&self, connection: ConnectionId) -> TabId {
        self.engine.connect(connection).await.unwrap();
        let tab = TabId(uuid::Uuid::new_v4());
        self.engine
            .console_configure(tab, ConsoleConfig { connection_id: connection, database: "shop".into(), schema: None, mode: TxnMode::AutoCommit })
            .await
            .unwrap();
        tab
    }

    async fn request(&self, tab: TabId, request: RunRequest) -> ConsoleState {
        let mut events = self.engine.subscribe();
        let run_id = self.engine.console_run(tab, request, None).await.unwrap().run_id;
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if let AppEvent::ConsoleChanged(state) = events.recv().await.unwrap()
                    && state.tab_id == tab
                    && matches!(state.run, RunState::Finished { run_id: finished, .. } if finished == run_id)
                {
                    assert!(matches!(state.run, RunState::Finished { outcome: RunOutcome::Ok, .. }), "{:?}", state.run);
                    return state;
                }
            }
        })
        .await
        .expect("Metadata visibility query did not finish")
    }

    async fn sql(&self, tab: TabId, sql: &str) -> ConsoleState {
        self.request(tab, RunRequest::Script { text: sql.into(), target: RunTarget::All, mode: RunMode::Run, params: Vec::new(), cells: Vec::new() }).await
    }

    fn rows(&self, state: &ConsoleState) -> Vec<Vec<Option<String>>> {
        let RunState::Finished { statements, .. } = &state.run else { panic!("Run did not finish") };
        let result = statements
            .iter()
            .flat_map(|s| &s.outputs)
            .find_map(|o| match o {
                RunOutput::Rows { result, .. } => Some(result),
                _ => None,
            })
            .expect("Query returned no row set");
        self.engine.result_page(result.r#ref, 0, 100).unwrap().rows
    }
}

fn db(connection_id: ConnectionId) -> DatabaseRef {
    DatabaseRef { connection_id, database: "shop".into() }
}

fn assert_structure(detail: &RelationDetail, stem: &str) {
    assert!(detail.defaults.contains(&ColumnDefault { column: "id".into(), value: DefaultValue::Identity { always: true } }));
    assert!(detail.keys.contains(&KeyInfo { name: format!("{stem}_pk"), columns: vec!["id".into()], kind: KeyKind::Primary }));
    assert!(detail.keys.contains(&KeyInfo { name: format!("{stem}_uq"), columns: vec!["n".into()], kind: KeyKind::Unique }));
    assert_eq!(detail.indexes.len(), 3);
    assert!(detail.indexes.iter().any(|i| i.primary && i.unique && i.valid && i.keys == "([id])"));
    let index = detail.indexes.iter().find(|i| i.name == format!("{stem}_ix")).unwrap();
    assert_eq!(index.keys, "([n] DESC) INCLUDE ([id])");
    assert!(index.valid && !index.unique && !index.primary);
    let trigger = &detail.triggers[0];
    assert_eq!(detail.triggers.len(), 1);
    assert_eq!(trigger.name, format!("{stem}_tr"));
    assert_eq!(trigger.timing, TriggerTiming::After);
    assert_eq!(trigger.events, [TriggerEvent::Insert, TriggerEvent::Update]);
    assert!(trigger.enabled && !trigger.per_row);
}

#[tokio::test]
async fn select_only_login_keeps_relation_detail_and_actual_assistant_description() {
    let h = Harness::open().await;
    let owner = h.save("sa", std::env::var("L1_DATA_TEST_MSSQL_PASSWORD").unwrap_or_else(|_| "L1Data_dev_Str0ng".into())).await;
    let owner_tab = h.connect(owner).await;
    let stem = format!("l1_metadata_{}", uuid::Uuid::new_v4().simple());
    let table = format!("{stem}_table");
    let login = format!("{stem}_login");
    let password = format!("L1Meta_{}!", uuid::Uuid::new_v4().simple());
    let reader = h.save(&login, password.clone()).await;
    let expressions = format!(
        "
        SELECT N'check' AS kind, object_id, definition FROM sys.check_constraints WHERE parent_object_id=OBJECT_ID(N'dbo.{table}')
        UNION ALL SELECT N'computed', object_id, definition FROM sys.computed_columns WHERE object_id=OBJECT_ID(N'dbo.{table}')
        UNION ALL SELECT N'default', object_id, definition FROM sys.default_constraints WHERE parent_object_id=OBJECT_ID(N'dbo.{table}')
        ORDER BY kind"
    );

    let outcome = AssertUnwindSafe(async {
        h.sql(owner_tab, &format!("
            CREATE TABLE dbo.[{table}] (
                id int IDENTITY CONSTRAINT [{stem}_pk] PRIMARY KEY,
                n int NOT NULL CONSTRAINT [{stem}_df] DEFAULT(42) CONSTRAINT [{stem}_uq] UNIQUE,
                computed AS(id+n),
                CONSTRAINT [{stem}_ck] CHECK(n>=0)
            );
            CREATE INDEX [{stem}_ix] ON dbo.[{table}](n DESC) INCLUDE(id) WHERE n>=0;
            INSERT INTO dbo.[{table}] DEFAULT VALUES;
            CREATE LOGIN [{login}] WITH PASSWORD=N'{password}', CHECK_POLICY=OFF, DEFAULT_DATABASE=[shop];
            CREATE USER [{login}] FOR LOGIN [{login}];
            GRANT SELECT ON OBJECT::dbo.[{table}] TO [{login}]")).await;
        h.sql(owner_tab, &format!("CREATE TRIGGER dbo.[{stem}_tr] ON dbo.[{table}] AFTER INSERT, UPDATE AS BEGIN SET NOCOUNT ON; END")).await;
        h.sql(owner_tab, &format!("
            EXEC sys.sp_addextendedproperty N'MS_Description', N'hidden check', N'SCHEMA', N'dbo', N'TABLE', N'{table}', N'CONSTRAINT', N'{stem}_ck';
            EXEC sys.sp_addextendedproperty N'MS_Description', N'hidden default', N'SCHEMA', N'dbo', N'TABLE', N'{table}', N'CONSTRAINT', N'{stem}_df';
            ADD SENSITIVITY CLASSIFICATION TO dbo.[{table}].n WITH (LABEL = N'Hidden')")).await;
        h.engine.refresh_catalog(db(owner)).await.unwrap();
        let owner_catalog = h.engine.catalog(db(owner)).await.unwrap();
        let relation = owner_catalog.relation("dbo", &table).unwrap();
        let visible = h.rows(&h.sql(owner_tab, &expressions).await);
        assert_eq!(visible.len(), 3);
        assert!(visible.iter().all(|r| r[1].is_some() && r[2].is_some()), "{visible:?}");
        let owner_detail = h.engine.relation_detail(db(owner), relation.oid).await.unwrap();
        assert_structure(&owner_detail, &stem);
        assert_eq!(owner_detail.defaults.len(), 3);
        assert!(owner_detail.defaults.contains(&ColumnDefault { column: "n".into(), value: DefaultValue::Expr { sql: visible[2][2].clone().unwrap() } }));
        assert!(owner_detail.defaults.contains(&ColumnDefault { column: "computed".into(), value: DefaultValue::Generated { sql: visible[1][2].clone().unwrap() } }));
        assert!(owner_detail.keys.iter().any(|k| k.name == format!("{stem}_ck") && k.kind == KeyKind::Check { definition: visible[0][2].clone().unwrap() }));
        assert!(owner_detail.indexes.iter().find(|i| i.name == format!("{stem}_ix")).unwrap().predicate.is_some());
        assert!(matches!(&owner_detail.triggers[0].implementation, TriggerImplementation::SqlModule { definition: Some(sql) } if sql.contains(&format!("{stem}_tr"))));

        let reader_tab = h.connect(reader).await;
        let permissions = h.rows(&h.sql(reader_tab, &format!("
            SELECT ORIGINAL_LOGIN(),
                CONVERT(int,HAS_PERMS_BY_NAME(N'dbo.{table}',N'OBJECT',N'SELECT')),
                CONVERT(int,HAS_PERMS_BY_NAME(N'dbo.{table}',N'OBJECT',N'VIEW DEFINITION')),
                CONVERT(int,HAS_PERMS_BY_NAME(N'dbo.{table}',N'OBJECT',N'ALTER')),
                CONVERT(int,HAS_PERMS_BY_NAME(N'dbo.{table}',N'OBJECT',N'INSERT')),
                CONVERT(int,HAS_PERMS_BY_NAME(N'dbo.{table}',N'OBJECT',N'UPDATE')),
                CONVERT(int,HAS_PERMS_BY_NAME(N'dbo.{table}',N'OBJECT',N'DELETE'))")).await);
        assert_eq!(permissions, vec![vec![Some(login.clone()), Some("1".into()), Some("0".into()), Some("0".into()), Some("0".into()), Some("0".into()), Some("0".into())]]);
        let selected = h.rows(&h.sql(reader_tab, &format!("SELECT * FROM dbo.[{table}]")).await);
        assert_eq!(selected, vec![vec![Some("1".into()), Some("42".into()), Some("43".into())]]);
        let hidden = h.rows(&h.sql(reader_tab, &expressions).await);
        assert_eq!(hidden.len(), 3, "Hidden expressions still have native catalog rows");
        for (hidden, visible) in hidden.iter().zip(&visible) {
            assert_eq!(&hidden[..2], &visible[..2], "Expression identity remains visible");
            assert_eq!(hidden[2], None, "Expression text is SQL NULL for the SELECT-only login");
        }
        eprintln!("Native SELECT-only expressions {hidden:?}");
        h.engine.refresh_catalog(db(reader)).await.unwrap();
        let catalog = h.engine.catalog(db(reader)).await.unwrap();
        let relation = catalog.relation("dbo", &table).unwrap();
        assert_eq!(relation.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["id", "n", "computed"]);
        assert!(relation.columns.iter().all(|c| c.type_display == "int"));
        assert!(relation.columns[0].pk && !relation.columns[0].writable);
        assert!(relation.columns[1].writable);
        assert!(!relation.columns[2].writable);
        let browsed = h.request(reader_tab, RunRequest::TablePage { schema: "dbo".into(), table: table.clone(), page: 0, query: TableQuery::default() }).await;
        assert_eq!(h.rows(&browsed), selected);

        let script = h.engine.object_script(db(reader), ScriptTarget::Relation { oid: relation.oid }).await.expect("SELECT-only Script as CREATE warns about what it cannot read");
        let path = format!("[dbo].[{table}]");
        for warning in [
            format!("the default of {path}.[n], which this login cannot read"),
            format!("the computed expression of {path}.[computed], which this login cannot read"),
            format!("check constraint [{stem}_ck] on {path}, whose definition this login cannot read"),
            format!("filtered index [{stem}_ix] on {path}, whose filter this login cannot read"),
            format!("trigger [dbo].[{stem}_tr] on {path}, whose text is encrypted or hidden from this login"),
            format!("a complete privilege list for {path}, because this login lacks VIEW DEFINITION on the database"),
            format!("every sensitivity classification on {path}, because this login lacks VIEW ANY SENSITIVITY CLASSIFICATION"),
        ] {
            assert!(script.warnings.contains(&warning), "{warning} missing from {:?}", script.warnings);
        }
        for left_out in [format!("N'{stem}_ck'"), format!("N'{stem}_df'")] {
            assert!(!script.text.contains(&left_out), "The extended property on a constraint the script leaves out replays without it: {}", script.text);
        }
        let detail = h.engine.relation_detail(db(reader), relation.oid).await;
        h.engine.set_settings(Settings { assistant_access: AssistantAccess::Full, ..h.engine.settings() });
        let access = *h.engine.shared.access.lock();
        let described = tools::execute(&h.engine.shared, Some(&db(reader)), ParsedCall::DescribeTables { tables: vec![format!("dbo.{table}")] }, access).await;
        eprintln!("Public relation_detail {detail:?}\nActual describe_tables {:?}\n{}", described.status, described.for_model.text);
        let detail = detail.expect("SELECT-only relation detail retains available metadata");
        assert_structure(&detail, &stem);
        assert_eq!(detail.defaults.len(), 1);
        assert_eq!(detail.keys.len(), 2);
        assert!(matches!(detail.triggers[0].implementation, TriggerImplementation::SqlModule { definition: None }));
        assert!(!described.for_model.is_error, "{}", described.for_model.text);
        assert!(matches!(&described.status, ToolStatus::Done { result: ToolResult::Described { found, missing } } if found == &[format!("dbo.{table}")] && missing.is_empty()));
        let text = &described.for_model.text;
        assert!(text.contains(&format!("dbo.{table} (table")), "{text}");
        assert!(text.contains("id int NOT NULL PK") && text.contains("n int NOT NULL") && text.contains("computed int"), "{text}");
        assert!(text.contains(&format!("{stem}_ix ([n] DESC) INCLUDE ([id])")), "{text}");
    }).catch_unwind().await;

    h.engine.disconnect(reader, true).await.unwrap();
    h.sql(owner_tab, &format!("DROP TABLE IF EXISTS dbo.[{table}]; IF USER_ID(N'{login}') IS NOT NULL DROP USER [{login}]; IF SUSER_ID(N'{login}') IS NOT NULL DROP LOGIN [{login}]")).await;
    let remaining = h.rows(
        &h.sql(
            owner_tab,
            &format!(
                "
        SELECT (SELECT COUNT(*) FROM sys.tables WHERE name=N'{table}'),
            (SELECT COUNT(*) FROM sys.database_principals WHERE name=N'{login}'),
            (SELECT COUNT(*) FROM sys.server_principals WHERE name=N'{login}')"
            ),
        )
        .await,
    );
    assert_eq!(remaining, vec![vec![Some("0".into()), Some("0".into()), Some("0".into())]]);
    eprintln!("Cleanup after disconnect {table} / {login}: {remaining:?}");
    h.engine.disconnect(owner, true).await.unwrap();
    h.engine.shutdown().await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}
