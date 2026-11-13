use super::*;

#[tokio::test]
async fn import_lock_blocks_ddl_on_empty_and_populated_tables_and_late_error_rolls_back() {
    for populated in [false, true] {
        let mut owner = MsSession::open(&tests::params()).await.unwrap();
        let mut other = MsSession::open(&tests::params()).await.unwrap();
        let table = format!("dbo.import_lock_{}", uuid::Uuid::new_v4().simple());
        owner.batch(&format!("CREATE TABLE {table}(id int PRIMARY KEY, value int CHECK(value > 0))")).await.unwrap();
        if populated {
            owner.batch(&format!("INSERT {table} VALUES (0,1)")).await.unwrap();
        }
        let disabled_trigger = format!("dbo.import_disabled_{}", uuid::Uuid::new_v4().simple());
        owner.batch(&format!("CREATE TRIGGER {disabled_trigger} ON {table} AFTER INSERT AS SELECT 1")).await.unwrap();
        owner.batch(&format!("DISABLE TRIGGER {disabled_trigger} ON {table}")).await.unwrap();
        owner
            .batch(&format!("BEGIN TRANSACTION; SELECT TOP (1) 1 FROM {table} WITH (TABLOCKX,HOLDLOCK)"))
            .await
            .unwrap();
        assert_eq!(owner.txn().0, TxnState::Open);
        for ddl in [
            format!("ENABLE TRIGGER {disabled_trigger} ON {table}"),
            format!("ALTER TABLE {table} ADD extra int"),
            format!("ALTER TABLE {table} ADD computed AS (value+1)"),
            format!("ALTER TABLE {table} ADD DEFAULT 7 FOR value"),
            format!("DROP TABLE {table}"),
            format!(
                "CREATE TRIGGER import_trigger_{} ON {table} AFTER INSERT AS SELECT 1",
                uuid::Uuid::new_v4().simple()
            ),
        ] {
            other.batch("SET LOCK_TIMEOUT 100").await.unwrap();
            let error = other.batch(&ddl).await.unwrap_err();
            assert!(error.message.contains("Lock request time out"), "{error:?}");
        }
        other.batch("SET LOCK_TIMEOUT 5000").await.unwrap();
        owner.batch(&format!("INSERT {table} VALUES (1,1),(2,2)")).await.unwrap();
        assert!(owner.batch(&format!("INSERT {table} VALUES (3,-1)")).await.is_err());
        owner.batch("ROLLBACK TRANSACTION").await.unwrap();
        assert_eq!(owner.txn().0, TxnState::Idle);
        assert_eq!(other.count(&format!("SELECT COUNT_BIG(*) FROM {table}")).await.unwrap(), u64::from(populated));
        other.batch(&format!("DROP TABLE {table}")).await.unwrap();
    }
}

#[tokio::test]
async fn import_metadata_refuses_hidden_metadata_and_opaque_expressions() {
    let mut session = MsSession::open(&tests::params()).await.unwrap();
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let table = format!("import_meta_{suffix}");
    let user = format!("import_user_{suffix}");
    let function = format!("import_fn_{suffix}");
    session.batch(&format!("CREATE TABLE dbo.[{table}](n int); CREATE USER [{user}] WITHOUT LOGIN; GRANT SELECT, INSERT ON dbo.[{table}] TO [{user}]")).await.unwrap();
    let oid = session.count(&format!("SELECT CAST(OBJECT_ID(N'dbo.{table}') AS bigint)")).await.unwrap() as u32;
    let target = ImportTarget {
        db: DatabaseRef { connection_id: ConnectionId(uuid::Uuid::new_v4()), database: "shop".into() },
        schema: "dbo".into(),
        table: table.clone(),
        oid,
    };
    session.batch(&format!("EXECUTE AS USER = '{user}'")).await.unwrap();
    let e = session.import_target(&target).await.err().unwrap();
    assert!(e.message.contains("VIEW DEFINITION"), "{e:?}");
    session.batch("REVERT").await.unwrap();
    session
        .batch(&format!("CREATE FUNCTION dbo.[{function}](@n int) RETURNS int AS BEGIN RETURN @n END"))
        .await
        .unwrap();
    session.batch(&format!("ALTER TABLE dbo.[{table}] ADD computed AS dbo.[{function}](n)")).await.unwrap();
    let e = session.import_target(&target).await.err().unwrap();
    assert!(e.message.contains("user-function"), "{e:?}");
    session
        .batch(&format!("ALTER TABLE dbo.[{table}] DROP COLUMN computed; ALTER TABLE dbo.[{table}] ADD CONSTRAINT [df_{table}] DEFAULT dbo.[{function}](1) FOR n"))
        .await
        .unwrap();
    let e = session.import_target(&target).await.err().unwrap();
    assert!(e.message.contains("user-function"), "{e:?}");
    session.batch(&format!("ALTER TABLE dbo.[{table}] DROP CONSTRAINT [df_{table}]; ALTER TABLE dbo.[{table}] ADD CHECK(n >= dbo.[{function}](1))")).await.unwrap();
    let e = session.import_target(&target).await.err().unwrap();
    assert!(e.message.contains("user-function"), "{e:?}");
    session
        .batch(&format!("DROP TABLE dbo.[{table}]; DROP FUNCTION dbo.[{function}]; DROP USER [{user}]"))
        .await
        .unwrap();
}

#[tokio::test]
async fn import_lock_blocks_rls_and_indexed_view_attachment() {
    for populated in [false, true] {
        let mut owner = MsSession::open(&tests::params()).await.unwrap();
        let mut ddl = MsSession::open(&tests::params()).await.unwrap();
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let table = format!("dbo.import_race_{suffix}");
        let function = format!("dbo.import_predicate_{suffix}");
        let policy = format!("dbo.import_policy_{suffix}");
        let view = format!("dbo.import_view_{suffix}");
        owner.batch(&format!("CREATE TABLE {table}(id int NOT NULL PRIMARY KEY)")).await.unwrap();
        owner.batch(&format!("CREATE FUNCTION {function}(@n int) RETURNS TABLE WITH SCHEMABINDING AS RETURN SELECT 1 AS allowed WHERE @n>0")).await.unwrap();
        owner.batch(&format!("CREATE VIEW {view} WITH SCHEMABINDING AS SELECT id FROM {table}")).await.unwrap();
        if populated {
            owner.batch(&format!("INSERT {table} VALUES(1)")).await.unwrap();
        }
        owner.batch(&format!("BEGIN TRAN; SELECT TOP(1) 1 FROM {table} WITH(TABLOCKX,HOLDLOCK)")).await.unwrap();
        ddl.batch("SET LOCK_TIMEOUT 100; SET ANSI_NULLS ON; SET QUOTED_IDENTIFIER ON; SET ANSI_PADDING ON; SET ANSI_WARNINGS ON; SET ARITHABORT ON; SET CONCAT_NULL_YIELDS_NULL ON; SET NUMERIC_ROUNDABORT OFF").await.unwrap();
        for sql in [
            format!("CREATE SECURITY POLICY {policy} ADD FILTER PREDICATE {function}(id) ON {table} WITH (STATE=ON)"),
            format!("CREATE UNIQUE CLUSTERED INDEX ix ON {view}(id)"),
        ] {
            let e = ddl.batch(&sql).await.unwrap_err();
            assert!(e.message.contains("Lock request time out"), "{e:?}");
        }
        ddl.batch("SET LOCK_TIMEOUT 5000").await.unwrap();
        owner.batch("ROLLBACK").await.unwrap();
        ddl.batch(&format!("DROP VIEW {view}; DROP TABLE {table}; DROP FUNCTION {function}")).await.unwrap();
    }
}

#[tokio::test]
async fn import_metadata_refuses_rls_and_indexed_views() {
    let mut session = MsSession::open(&tests::params()).await.unwrap();
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let table = format!("import_policy_{suffix}");
    let function = format!("dbo.fn_{table}");
    let policy = format!("dbo.policy_{table}");
    let view = format!("dbo.view_{table}");
    session.batch(&format!("CREATE TABLE dbo.[{table}](n int NOT NULL PRIMARY KEY)")).await.unwrap();
    let oid = session.count(&format!("SELECT CAST(OBJECT_ID(N'dbo.{table}') AS bigint)")).await.unwrap() as u32;
    let target = ImportTarget {
        db: DatabaseRef { connection_id: ConnectionId(uuid::Uuid::new_v4()), database: "shop".into() },
        schema: "dbo".into(),
        table: table.clone(),
        oid,
    };
    session.batch(&format!("CREATE FUNCTION {function}(@n int) RETURNS TABLE WITH SCHEMABINDING AS RETURN SELECT 1 AS allowed WHERE @n>0")).await.unwrap();
    session
        .batch(&format!(
            "CREATE SECURITY POLICY {policy} ADD FILTER PREDICATE {function}(n) ON dbo.[{table}] WITH (STATE=OFF)"
        ))
        .await
        .unwrap();
    let disabled = session.import_target(&target).await.err().map(|e| e.message);
    assert!(
        disabled.as_ref().is_some_and(|e| e.contains("row-level security")),
        "expected a row-level security refusal for a disabled policy, got {disabled:?}"
    );
    session.batch(&format!("ALTER SECURITY POLICY {policy} WITH (STATE=ON)")).await.unwrap();
    assert!(session.import_target(&target).await.err().unwrap().message.contains("row-level security"));
    session.batch(&format!("DROP SECURITY POLICY {policy}; DROP FUNCTION {function}; SET ANSI_NULLS ON; SET QUOTED_IDENTIFIER ON; SET ANSI_PADDING ON; SET ANSI_WARNINGS ON; SET ARITHABORT ON; SET CONCAT_NULL_YIELDS_NULL ON; SET NUMERIC_ROUNDABORT OFF")).await.unwrap();
    session.batch(&format!("CREATE VIEW {view} WITH SCHEMABINDING AS SELECT n FROM dbo.[{table}]")).await.unwrap();
    session.batch(&format!("CREATE UNIQUE CLUSTERED INDEX ix ON {view}(n)")).await.unwrap();
    assert!(session.import_target(&target).await.err().unwrap().message.contains("indexed-view"));
    session.batch(&format!("DROP VIEW {view}; DROP TABLE dbo.[{table}]")).await.unwrap();
}
