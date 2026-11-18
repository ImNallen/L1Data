use super::*;
use std::sync::Arc;

async fn open() -> MsSession {
    MsSession::open(&super::super::tests::params()).await.unwrap()
}

fn name(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::new_v4().simple())
}

fn cell(column: &str, value: &str) -> CellWrite {
    CellWrite { column: column.into(), value: Some(value.into()) }
}

fn update(id: &str, n: &str, was: &str) -> RowUpdate {
    RowUpdate { key: vec![cell("id", id)], set: vec![cell("n", n)], expect: vec![cell("n", was)] }
}

fn changes(table: &str, updates: Vec<RowUpdate>) -> TableChanges {
    TableChanges { schema: "dbo".into(), table: table.into(), inserts: vec![], updates, deletes: vec![] }
}

async fn scalar(session: &mut MsSession, sql: &str) -> Option<String> {
    session.rows(sql).await.unwrap().remove(0).remove(0)
}

async fn plan_for(session: &mut MsSession, edit: &TableChanges, lock_timeout: i32) -> EditPlan {
    let qualified = format!("[dbo].[{}]", edit.table);
    let native: i32 = scalar(session, &format!("SELECT OBJECT_ID(N'{qualified}')")).await.unwrap().parse().unwrap();
    let opaque = u32::from_be_bytes(native.to_be_bytes());
    let Answer::Checked(check) = session.edit_check(opaque, &qualified).await.unwrap() else { panic!("not checked") };
    plan(edit, &check, &check.support(), &qualified, lock_timeout).unwrap()
}

fn rolled_back(run: &EditRun) -> &ApplyError {
    match &run.outcome {
        EditOutcome::RolledBack(error) => error,
        EditOutcome::Applied => panic!("applied"),
        EditOutcome::Unknown(e) => panic!("unknown: {}", e.message),
    }
}

#[tokio::test]
async fn edit_driver_reports_unknown_when_server_code_commits_and_rolled_back_only_on_evidence() {
    let mut observer = open().await;
    for server_code in ["COMMIT; BEGIN TRANSACTION; THROW 50099, N'after a commit', 1", "COMMIT"] {
        let mut session = open().await;
        let table = name("edit_commits");
        session
            .batch(&format!(
                "CREATE TABLE dbo.[{table}] (id int PRIMARY KEY, n int); INSERT dbo.[{table}] VALUES (1, 1), (2, 2)"
            ))
            .await
            .unwrap();
        let edit = changes(&table, vec![update("1", "10", "1"), update("2", "20", "2")]);
        let mut plan = plan_for(&mut session, &edit, LOCK_TIMEOUT).await;
        plan.writes[1].sql =
            write_module(&format!("UPDATE [dbo].[{table}] SET n = 20 WHERE id = 2; {server_code}"), LOCK_TIMEOUT);
        let run = session.apply_edits(&plan, &AtomicBool::new(false)).await;
        assert!(matches!(run.outcome, EditOutcome::Unknown(_)), "{server_code}");
        assert!(session.closed_reason().is_some(), "an unknown outcome retires the session");
        let saved = observer.rows(&format!("SELECT n FROM dbo.[{table}] ORDER BY id")).await.unwrap();
        assert_eq!(
            saved,
            [[Some("10".into())], [Some("20".into())]],
            "the server code committed the staged edits: {server_code}"
        );
        observer.batch(&format!("DROP TABLE dbo.[{table}]")).await.unwrap();
    }

    let mut session = open().await;
    let table = name("edit_compile_error");
    session
        .batch(&format!(
            "CREATE TABLE dbo.[{table}] (id int PRIMARY KEY, n int); INSERT dbo.[{table}] VALUES (1, 1), (2, 2)"
        ))
        .await
        .unwrap();
    let edit = changes(&table, vec![update("1", "10", "1"), update("2", "20", "2")]);
    let mut plan = plan_for(&mut session, &edit, LOCK_TIMEOUT).await;
    plan.writes[1].sql = write_module(&format!("UPDATE [dbo].[{table}] SET missing = 20 WHERE id = 2"), LOCK_TIMEOUT);
    let run = session.apply_edits(&plan, &AtomicBool::new(false)).await;
    assert!(matches!(
        rolled_back(&run),
        ApplyError::Failed { change: ChangeRef { kind: ChangeKind::Update, index: 1 }, .. }
    ));
    assert!(session.closed_reason().is_none());
    assert_eq!(scalar(&mut session, "SELECT @@TRANCOUNT").await.as_deref(), Some("0"));
    let saved = observer.rows(&format!("SELECT n FROM dbo.[{table}] ORDER BY id")).await.unwrap();
    assert_eq!(saved, [[Some("1".into())], [Some("2".into())]]);
    observer.batch(&format!("DROP TABLE dbo.[{table}]")).await.unwrap();
}

#[tokio::test]
async fn edit_lock_blocks_ddl_on_the_target_and_cascade_tables_until_commit() {
    let mut session = open().await;
    let mut observer = open().await;
    let (table, child) = (name("edit_locked"), name("edit_locked_child"));
    session
        .batch(&format!(
            "CREATE TABLE dbo.[{table}] (id int PRIMARY KEY, n int); INSERT dbo.[{table}] VALUES (1, 1), (2, 2);
            CREATE TABLE dbo.[{child}] (id int PRIMARY KEY, pid int REFERENCES dbo.[{table}](id) ON DELETE CASCADE, m int); INSERT dbo.[{child}] VALUES (10, 2, 0);"
        ))
        .await
        .unwrap();
    let mut plan = plan_for(&mut session, &changes(&table, vec![update("1", "10", "1")]), LOCK_TIMEOUT).await;
    let pause = Arc::new((tokio::sync::Notify::new(), tokio::sync::Notify::new()));
    plan.pause = Some(pause.clone());
    let pid = session.backend_pid;
    let task = tokio::spawn(async move {
        let run = session.apply_edits(&plan, &AtomicBool::new(false)).await;
        (run.outcome, session)
    });
    pause.0.notified().await;
    let locks = observer
        .rows(&format!(
            "SELECT OBJECT_NAME(resource_associated_entity_id), resource_type, request_mode FROM sys.dm_tran_locks
            WHERE request_session_id = {pid} AND resource_database_id = DB_ID() AND resource_type <> 'DATABASE' ORDER BY 1"
        ))
        .await
        .unwrap();
    let mut expected = vec![
        vec![Some(table.clone()), Some("OBJECT".into()), Some("IX".into())],
        vec![Some(child.clone()), Some("OBJECT".into()), Some("IX".into())],
    ];
    expected.sort();
    assert_eq!(locks, expected);
    for target in [&table, &child] {
        // A schema-bound view needs no lock the edit holds; indexing it, which makes writes maintain it, does.
        observer
            .batch(&format!("CREATE VIEW dbo.[{target}_v] WITH SCHEMABINDING AS SELECT id FROM dbo.[{target}]"))
            .await
            .unwrap();
        for ddl in [
            format!("CREATE TRIGGER dbo.[{target}_tr] ON dbo.[{target}] AFTER UPDATE AS BEGIN COMMIT END"),
            format!("ALTER TABLE dbo.[{target}] ADD CONSTRAINT [{target}_ck] CHECK (id > 0)"),
            format!("CREATE UNIQUE INDEX [{target}_ix] ON dbo.[{target}](id) WITH (IGNORE_DUP_KEY = ON)"),
            format!(
                "CREATE TABLE dbo.[{target}_late] (id int PRIMARY KEY, pid int REFERENCES dbo.[{target}](id) ON DELETE CASCADE)"
            ),
            format!("CREATE UNIQUE CLUSTERED INDEX [{target}_vix] ON dbo.[{target}_v](id)"),
        ] {
            observer.batch("SET LOCK_TIMEOUT 1000").await.unwrap();
            let result = observer.batch(&ddl).await;
            let blocked = result.as_ref().err().and_then(batch::code);
            assert_eq!(blocked, Some(LOCK_TIMED_OUT), "{ddl}: {result:?}");
            observer.batch("SET LOCK_TIMEOUT -1; IF @@TRANCOUNT > 0 ROLLBACK").await.unwrap();
        }
    }
    observer.batch(&format!("SET LOCK_TIMEOUT 1000; UPDATE dbo.[{table}] SET n = 5 WHERE id = 2; UPDATE dbo.[{child}] SET m = 1; SET LOCK_TIMEOUT -1")).await.unwrap();
    pause.1.notify_one();
    let (outcome, mut session) = task.await.unwrap();
    assert!(matches!(outcome, EditOutcome::Applied));
    assert_eq!(scalar(&mut session, &format!("SELECT n FROM dbo.[{table}] WHERE id = 1")).await.as_deref(), Some("10"));
    session
        .batch(&format!(
            "DROP VIEW dbo.[{child}_v]; DROP VIEW dbo.[{table}_v]; DROP TABLE dbo.[{child}]; DROP TABLE dbo.[{table}]"
        ))
        .await
        .unwrap();
}

#[tokio::test]
async fn edit_driver_refuses_a_target_recreated_under_its_name_and_rechecks_inside_the_transaction() {
    let mut session = open().await;
    let (table, audit) = (name("edit_recreated"), name("edit_recreated_audit"));
    session.batch(&format!("CREATE TABLE dbo.[{table}] (id int PRIMARY KEY, n int); INSERT dbo.[{table}] VALUES (1, 1); CREATE TABLE dbo.[{audit}] (n int)")).await.unwrap();
    let edit = changes(&table, vec![update("1", "10", "1")]);
    let plan = plan_for(&mut session, &edit, LOCK_TIMEOUT).await;
    session.batch(&format!("DROP TABLE dbo.[{table}]; CREATE TABLE dbo.[{table}] (id int PRIMARY KEY, n int); INSERT dbo.[{table}] VALUES (1, 1)")).await.unwrap();
    let run = session.apply_edits(&plan, &AtomicBool::new(false)).await;
    assert!(matches!(rolled_back(&run), ApplyError::TableChanged));

    let plan = plan_for(&mut session, &edit, LOCK_TIMEOUT).await;
    session.batch(&format!("CREATE TRIGGER dbo.[{table}_tr] ON dbo.[{table}] AFTER UPDATE AS BEGIN INSERT dbo.[{audit}] VALUES (1); COMMIT; BEGIN TRANSACTION; END")).await.unwrap();
    let run = session.apply_edits(&plan, &AtomicBool::new(false)).await;
    assert!(matches!(rolled_back(&run), ApplyError::Refused { reason: EditRefusal::Trigger { .. } }));
    assert_eq!(scalar(&mut session, &format!("SELECT COUNT(*) FROM dbo.[{audit}]")).await.as_deref(), Some("0"));
    assert_eq!(scalar(&mut session, &format!("SELECT n FROM dbo.[{table}]")).await.as_deref(), Some("1"));
    session.batch(&format!("DROP TABLE dbo.[{table}]; DROP TABLE dbo.[{audit}]")).await.unwrap();
}

#[tokio::test]
async fn edit_cancel_deadline_reports_unknown_and_retires_the_session() {
    let mut session = open().await;
    let mut blocker = open().await;
    let table = name("edit_deadline");
    session
        .batch(&format!("CREATE TABLE dbo.[{table}] (id int PRIMARY KEY, n int); INSERT dbo.[{table}] VALUES (1, 1)"))
        .await
        .unwrap();
    let plan = plan_for(&mut session, &changes(&table, vec![update("1", "10", "1")]), -1).await;
    blocker.batch(&format!("BEGIN TRANSACTION; UPDATE dbo.[{table}] SET n = 1 WHERE id = 1")).await.unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let task = {
        let stop = stop.clone();
        tokio::spawn(async move {
            let run = session.apply_edits(&plan, &stop).await;
            (run.outcome, session)
        })
    };
    tokio::time::sleep(Duration::from_millis(500)).await;
    stop.store(true, Ordering::Relaxed);
    let started = std::time::Instant::now();
    let (outcome, session) = task.await.unwrap();
    assert!(matches!(outcome, EditOutcome::Unknown(_)));
    assert!(started.elapsed() >= batch::CLEANUP);
    assert!(session.closed_reason().is_some());
    blocker.batch("ROLLBACK TRANSACTION").await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(scalar(&mut blocker, &format!("SELECT n FROM dbo.[{table}]")).await.as_deref(), Some("1"));
    blocker.batch(&format!("DROP TABLE dbo.[{table}]")).await.unwrap();
}
