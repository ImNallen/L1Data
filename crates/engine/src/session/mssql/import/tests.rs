use super::*;

async fn plan(session: &mut MsSession, table: &str) -> InsertPlan {
    session.batch(&format!("CREATE TABLE dbo.[{table}](n int)")).await.unwrap();
    let oid = session.count(&format!("SELECT CAST(OBJECT_ID(N'dbo.{table}') AS bigint)")).await.unwrap() as u32;
    let target = ImportTarget {
        db: DatabaseRef { connection_id: ConnectionId(uuid::Uuid::new_v4()), database: "shop".into() },
        schema: "dbo".into(),
        table: table.into(),
        oid,
    };
    let shape = session.import_target(&target).await.unwrap();
    InsertPlan::new(target, shape, &["n"]).unwrap()
}

async fn wait_for_waitfor(observer: &mut MsSession, pid: i32) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if observer
                .count(&format!(
                    "SELECT COUNT_BIG(*) FROM sys.dm_exec_requests WHERE session_id={pid} AND wait_type='WAITFOR'"
                ))
                .await
                .unwrap()
                == 1
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn commit_loss_is_unknown_but_restore_loss_keeps_the_acknowledged_commit() {
    for restoration in [false, true] {
        let mut session = MsSession::open(&super::super::tests::params()).await.unwrap();
        let mut observer = MsSession::open(&super::super::tests::params()).await.unwrap();
        let table = format!("import_loss_{}", uuid::Uuid::new_v4().simple());
        let mut plan = plan(&mut session, &table).await;
        if restoration {
            plan.restore_suffix = Some("WAITFOR DELAY '00:00:30'".into());
        } else {
            plan.commit = Some("COMMIT TRANSACTION; WAITFOR DELAY '00:00:30'".into());
        }
        let pid = session.backend_pid;
        let (tx, rx) = mpsc::channel(4);
        let producer = tokio::spawn(async move {
            tx.send(Ok(ImportRows { rows: vec![vec![Some("42".into())]], bytes: 2, records: 1, first_record: 1 }))
                .await
                .unwrap();
            Ok(())
        });
        let stop = Arc::new(AtomicBool::new(false));
        let (progress, _) = watch::channel((0, 0));
        let task = tokio::spawn(async move {
            let result = session.import_rows(&plan, rx, producer, &stop, progress).await;
            (result, session)
        });
        wait_for_waitfor(&mut observer, pid).await;
        observer.batch(&format!("KILL {pid}")).await.unwrap();
        let (result, session) = task.await.unwrap();
        if restoration {
            assert_eq!(result.unwrap(), 1);
        } else {
            assert!(matches!(result, Err(ImportError::OutcomeUnknown { .. })), "{result:?}");
        }
        assert!(session.closed_reason().is_some());
        assert_eq!(observer.count(&format!("SELECT COUNT_BIG(*) FROM dbo.[{table}] WHERE n=42")).await.unwrap(), 1);
        observer.batch(&format!("DROP TABLE dbo.[{table}]")).await.unwrap();
    }
}

#[tokio::test]
async fn acknowledged_commit_wins_stop_after_commit_started() {
    let mut session = MsSession::open(&super::super::tests::params()).await.unwrap();
    let mut observer = MsSession::open(&super::super::tests::params()).await.unwrap();
    let table = format!("import_late_stop_{}", uuid::Uuid::new_v4().simple());
    let mut plan = plan(&mut session, &table).await;
    plan.commit = Some("WAITFOR DELAY '00:00:01'; COMMIT TRANSACTION".into());
    let pid = session.backend_pid;
    let (tx, rx) = mpsc::channel(4);
    let producer = tokio::spawn(async move {
        tx.send(Ok(ImportRows { rows: vec![vec![Some("42".into())]], bytes: 2, records: 1, first_record: 1 }))
            .await
            .unwrap();
        Ok(())
    });
    let stop = Arc::new(AtomicBool::new(false));
    let signal = stop.clone();
    let (progress, _) = watch::channel((0, 0));
    let task = tokio::spawn(async move { session.import_rows(&plan, rx, producer, &stop, progress).await });
    wait_for_waitfor(&mut observer, pid).await;
    signal.store(true, Ordering::Relaxed);
    assert_eq!(task.await.unwrap().unwrap(), 1);
    assert_eq!(observer.count(&format!("SELECT COUNT_BIG(*) FROM dbo.[{table}]")).await.unwrap(), 1);
    observer.batch(&format!("DROP TABLE dbo.[{table}]")).await.unwrap();
}

#[tokio::test]
async fn ordinary_user_import_observes_transaction_identity_without_server_state_privilege() {
    let mut session = MsSession::open(&super::super::tests::params()).await.unwrap();
    let table = format!("import_permissions_{}", uuid::Uuid::new_v4().simple());
    let plan = plan(&mut session, &table).await;
    let user = format!("user_{table}");
    session.batch(&format!("CREATE USER [{user}] WITHOUT LOGIN; GRANT SELECT, INSERT ON dbo.[{table}] TO [{user}]; GRANT VIEW DEFINITION, VIEW SECURITY DEFINITION TO [{user}]; GRANT SELECT ON sys.sql_expression_dependencies TO [{user}]; EXECUTE AS USER='{user}'")).await.unwrap();
    let (tx, rx) = mpsc::channel(4);
    let producer = tokio::spawn(async move {
        tx.send(Ok(ImportRows { rows: vec![vec![Some("42".into())]], bytes: 2, records: 1, first_record: 1 }))
            .await
            .unwrap();
        Ok(())
    });
    let stop = Arc::new(AtomicBool::new(false));
    let (progress, _) = watch::channel((0, 0));
    assert_eq!(session.import_rows(&plan, rx, producer, &stop, progress).await.unwrap(), 1);
    session.batch("REVERT").await.unwrap();
    assert_eq!(session.count(&format!("SELECT COUNT_BIG(*) FROM dbo.[{table}] WHERE n=42")).await.unwrap(), 1);
    session.batch(&format!("DROP TABLE dbo.[{table}]; DROP USER [{user}]")).await.unwrap();
}
