#![cfg(feature = "mssql-tests")]

use engine::model::*;
use engine::{AgentPath, Engine, EngineConfig, MemoryKeyStore};
use futures_util::FutureExt;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

type Rows = Vec<Vec<Option<String>>>;

struct Harness {
    engine: Arc<Engine>,
    _dir: tempfile::TempDir,
}

impl Harness {
    async fn open() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::open(EngineConfig {
            data_dir: dir.path().into(),
            keys: Arc::new(MemoryKeyStore::default()),
            agent_path: AgentPath::Fixed("".into()),
            az_binary: None,
            user_known_hosts: None,
            updates: Arc::new(engine::ScriptedTransport::named("off").unwrap()),
        })
        .await
        .unwrap();
        Self { engine, _dir: dir }
    }

    async fn save(&self, user: &str, password: String) -> ConnectionId {
        self.engine
            .save_connection(ConnectionDraft {
                database_kind: DatabaseKind::SqlServer,
                id: None,
                name: "SQL Server alias visibility regression".into(),
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

    async fn connect(&self, connection_id: ConnectionId) -> TabId {
        self.engine.connect(connection_id).await.unwrap();
        let tab = TabId(uuid::Uuid::new_v4());
        self.engine
            .console_configure(
                tab,
                ConsoleConfig { connection_id, database: "shop".into(), schema: None, mode: TxnMode::AutoCommit },
            )
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
        .expect("SQL Server catalog regression query did not finish")
    }

    fn rows(&self, state: &ConsoleState) -> Rows {
        let RunState::Finished { statements, .. } = &state.run else { panic!("Run did not finish") };
        statements
            .iter()
            .flat_map(|s| &s.outputs)
            .filter_map(|o| match o {
                RunOutput::Rows { result, .. } => Some(self.engine.result_page(result.r#ref, 0, 100).unwrap().rows),
                _ => None,
            })
            .flatten()
            .collect()
    }

    async fn sql(&self, tab: TabId, sql: &str) -> Rows {
        self.rows(
            &self
                .request(
                    tab,
                    RunRequest::Script {
                        text: sql.into(),
                        target: RunTarget::All,
                        mode: RunMode::Run,
                        params: Vec::new(),
                        cells: Vec::new(),
                    },
                )
                .await,
        )
    }
}

fn db(connection_id: ConnectionId) -> DatabaseRef {
    DatabaseRef { connection_id, database: "shop".into() }
}

#[tokio::test]
async fn hidden_alias_types_keep_every_readable_column_and_exact_browse_value() {
    check_visibility(false).boxed().await;
}

#[tokio::test]
async fn executable_procedure_with_hidden_alias_parameters_does_not_fail_catalog() {
    check_visibility(true).boxed().await;
}

async fn check_visibility(executable: bool) {
    let h = Harness::open().await;
    let owner =
        h.save("sa", std::env::var("L1_DATA_TEST_MSSQL_PASSWORD").unwrap_or_else(|_| "L1Data_dev_Str0ng".into())).await;
    let owner_tab = h.connect(owner).await;
    let stem = format!("l1_alias_{}", uuid::Uuid::new_v4().simple());
    let table = format!("{stem}_table");
    let procedure = format!("{stem}_proc");
    let types = [format!("{stem}_text"), format!("{stem}_money"), format!("{stem}_decimal")];
    let login = format!("{stem}_login");
    let password = format!("L1Alias_{}!", uuid::Uuid::new_v4().simple());
    let reader = h.save(&login, password.clone()).await;
    let columns_sql = format!(
        "
        SELECT c.column_id,c.name,TYPE_NAME(c.user_type_id),declared.name,system.name
        FROM sys.columns c
        LEFT JOIN sys.types declared ON declared.user_type_id=c.user_type_id
        LEFT JOIN sys.types system ON system.user_type_id=c.system_type_id AND system.system_type_id=system.user_type_id
        WHERE c.object_id=OBJECT_ID(N'dbo.{table}') ORDER BY c.column_id"
    );
    let parameters_sql = format!(
        "
        SELECT p.parameter_id,p.name,TYPE_NAME(p.user_type_id),TYPE_NAME(p.system_type_id),p.is_output
        FROM sys.parameters p WHERE p.object_id=OBJECT_ID(N'dbo.{procedure}') ORDER BY p.parameter_id"
    );

    let outcome = AssertUnwindSafe(
        async {
            for (name, native) in types.iter().zip(["nvarchar(20)", "money", "decimal(38,18)"]) {
                h.sql(owner_tab, &format!("CREATE TYPE dbo.[{name}] FROM {native} NULL")).await;
            }
            h.sql(
                owner_tab,
                &format!(
                    "
            CREATE TABLE dbo.[{table}] (id int PRIMARY KEY, label dbo.[{}], amount dbo.[{}], exact dbo.[{}]);
            INSERT INTO dbo.[{table}] VALUES (1,N'visible',922337203685477.5807,12345678901234567890.123456789012345678),(2,NULL,NULL,NULL);
            CREATE LOGIN [{login}] WITH PASSWORD=N'{password}', CHECK_POLICY=OFF, DEFAULT_DATABASE=[shop];
            CREATE USER [{login}] FOR LOGIN [{login}];
            GRANT SELECT ON OBJECT::dbo.[{table}] TO [{login}]
        ",
                    types[0], types[1], types[2]
                ),
            )
            .await;
            h.sql(
                owner_tab,
                &format!(
                    "
            CREATE PROCEDURE dbo.[{procedure}] @value dbo.[{ty}], @echo dbo.[{ty}] OUTPUT
            AS BEGIN SET NOCOUNT ON; SET @echo=@value; SELECT @value AS value; END
        ",
                    ty = types[0]
                ),
            )
            .await;
            if executable {
                h.sql(owner_tab, &format!("GRANT EXECUTE ON OBJECT::dbo.[{procedure}] TO [{login}]")).await;
            }

            let visible_columns = h.sql(owner_tab, &columns_sql).await;
            let visible_parameters = h.sql(owner_tab, &parameters_sql).await;
            assert_eq!(visible_columns.len(), 4);
            for (column, alias) in visible_columns[1..].iter().zip(&types) {
                assert_eq!(column[2].as_ref(), Some(alias));
                assert_eq!(column[3].as_ref(), Some(alias));
            }
            assert_eq!(visible_parameters.len(), 2);
            assert!(visible_parameters.iter().all(|p| p[2].as_ref() == Some(&types[0])));
            h.sql(owner_tab, &format!("CREATE SYNONYM dbo.[{stem}_syn] FOR dbo.[{table}]")).await;
            h.engine.refresh_catalog(db(owner)).await.unwrap();
            let owner_catalog = h.engine.catalog(db(owner)).await.unwrap();
            let synonym = owner_catalog.schemas.iter().flat_map(|s| &s.synonyms).find(|s| s.name == format!("{stem}_syn")).expect("synonym");
            assert!(synonym.target.contains(&table), "{}", synonym.target);
            let synonym_sql = h.engine.object_script(db(owner), ScriptTarget::Synonym { oid: synonym.oid }).await.unwrap().text;
            assert!(synonym_sql.contains("CREATE SYNONYM") && synonym_sql.contains(&table), "{synonym_sql}");
            let alias = owner_catalog.schemas.iter().flat_map(|s| &s.types).find(|t| t.name == types[0]).expect("alias type");
            assert_eq!(alias.kind, TypeKind::Alias { base: "nvarchar(20)".into(), nullable: true });
            let owner_relation = owner_catalog.relation("dbo", &table).unwrap();
            let owner_routine = owner_catalog.schemas.iter().flat_map(|s| &s.functions).find(|f| f.name == procedure).unwrap();
            assert_eq!(owner_routine.args, format!("@value {}, @echo {} OUTPUT", types[0], types[0]));
            assert_eq!(owner_routine.returns, "");

            let reader_tab = h.connect(reader).await;
            let permissions = h
                .sql(
                    reader_tab,
                    &format!(
                        "
            SELECT ORIGINAL_LOGIN(),
                CONVERT(int,HAS_PERMS_BY_NAME(N'dbo.{table}',N'OBJECT',N'SELECT')),
                CONVERT(int,HAS_PERMS_BY_NAME(N'dbo.{table}',N'OBJECT',N'VIEW DEFINITION')),
                CONVERT(int,HAS_PERMS_BY_NAME(N'dbo.{}',N'TYPE',N'VIEW DEFINITION'))
        ",
                        types[0]
                    ),
                )
                .await;
            assert_eq!(permissions[0][..3], [Some(login.clone()), Some("1".into()), Some("0".into())]);
            assert_ne!(permissions[0][3].as_deref(), Some("1"));
            let hidden_columns = h.sql(reader_tab, &columns_sql).await;
            assert_eq!(hidden_columns.len(), visible_columns.len());
            for (hidden, visible) in hidden_columns.iter().zip(&visible_columns) {
                assert_eq!(&hidden[..2], &visible[..2], "Column identities remain visible");
                assert_eq!(hidden[4], visible[4], "System types remain independently visible");
            }
            assert!(hidden_columns[1..].iter().all(|c| c[2].is_none() && c[3].is_none()));
            eprintln!("Owner columns {visible_columns:?}; restricted native columns {hidden_columns:?}");

            let selected = h.sql(reader_tab, &format!("SELECT id,label,CAST(amount AS decimal(19,4)) AS amount,exact FROM dbo.[{table}] ORDER BY id")).await;
            assert_eq!(
                selected,
                vec![
                    vec![Some("1".into()), Some("visible".into()), Some("922337203685477.5807".into()), Some("12345678901234567890.123456789012345678".into())],
                    vec![Some("2".into()), None, None, None],
                ]
            );
            if executable {
                let permissions = h
                    .sql(
                        reader_tab,
                        &format!(
                            "
                SELECT CONVERT(int,HAS_PERMS_BY_NAME(N'dbo.{procedure}',N'OBJECT',N'EXECUTE')),
                    CONVERT(int,HAS_PERMS_BY_NAME(N'dbo.{procedure}',N'OBJECT',N'VIEW DEFINITION'))
            "
                        ),
                    )
                    .await;
                assert_eq!(permissions, vec![vec![Some("1".into()), Some("0".into())]]);
                let hidden_parameters = h.sql(reader_tab, &parameters_sql).await;
                assert_eq!(hidden_parameters.len(), visible_parameters.len());
                for (hidden, visible) in hidden_parameters.iter().zip(&visible_parameters) {
                    assert_eq!(&hidden[..2], &visible[..2], "Parameter identities remain visible");
                    assert_eq!(hidden[2], None);
                    assert_eq!(&hidden[3..], &visible[3..], "System type and output flags remain visible");
                }
                let executed = h.sql(reader_tab, &format!("DECLARE @echo nvarchar(20); EXEC dbo.[{procedure}] @value=N'works',@echo=@echo OUTPUT")).await;
                assert_eq!(executed, vec![vec![Some("works".into())]]);
                eprintln!("Owner parameters {visible_parameters:?}; restricted native parameters {hidden_parameters:?}; native EXEC {executed:?}");
            }
            eprintln!("Restricted native SELECT {selected:?}");

            let catalog = h.engine.catalog(db(reader)).await;
            eprintln!("Restricted public catalog result {:?}", catalog.as_ref().map(|c| c.version));
            let catalog = catalog.expect("Hidden descriptive type metadata must not fail the database catalog");
            let relation = catalog.relation("dbo", &table).unwrap();
            let browsed =
                h.request(reader_tab, RunRequest::TablePage { schema: "dbo".into(), table: table.clone(), page: 0, query: TableQuery::default() }).await;
            eprintln!("Restricted public columns {:?}; TablePage {:?}", relation.columns, h.rows(&browsed));
            assert_eq!(relation.oid, owner_relation.oid);
            assert_eq!(
                serde_json::to_value(&relation.columns).unwrap(),
                serde_json::to_value(&owner_relation.columns).unwrap(),
                "Every readable column retains its identity and authoritative system type"
            );
            assert_eq!(
                relation.columns.iter().map(|c| (c.name.as_str(), c.type_display.as_str(), c.category)).collect::<Vec<_>>(),
                [
                    ("id", "int", TypeCategory::Integer),
                    ("label", "nvarchar(20)", TypeCategory::Text),
                    ("amount", "money", TypeCategory::Numeric),
                    ("exact", "decimal(38,18)", TypeCategory::Numeric),
                ]
            );
            if executable {
                let routine = catalog.schemas.iter().flat_map(|s| &s.functions).find(|f| f.name == procedure).unwrap();
                assert_eq!(routine.oid, owner_routine.oid);
                assert_eq!(routine.kind, FnKind::Procedure);
                assert_eq!(routine.args, "@value nvarchar, @echo nvarchar OUTPUT");
                assert_eq!(routine.returns, "");
                assert_eq!(routine.insert_text, format!("EXEC [dbo].[{procedure}] NULL, NULL"));
                assert_eq!(routine.insert_text, owner_routine.insert_text, "Hidden alias types do not change argument placeholders");
                eprintln!("Restricted public routine {routine:?}");
            }
            assert_eq!(h.rows(&browsed), selected, "TablePage must return every readable SELECT value exactly");
        }
        .boxed(),
    )
    .catch_unwind()
    .await;

    let mut cleanup_failures = vec![];
    if let Err(error) = h.engine.disconnect(reader, true).await {
        cleanup_failures.push(format!("Reader disconnect failed {error:?}"));
    }
    let sessions = AssertUnwindSafe(tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let rows = h
                .sql(owner_tab, &format!("SELECT COUNT(*) FROM sys.dm_exec_sessions WHERE login_name=N'{login}'"))
                .await;
            if rows == vec![vec![Some("0".into())]] {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }))
    .catch_unwind()
    .await;
    if !matches!(sessions, Ok(Ok(()))) {
        cleanup_failures.push("Disposable login sessions remained after disconnect".into());
    } else {
        eprintln!("Disconnected {login} has zero sessions before DROP LOGIN");
    }
    for sql in [
        format!("DROP PROCEDURE IF EXISTS dbo.[{procedure}]"),
        format!("IF OBJECT_ID(N'dbo.{stem}_syn', N'SN') IS NOT NULL DROP SYNONYM dbo.[{stem}_syn]"),
        format!("DROP TABLE IF EXISTS dbo.[{table}]"),
        format!("IF TYPE_ID(N'dbo.{}') IS NOT NULL DROP TYPE dbo.[{}]", types[0], types[0]),
        format!("IF TYPE_ID(N'dbo.{}') IS NOT NULL DROP TYPE dbo.[{}]", types[1], types[1]),
        format!("IF TYPE_ID(N'dbo.{}') IS NOT NULL DROP TYPE dbo.[{}]", types[2], types[2]),
        format!("IF USER_ID(N'{login}') IS NOT NULL DROP USER [{login}]"),
        format!("IF SUSER_ID(N'{login}') IS NOT NULL DROP LOGIN [{login}]"),
    ] {
        if AssertUnwindSafe(h.sql(owner_tab, &sql)).catch_unwind().await.is_err() {
            cleanup_failures.push(sql);
        }
    }
    let remaining = h
        .sql(
            owner_tab,
            &format!(
                "
        SELECT (SELECT COUNT(*) FROM sys.objects WHERE name LIKE N'{stem}%'),
            (SELECT COUNT(*) FROM sys.types WHERE name LIKE N'{stem}%'),
            (SELECT COUNT(*) FROM sys.database_principals WHERE name=N'{login}'),
            (SELECT COUNT(*) FROM sys.server_principals WHERE name=N'{login}'),
            (SELECT COUNT(*) FROM sys.dm_exec_sessions WHERE login_name=N'{login}')
    "
            ),
        )
        .await;
    eprintln!("Cleanup {stem} objects/types/users/logins/sessions {remaining:?}; failures {cleanup_failures:?}");
    h.engine.disconnect(owner, true).await.unwrap();
    h.engine.shutdown().await;
    assert!(cleanup_failures.is_empty(), "Cleanup failed {cleanup_failures:?}");
    assert_eq!(remaining, vec![vec![Some("0".into()); 5]], "Disposable fixture must leave nothing behind");
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}
