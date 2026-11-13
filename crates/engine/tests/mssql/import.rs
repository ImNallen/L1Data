use super::*;

async fn import_request(h: &Harness, table: &str, text: &str) -> ImportRequest {
    h.engine.refresh_catalog(h.db()).await.unwrap();
    let catalog = h.engine.catalog(h.db()).await.unwrap();
    let target = ImportTarget {
        db: h.db(),
        schema: "dbo".into(),
        table: table.into(),
        oid: catalog.relation("dbo", table).unwrap().oid,
    };
    let path = h._dir.path().join("input.csv");
    std::fs::write(&path, text).unwrap();
    let source = h.engine.table_import_prepare(h.tab, target, path.to_string_lossy().into()).await.unwrap();
    let preview = h.engine.table_import_preview(h.tab, source, true).await.unwrap();
    ImportRequest { source, header: true, mapping: preview.mapping }
}

#[tokio::test]
async fn csv_import_preserves_values_defaults_and_rolls_back_late_errors() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    let table = format!("import_{}", uuid::Uuid::new_v4().simple());
    h.ok(&format!(
        "CREATE TABLE dbo.[{table}] (id int IDENTITY PRIMARY KEY, n int UNIQUE CHECK(n>0), [文] nvarchar(max), a varchar(30), d int DEFAULT (abs(-7)), doubled AS (n*2))"
    ))
    .await;
    let long = "世🦀".repeat(50000);
    let text = format!("n,文,a\n1,\"{long}\",ascii\n2,,\"\"\n3,\"\",\n4,\"comma,quote\"\"\nline\",ok\n");
    let request = import_request(&h, &table, &text).await;
    for owned in ["id", "doubled"] {
        let mut invalid = request.clone();
        invalid.mapping[0] = Some(owned.into());
        assert!(matches!(h.engine.table_import(h.tab, invalid, None).await, Err(ImportError::Mapping { .. })));
    }
    let summary = h.engine.table_import(h.tab, request.clone(), None).await.unwrap();
    assert_eq!(summary.rows, 4);
    assert!(summary.committed);
    assert!(matches!(h.engine.table_import(h.tab, request, None).await, Err(ImportError::Gone)));
    let state = h.ok(&format!("SELECT n,[文],a,d,doubled FROM dbo.[{table}] ORDER BY n")).await;
    let mut actual = h.rows(Harness::rowsets(&state)[0]);
    actual[0][1] = h.engine.result_cell(Harness::rowsets(&state)[0].r#ref, 0, 1).unwrap();
    assert_eq!(actual[0], vec![Some("1".into()), Some(long), Some("ascii".into()), Some("7".into()), Some("2".into())]);
    assert_eq!(actual[1][1], None);
    assert_eq!(actual[1][2], Some("".into()));
    assert_eq!(actual[2][1], Some("".into()));
    assert_eq!(actual[2][2], None);
    assert_eq!(actual[3][1], Some("comma,quote\"\nline".into()));
    for bad in ["bad,text,ok", "1,text,ok", "-1,text,ok", "999,text,非ASCII"] {
        let mut text = "n,文,a\n".to_owned();
        for n in 10..300 {
            text.push_str(&format!("{n},text,ok\n"));
        }
        text.push_str(bad);
        let request = import_request(&h, &table, &text).await;
        let result = h.engine.table_import(h.tab, request, None).await;
        assert!(matches!(result, Err(ImportError::Query { .. } | ImportError::Csv { .. })), "{result:?}");
        let state = h.ok(&format!("SELECT COUNT(*) FROM dbo.[{table}]")).await;
        assert_eq!(h.rows(Harness::rowsets(&state)[0])[0][0].as_deref(), Some("4"));
        assert_eq!(state.txn, TxnState::Idle);
    }
    h.ok(&format!("DROP TABLE dbo.[{table}]")).await;
}

#[tokio::test]
async fn csv_import_refuses_unsafe_and_stale_targets_and_native_transactions() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    let table = format!("import_safe_{}", uuid::Uuid::new_v4().simple());
    h.ok(&format!("CREATE TABLE dbo.[{table}] (n int)")).await;
    let request = import_request(&h, &table, "n\n1\n").await;
    h.ok("BEGIN TRANSACTION").await;
    assert!(matches!(
        h.engine.table_import(h.tab, request.clone(), None).await,
        Err(ImportError::Query { .. } | ImportError::TransactionAborted)
    ));
    h.ok("ROLLBACK TRANSACTION").await;
    h.ok(&format!("DROP TABLE dbo.[{table}]; CREATE TABLE dbo.[{table}] (n int)")).await;
    assert!(matches!(
        h.engine.table_import(h.tab, request, None).await,
        Err(ImportError::TargetChanged | ImportError::Query { .. })
    ));
    for ddl in [
        format!(
            "CREATE TRIGGER dbo.[tr_{table}] ON dbo.[{table}] AFTER INSERT AS BEGIN COMMIT; INSERT dbo.[{table}] VALUES(99); END"
        ),
        format!(
            "DROP TRIGGER dbo.[tr_{table}]; CREATE UNIQUE INDEX [uq_{table}] ON dbo.[{table}](n) WITH (IGNORE_DUP_KEY=ON)"
        ),
    ] {
        h.ok(&ddl).await;
        h.engine.refresh_catalog(h.db()).await.unwrap();
        let catalog = h.engine.catalog(h.db()).await.unwrap();
        let target = ImportTarget {
            db: h.db(),
            schema: "dbo".into(),
            table: table.clone(),
            oid: catalog.relation("dbo", &table).unwrap().oid,
        };
        let path = h._dir.path().join("refuse.csv");
        std::fs::write(&path, "n\n1").unwrap();
        let source = h.engine.table_import_prepare(h.tab, target, path.to_string_lossy().into()).await.unwrap();
        assert!(matches!(h.engine.table_import_preview(h.tab, source, true).await, Err(ImportError::Query { .. })));
        let state = h.ok(&format!("SELECT COUNT(*) FROM dbo.[{table}]")).await;
        assert_eq!(h.rows(Harness::rowsets(&state)[0])[0][0].as_deref(), Some("0"));
    }
    h.ok(&format!("DROP TABLE dbo.[{table}]")).await;
}

#[tokio::test]
async fn csv_import_stop_drains_blocked_work_and_abandoned_waiters_still_finish() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    let mut blocker = Harness::open(TxnMode::AutoCommit).await;
    let table = format!("import_stop_{}", uuid::Uuid::new_v4().simple());
    h.ok(&format!("CREATE TABLE dbo.[{table}] (n int)")).await;
    for abandon in [false, true] {
        let request = import_request(&h, &table, "n\n1\n2\n").await;
        blocker.ok(&format!("BEGIN TRAN; SELECT TOP (1) 1 FROM dbo.[{table}] WITH (TABLOCKX,HOLDLOCK)")).await;
        let engine = h.engine.clone();
        let tab = h.tab;
        let worker = tokio::spawn(async move { engine.table_import(tab, request, None).await });
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let AppEvent::ConsoleChanged(state) = h.events.recv().await.unwrap() {
                    if state.tab_id == tab && state.work == Some(ConsoleWork::Import) {
                        break;
                    }
                }
            }
        })
        .await
        .unwrap();
        if abandon {
            worker.abort();
        } else {
            h.engine.console_cancel(tab).await;
        }
        blocker.ok("ROLLBACK TRAN").await;
        if !abandon {
            assert!(matches!(worker.await.unwrap(), Err(ImportError::Cancelled)));
        }
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let AppEvent::ConsoleChanged(state) = h.events.recv().await.unwrap() {
                    if state.tab_id == tab && state.work.is_none() {
                        assert_eq!(state.txn, TxnState::Idle);
                        break;
                    }
                }
            }
        })
        .await
        .unwrap();
        let state = h.ok(&format!("SELECT COUNT(*) FROM dbo.[{table}]")).await;
        assert_eq!(h.rows(Harness::rowsets(&state)[0])[0][0].as_deref(), Some(if abandon { "2" } else { "0" }));
    }
    h.ok(&format!("DROP TABLE dbo.[{table}]")).await;
}

#[tokio::test]
async fn csv_import_blocked_stop_deadline_retires_and_reports_unknown() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    let mut blocker = Harness::open(TxnMode::AutoCommit).await;
    let table = format!("import_timeout_{}", uuid::Uuid::new_v4().simple());
    h.ok(&format!("CREATE TABLE dbo.[{table}] (n int)")).await;
    let request = import_request(&h, &table, "n\n1").await;
    blocker.ok(&format!("BEGIN TRAN; SELECT TOP (1) 1 FROM dbo.[{table}] WITH (TABLOCKX,HOLDLOCK)")).await;
    let engine = h.engine.clone();
    let tab = h.tab;
    let worker = tokio::spawn(async move { engine.table_import(tab, request, None).await });
    tokio::time::sleep(Duration::from_millis(300)).await;
    h.engine.console_cancel(tab).await;
    let result = tokio::time::timeout(Duration::from_secs(15), worker).await.unwrap().unwrap();
    assert!(matches!(result, Err(ImportError::OutcomeUnknown { .. })), "{result:?}");
    blocker.ok("ROLLBACK TRAN").await;
    let state = h.ok(&format!("SELECT COUNT(*) FROM dbo.[{table}]")).await;
    assert_eq!(h.rows(Harness::rowsets(&state)[0])[0][0].as_deref(), Some("0"));
    h.ok(&format!("DROP TABLE dbo.[{table}]")).await;
}

#[tokio::test]
async fn csv_import_respects_fk_truncation_batch_limits_and_restores_settings() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    let table = format!("import_limits_{}", uuid::Uuid::new_v4().simple());
    h.ok(&format!("CREATE TABLE dbo.[{table}_parent](id int PRIMARY KEY); INSERT dbo.[{table}_parent] VALUES(1); CREATE TABLE dbo.[{table}](n int REFERENCES dbo.[{table}_parent](id), a varchar(3)); SET ANSI_WARNINGS OFF; SET NOCOUNT ON; SET XACT_ABORT ON;")).await;
    for tail in ["2,ok", "1,toolong"] {
        let text = format!("n,a\n{}{}", "1,ok\n".repeat(300), tail);
        let request = import_request(&h, &table, &text).await;
        let result = h.engine.table_import(h.tab, request, None).await;
        assert!(matches!(result, Err(ImportError::Query { .. })), "{result:?}");
        let state = h.ok(&format!("SELECT COUNT(*) FROM dbo.[{table}]; SELECT @@OPTIONS & (8|512|16384)")).await;
        let sets = Harness::rowsets(&state);
        assert_eq!(h.rows(sets[0])[0][0].as_deref(), Some("0"));
        assert_eq!(h.rows(sets[1])[0][0].as_deref(), Some("16896"));
    }
    h.ok(&format!(
        "DROP TABLE dbo.[{table}]; DROP TABLE dbo.[{table}_parent]; CREATE TABLE dbo.[{table}]({})",
        (0..1024).map(|i| format!("c{i} int")).collect::<Vec<_>>().join(",")
    ))
    .await;
    let header = (0..1024).map(|i| format!("c{i}")).collect::<Vec<_>>().join(",");
    let row = vec!["42"; 1024].join(",");
    let request = import_request(&h, &table, &format!("{header}\n{row}\n{row}\n")).await;
    assert_eq!(h.engine.table_import(h.tab, request, None).await.unwrap().rows, 2);
    let state = h.ok(&format!("SELECT COUNT(*),MIN(c0),MAX(c1023) FROM dbo.[{table}]")).await;
    assert_eq!(h.rows(Harness::rowsets(&state)[0])[0], vec![Some("2".into()), Some("42".into()), Some("42".into())]);
    h.ok(&format!("DROP TABLE dbo.[{table}]")).await;
}

#[tokio::test]
async fn csv_import_production_approval_binds_mapping_header_and_snapshot() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    let table = format!("import_approval_{}", uuid::Uuid::new_v4().simple());
    h.ok(&format!("CREATE TABLE dbo.[{table}](a nvarchar(100), b nvarchar(100))")).await;
    set_environment(&h, Some(Environment::Production)).await;
    let request = import_request(&h, &table, "a,b\nleft,right\n").await;
    let Err(ImportError::ConfirmWrites { confirmation }) = h.engine.table_import(h.tab, request.clone(), None).await
    else {
        panic!("Production must ask")
    };
    let mut changed = request.clone();
    changed.mapping.reverse();
    assert!(matches!(
        h.engine.table_import(h.tab, changed, Some(confirmation.digest.clone())).await,
        Err(ImportError::ConfirmWrites { .. })
    ));
    let mut changed = request.clone();
    changed.header = false;
    assert!(matches!(
        h.engine.table_import(h.tab, changed, Some(confirmation.digest.clone())).await,
        Err(ImportError::ConfirmWrites { .. })
    ));
    std::fs::write(h._dir.path().join("input.csv"), "a,b\naltered,file").unwrap();
    assert_eq!(h.engine.table_import(h.tab, request, Some(confirmation.digest)).await.unwrap().rows, 1);
    let state = h.ok(&format!("SELECT a,b FROM dbo.[{table}]")).await;
    assert_eq!(h.rows(Harness::rowsets(&state)[0])[0], vec![Some("left".into()), Some("right".into())]);
    let request = import_request(&h, &table, "a,b\nnew,source").await;
    let Err(ImportError::ConfirmWrites { confirmation }) = h.engine.table_import(h.tab, request, None).await else {
        panic!("Production must ask")
    };
    let request = import_request(&h, &table, "a,b\nother,source").await;
    assert!(matches!(
        h.engine.table_import(h.tab, request, Some(confirmation.digest)).await,
        Err(ImportError::ConfirmWrites { .. })
    ));
    let sql = format!("DROP TABLE dbo.[{table}]");
    let confirmation = asked_to_run(&h, &sql, None).await;
    let id = h
        .engine
        .console_run(
            h.tab,
            RunRequest::Script {
                text: sql,
                target: RunTarget::All,
                mode: RunMode::Run,
                params: vec![],
                cells: Vec::new(),
            },
            Some(confirmation.digest),
        )
        .await
        .unwrap()
        .run_id;
    h.finish(id).await;
}

#[tokio::test]
async fn csv_import_cancel_after_completed_batches_keeps_no_rows() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    let table = format!("import_progress_{}", uuid::Uuid::new_v4().simple());
    h.ok(&format!("CREATE TABLE dbo.[{table}](n int)")).await;
    let request = import_request(&h, &table, &format!("n\n{}", "1\n".repeat(100_000))).await;
    let engine = h.engine.clone();
    let tab = h.tab;
    let task = tokio::spawn(async move { engine.table_import(tab, request, None).await });
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let AppEvent::ConsoleChanged(state) = h.events.recv().await.unwrap() {
                if state.tab_id == tab && state.import.is_some_and(|p| p.rows > 0 && p.rows < p.total_rows) {
                    break;
                }
            }
        }
    })
    .await
    .unwrap();
    h.engine.console_cancel(tab).await;
    assert!(matches!(task.await.unwrap(), Err(ImportError::Cancelled)));
    let state = h.ok(&format!("SELECT COUNT(*) FROM dbo.[{table}]")).await;
    assert_eq!(h.rows(Harness::rowsets(&state)[0])[0][0].as_deref(), Some("0"));
    h.ok(&format!("DROP TABLE dbo.[{table}]")).await;
}

#[tokio::test]
async fn csv_import_close_and_disconnect_stop_without_bypassing_rollback() {
    for disconnect in [false, true] {
        let mut h = Harness::open(TxnMode::AutoCommit).await;
        let mut blocker = Harness::open(TxnMode::AutoCommit).await;
        let table = format!("import_close_{}", uuid::Uuid::new_v4().simple());
        h.ok(&format!("CREATE TABLE dbo.[{table}](n int)")).await;
        let request = import_request(&h, &table, "n\n1").await;
        blocker.ok(&format!("BEGIN TRAN; SELECT TOP(1) 1 FROM dbo.[{table}] WITH(TABLOCKX,HOLDLOCK)")).await;
        let engine = h.engine.clone();
        let tab = h.tab;
        let task = tokio::spawn(async move { engine.table_import(tab, request, None).await });
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let AppEvent::ConsoleChanged(state) = h.events.recv().await.unwrap() {
                    if state.tab_id == tab && state.work == Some(ConsoleWork::Import) {
                        break;
                    }
                }
            }
        })
        .await
        .unwrap();
        if disconnect {
            h.engine.disconnect(h.connection, true).await.unwrap();
        } else {
            h.engine.console_close(tab, true).await.unwrap();
        }
        blocker.ok("ROLLBACK").await;
        assert!(matches!(task.await.unwrap(), Err(ImportError::Cancelled)));
        let state = blocker.ok(&format!("SELECT COUNT(*) FROM dbo.[{table}]")).await;
        assert_eq!(blocker.rows(Harness::rowsets(&state)[0])[0][0].as_deref(), Some("0"));
        blocker.ok(&format!("DROP TABLE dbo.[{table}]")).await;
    }
}

#[tokio::test]
async fn csv_import_rechecks_a_trigger_installed_after_preview() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    let mut ddl = Harness::open(TxnMode::AutoCommit).await;
    let table = format!("import_trigger_race_{}", uuid::Uuid::new_v4().simple());
    h.ok(&format!("CREATE TABLE dbo.[{table}](n int)")).await;
    let request = import_request(&h, &table, "n\n1").await;
    ddl.ok(&format!("CREATE TRIGGER dbo.[tr_{table}] ON dbo.[{table}] AFTER INSERT AS BEGIN COMMIT; INSERT dbo.[{table}] VALUES(99); END")).await;
    let result = h.engine.table_import(h.tab, request, None).await;
    assert!(matches!(result, Err(ImportError::Query { .. } | ImportError::TargetChanged)), "{result:?}");
    let state = ddl.ok(&format!("SELECT COUNT(*) FROM dbo.[{table}]")).await;
    assert_eq!(ddl.rows(Harness::rowsets(&state)[0])[0][0].as_deref(), Some("0"));
    ddl.ok(&format!("DROP TABLE dbo.[{table}]")).await;
}
