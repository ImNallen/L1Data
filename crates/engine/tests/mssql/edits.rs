use super::*;

fn cell(column: &str, value: Option<&str>) -> CellWrite {
    CellWrite { column: column.into(), value: value.map(Into::into) }
}

fn key(id: &str) -> Vec<CellWrite> {
    vec![cell("id", Some(id))]
}

fn update(id: &str, set: Vec<CellWrite>, expect: Vec<CellWrite>) -> RowUpdate {
    RowUpdate { key: key(id), set, expect }
}

fn delete(id: &str, expect: Vec<CellWrite>) -> RowDelete {
    RowDelete { key: key(id), expect }
}

fn name(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::new_v4().simple())
}

impl Harness {
    fn changes(table: &str) -> TableChanges {
        TableChanges { schema: "dbo".into(), table: table.into(), inserts: vec![], updates: vec![], deletes: vec![] }
    }
    async fn query(&mut self, sql: &str) -> Vec<Vec<Option<String>>> {
        let state = self.ok(sql).await;
        let sets = Harness::rowsets(&state);
        let meta = sets.last().expect("a result set");
        self.engine.result_page(meta.r#ref, 0, 1000).unwrap().rows
    }
    async fn support(&self, table: &str) -> EditSupport {
        self.refresh().await;
        self.engine.table_edit_support(self.tab, "dbo".into(), table.into()).await.unwrap()
    }
    async fn apply(&self, changes: TableChanges) -> Result<ApplySummary, ApplyError> {
        self.refresh().await;
        self.engine.table_apply(self.tab, changes, None).await
    }
    async fn idle(&mut self) {
        let rows = self.query("SELECT @@TRANCOUNT, XACT_STATE()").await;
        assert_eq!(rows, [[Some("0".into()), Some("0".into())]]);
        let state = self.engine.boot().consoles.into_iter().find(|c| c.tab_id == self.tab).unwrap();
        assert!(matches!(state.session, SessionState::Open { .. }), "{:?}", state.session);
        assert_eq!(state.txn, TxnState::Idle);
    }
    async fn drop_tables(&mut self, tables: &[&str]) {
        for table in tables {
            self.ok(&format!("DROP TABLE IF EXISTS dbo.[{table}]")).await;
        }
    }
}

fn editable(support: &EditSupport) -> (&RowCheck, &[ColumnEdit], &Option<EditRefusal>) {
    match support {
        EditSupport::Editable { row_check, columns, delete } => (row_check, columns, delete),
        EditSupport::Refused { reason } => panic!("refused: {reason:?}"),
    }
}

fn read_only<'a>(columns: &'a [ColumnEdit], name: &str) -> &'a Option<ReadOnlyReason> {
    &columns.iter().find(|c| c.name == name).unwrap().read_only
}

#[tokio::test]
async fn edits_apply_inserts_updates_deletes_null_and_default_in_one_transaction() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    let table = name("edits_apply");
    h.ok(&format!(
        "CREATE TABLE dbo.[{table}] (id int IDENTITY PRIMARY KEY, label nvarchar(40) NULL, amount decimal(12,4) NULL, price money NULL,
            ratio float NULL, small real NULL, old datetime NULL, modern datetime2(7) NULL, moment datetimeoffset(7) NULL,
            uid uniqueidentifier NULL, bytes varbinary(8) NULL, ascii varchar(20) NULL, doubled AS (amount * 2), stamp rowversion,
            note nvarchar(20) NOT NULL DEFAULT (N'fresh'));
        INSERT dbo.[{table}] (label, amount) VALUES (N'one', 1), (N'two', 2), (N'three', 3);"
    ))
    .await;
    let support = h.support(&table).await;
    let (row_check, columns, deletes) = editable(&support);
    assert_eq!(row_check, &RowCheck::Rowversion { column: "stamp".into() });
    assert_eq!(deletes, &None);
    assert_eq!(read_only(columns, "id"), &Some(ReadOnlyReason::Identity));
    assert_eq!(read_only(columns, "doubled"), &Some(ReadOnlyReason::Computed));
    assert_eq!(read_only(columns, "stamp"), &Some(ReadOnlyReason::Rowversion));
    assert_eq!(read_only(columns, "label"), &None);

    let stamps = h.query(&format!("SELECT stamp FROM dbo.[{table}] ORDER BY id")).await;
    let stamp = |i: usize| vec![CellWrite { column: "stamp".into(), value: stamps[i][0].clone() }];
    let mut edit = Harness::changes(&table);
    edit.deletes.push(delete("3", stamp(2)));
    edit.updates.push(update(
        "1",
        vec![
            cell("label", Some("世界 🦀")),
            cell("amount", Some("12345678.1235")),
            cell("price", Some("922337203685477.5807")),
            cell("ratio", Some("0.1")),
            cell("small", Some("3.4028235e38")),
            cell("old", Some("2024-01-02 03:04:05.123")),
            cell("modern", Some("2024-01-02 03:04:05.1234567")),
            cell("moment", Some("2024-01-02 03:04:05.1234567+02:00")),
            cell("uid", Some("abcd1234-5678-90ab-cdef-1234567890ab")),
            cell("bytes", Some("0xDEADBEEF")),
            cell("ascii", Some("plain")),
        ],
        stamp(0),
    ));
    edit.updates.push(update("2", vec![cell("label", None)], stamp(1)));
    edit.inserts.push(vec![cell("label", Some("new")), cell("note", Some("given"))]);
    edit.inserts.push(vec![]);
    let summary = h.apply(edit).await.unwrap();
    assert!(summary.committed);
    let rows = h
        .query(&format!(
            "SELECT id, label, amount, price, ratio, small, old, modern, moment, uid, bytes, ascii, doubled, note FROM dbo.[{table}] ORDER BY id"
        ))
        .await;
    let some = |v: &str| Some(v.to_owned());
    assert_eq!(
        rows[0],
        [
            some("1"),
            some("世界 🦀"),
            some("12345678.1235"),
            some("922337203685477.5807"),
            some("0.1"),
            some("340282350000000000000000000000000000000"),
            some("2024-01-02 03:04:05.123"),
            some("2024-01-02 03:04:05.1234567"),
            some("2024-01-02 03:04:05.1234567+02:00"),
            some("abcd1234-5678-90ab-cdef-1234567890ab"),
            some("0xDEADBEEF"),
            some("plain"),
            some("24691356.2470"),
            some("fresh"),
        ]
    );
    assert_eq!(rows[1][1], None);
    assert_eq!(rows.len(), 4, "row 3 is gone and two rows are new");
    assert_eq!((rows[2][1].clone(), rows[2][13].clone()), (some("new"), some("given")));
    assert_eq!((rows[3][1].clone(), rows[3][13].clone()), (None, some("fresh")));
    h.idle().await;
    h.drop_tables(&[&table]).await;
}

fn refused(result: Result<ApplySummary, ApplyError>) -> EditRefusal {
    match result {
        Err(ApplyError::Refused { reason }) => reason,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

fn refused_support(support: EditSupport) -> EditRefusal {
    match support {
        EditSupport::Refused { reason } => reason,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[tokio::test]
async fn edits_refuse_a_trigger_that_commits_or_rolls_back_before_sending_anything() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    let audit = name("edits_audit");
    h.ok(&format!("CREATE TABLE dbo.[{audit}] (n int)")).await;
    for body in ["COMMIT; BEGIN TRANSACTION;", "ROLLBACK;"] {
        let table = name("edits_trigger");
        let trigger = format!("{table}_tr");
        h.ok(&format!("CREATE TABLE dbo.[{table}] (id int PRIMARY KEY, n int); INSERT dbo.[{table}] VALUES (1, 1)"))
            .await;
        h.ok(&format!(
            "CREATE TRIGGER dbo.[{trigger}] ON dbo.[{table}] AFTER UPDATE AS BEGIN INSERT dbo.[{audit}] VALUES (1); {body} END"
        ))
        .await;
        let expected = EditRefusal::Trigger { table: format!("dbo.{table}"), trigger: format!("dbo.{trigger}") };
        assert_eq!(refused_support(h.support(&table).await), expected);
        let mut edit = Harness::changes(&table);
        edit.updates.push(update("1", vec![cell("n", Some("2"))], vec![cell("n", Some("1"))]));
        assert_eq!(refused(h.apply(edit).await), expected);
        assert_eq!(h.query(&format!("SELECT n FROM dbo.[{table}]")).await, [[Some("1".into())]]);
        assert_eq!(h.query(&format!("SELECT COUNT(*) FROM dbo.[{audit}]")).await, [[Some("0".into())]]);
        h.idle().await;
        h.drop_tables(&[&table]).await;
    }
    h.drop_tables(&[&audit]).await;
}

#[tokio::test]
async fn edits_refuse_deletes_that_cascade_into_a_trigger_and_keep_other_edits() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    let audit = name("edits_cascade_audit");
    let (p, c) = (name("edits_parent"), name("edits_child"));
    let (p2, c2) = (name("edits_null_parent"), name("edits_null_child"));
    let (p3, c3) = (name("edits_key_parent"), name("edits_key_child"));
    let (p4, c4, g4) = (name("edits_chain_parent"), name("edits_chain_child"), name("edits_chain_grandchild"));
    let (p5, c5) = (name("edits_computed_parent"), name("edits_computed_child"));
    let check = format!("{p5}_ok");
    let fk = |table: &str| format!("FK_{table}");
    h.ok(&format!(
        "CREATE TABLE dbo.[{audit}] (n int);
        CREATE TABLE dbo.[{p}] (id int PRIMARY KEY, n int);
        CREATE TABLE dbo.[{c}] (id int PRIMARY KEY, pid int CONSTRAINT [{}] REFERENCES dbo.[{p}](id) ON DELETE CASCADE);
        INSERT dbo.[{p}] VALUES (1, 1), (2, 2); INSERT dbo.[{c}] VALUES (10, 1);
        CREATE TABLE dbo.[{p2}] (id int PRIMARY KEY);
        CREATE TABLE dbo.[{c2}] (id int PRIMARY KEY, pid int NULL CONSTRAINT [{}] REFERENCES dbo.[{p2}](id) ON DELETE SET NULL);
        INSERT dbo.[{p2}] VALUES (1); INSERT dbo.[{c2}] VALUES (10, 1);
        CREATE TABLE dbo.[{p3}] (id int PRIMARY KEY, code varchar(10) NOT NULL UNIQUE, n int);
        CREATE TABLE dbo.[{c3}] (id int PRIMARY KEY, code varchar(10) CONSTRAINT [{}] REFERENCES dbo.[{p3}](code) ON UPDATE CASCADE);
        INSERT dbo.[{p3}] VALUES (1, 'a', 1); INSERT dbo.[{c3}] VALUES (10, 'a');
        CREATE TABLE dbo.[{p4}] (id int PRIMARY KEY);
        CREATE TABLE dbo.[{c4}] (id int PRIMARY KEY, pid int NULL CONSTRAINT [{}] REFERENCES dbo.[{p4}](id) ON DELETE SET NULL, CONSTRAINT [{c4}_uq] UNIQUE (pid));
        CREATE TABLE dbo.[{g4}] (id int PRIMARY KEY, cpid int NULL CONSTRAINT [{}] REFERENCES dbo.[{c4}](pid) ON UPDATE CASCADE);
        INSERT dbo.[{p4}] VALUES (1); INSERT dbo.[{c4}] VALUES (10, 1); INSERT dbo.[{g4}] VALUES (100, 1);",
        fk(&c),
        fk(&c2),
        fk(&c3),
        fk(&c4),
        fk(&g4)
    ))
    .await;
    h.ok(&format!("CREATE FUNCTION dbo.[{check}](@k varchar(10)) RETURNS bit AS BEGIN RETURN 1 END")).await;
    h.ok(&format!(
        "CREATE TABLE dbo.[{p5}] (code varchar(10) NOT NULL, k AS UPPER(code) PERSISTED NOT NULL PRIMARY KEY, n int);
        CREATE TABLE dbo.[{c5}] (id int PRIMARY KEY, k varchar(10) CONSTRAINT [{}] REFERENCES dbo.[{p5}](k) ON UPDATE CASCADE, CHECK (dbo.[{check}](k) = 1));
        INSERT dbo.[{p5}] (code, n) VALUES ('a', 1); INSERT dbo.[{c5}] VALUES (10, 'A');",
        fk(&c5)
    ))
    .await;
    for (table, event) in [(&c, "DELETE"), (&c2, "UPDATE"), (&c3, "UPDATE"), (&g4, "UPDATE")] {
        h.ok(&format!(
            "CREATE TRIGGER dbo.[{table}_tr] ON dbo.[{table}] AFTER {event} AS BEGIN INSERT dbo.[{audit}] VALUES (1); COMMIT; BEGIN TRANSACTION; END"
        ))
        .await;
    }
    let trigger =
        |table: &str| EditRefusal::Trigger { table: format!("dbo.{table}"), trigger: format!("dbo.{table}_tr") };
    let cascade = |child: &str, table: &str, cause: EditRefusal| EditRefusal::Cascade {
        foreign_key: fk(child),
        table: format!("dbo.{table}"),
        cause: Box::new(cause),
    };

    let support = h.support(&p).await;
    let (_, columns, deletes) = editable(&support);
    assert_eq!(deletes, &Some(cascade(&c, &c, trigger(&c))));
    assert_eq!(read_only(columns, "n"), &None);
    let mut both = Harness::changes(&p);
    both.updates.push(update("2", vec![cell("n", Some("20"))], vec![cell("n", Some("2"))]));
    both.deletes.push(delete("1", vec![]));
    assert_eq!(refused(h.apply(both.clone()).await), cascade(&c, &c, trigger(&c)));
    assert_eq!(h.query(&format!("SELECT COUNT(*) FROM dbo.[{p}] WHERE id = 1 OR n = 20")).await, [[Some("1".into())]]);
    both.deletes.clear();
    h.apply(both).await.unwrap();
    assert_eq!(h.query(&format!("SELECT n FROM dbo.[{p}] WHERE id = 2")).await, [[Some("20".into())]]);

    let support = h.support(&p2).await;
    assert_eq!(editable(&support).2, &Some(cascade(&c2, &c2, trigger(&c2))));

    let support = h.support(&p3).await;
    let (_, columns, deletes) = editable(&support);
    assert_eq!(
        read_only(columns, "code"),
        &Some(ReadOnlyReason::Cascade {
            foreign_key: fk(&c3),
            table: format!("dbo.{c3}"),
            cause: Box::new(trigger(&c3))
        })
    );
    assert_eq!((read_only(columns, "id"), read_only(columns, "n"), deletes), (&None, &None, &None));
    let mut key = Harness::changes(&p3);
    key.updates.push(update("1", vec![cell("code", Some("b"))], vec![cell("code", Some("a"))]));
    assert_eq!(refused(h.apply(key).await), cascade(&c3, &c3, trigger(&c3)));

    // A delete's SET NULL updates the child, whose update action reaches the grandchild.
    let support = h.support(&p4).await;
    assert_eq!(editable(&support).2, &Some(cascade(&c4, &g4, trigger(&g4))));
    let mut chain = Harness::changes(&p4);
    chain.deletes.push(delete("1", vec![]));
    assert_eq!(refused(h.apply(chain).await), cascade(&c4, &g4, trigger(&g4)));
    assert_eq!(h.query(&format!("SELECT cpid FROM dbo.[{g4}]")).await, [[Some("1".into())]]);

    // Changing a persisted computed key's base column cascades.
    let support = h.support(&p5).await;
    let (_, columns, _) = editable(&support);
    let function = EditRefusal::Function { table: format!("dbo.{c5}"), object: format!("dbo.{check}") };
    assert_eq!(
        read_only(columns, "code"),
        &Some(ReadOnlyReason::Cascade {
            foreign_key: fk(&c5),
            table: format!("dbo.{c5}"),
            cause: Box::new(function.clone())
        })
    );
    assert_eq!((read_only(columns, "k"), read_only(columns, "n")), (&Some(ReadOnlyReason::Computed), &None));
    let mut base = Harness::changes(&p5);
    base.updates.push(RowUpdate {
        key: vec![cell("k", Some("A"))],
        set: vec![cell("code", Some("b"))],
        expect: vec![cell("code", Some("a"))],
    });
    assert_eq!(refused(h.apply(base).await), cascade(&c5, &c5, function));

    assert_eq!(h.query(&format!("SELECT COUNT(*) FROM dbo.[{audit}]")).await, [[Some("0".into())]]);
    h.idle().await;
    h.drop_tables(&[&c, &p, &c2, &p2, &c3, &p3, &g4, &c4, &p4, &c5, &p5, &audit]).await;
    h.ok(&format!("DROP FUNCTION dbo.[{check}]")).await;
}

#[tokio::test]
async fn edits_cascade_into_clean_tables_and_roll_back_with_them() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    let (p, c) = (name("edits_clean_parent"), name("edits_clean_child"));
    h.ok(&format!(
        "CREATE TABLE dbo.[{p}] (id int PRIMARY KEY, n int CHECK (n > 0));
        CREATE TABLE dbo.[{c}] (id int PRIMARY KEY, pid int REFERENCES dbo.[{p}](id) ON DELETE CASCADE);
        INSERT dbo.[{p}] VALUES (1, 1), (2, 2), (3, 3); INSERT dbo.[{c}] VALUES (10, 1), (11, 1), (20, 2);"
    ))
    .await;
    assert_eq!(editable(&h.support(&p).await).2, &None);
    let mut first = Harness::changes(&p);
    first.deletes.push(delete("1", vec![]));
    h.apply(first).await.unwrap();
    assert_eq!(h.query(&format!("SELECT COUNT(*) FROM dbo.[{c}]")).await, [[Some("1".into())]]);
    let mut failing = Harness::changes(&p);
    failing.deletes.push(delete("2", vec![]));
    failing.updates.push(update("3", vec![cell("n", Some("-1"))], vec![cell("n", Some("3"))]));
    let result = h.apply(failing).await;
    assert!(
        matches!(result, Err(ApplyError::Failed { change: ChangeRef { kind: ChangeKind::Update, index: 0 }, .. })),
        "{result:?}"
    );
    assert_eq!(h.query(&format!("SELECT COUNT(*) FROM dbo.[{c}] WHERE pid = 2")).await, [[Some("1".into())]]);
    assert_eq!(h.query(&format!("SELECT COUNT(*) FROM dbo.[{p}]")).await, [[Some("2".into())]]);
    h.idle().await;
    h.drop_tables(&[&c, &p]).await;
}

#[tokio::test]
async fn edits_detect_rows_another_session_changed_or_deleted() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    let mut other = Harness::open(TxnMode::AutoCommit).await;
    let (versioned, plain) = (name("edits_versioned"), name("edits_plain"));
    h.ok(&format!(
        "CREATE TABLE dbo.[{versioned}] (id int PRIMARY KEY, a int, b int, stamp rowversion); INSERT dbo.[{versioned}] (id, a, b) VALUES (1, 1, 1);
        CREATE TABLE dbo.[{plain}] (id nvarchar(10) COLLATE Latin1_General_CI_AS PRIMARY KEY, a int, other int, bytes varbinary(4),
            moment datetimeoffset(7), ratio float);
        INSERT dbo.[{plain}] VALUES (N'abc', 1, 1, 0x01, '2024-01-02 10:00:00.0000000 +02:00', 0.1), (N'k', 1, 1, 0x01, NULL, NULL);"
    ))
    .await;
    let stamp = h.query(&format!("SELECT stamp FROM dbo.[{versioned}]")).await[0][0].clone();
    other.ok(&format!("UPDATE dbo.[{versioned}] SET b = 2")).await;
    let mut edit = Harness::changes(&versioned);
    edit.updates.push(update(
        "1",
        vec![cell("a", Some("5"))],
        vec![CellWrite { column: "stamp".into(), value: stamp }],
    ));
    assert!(matches!(h.apply(edit).await, Err(ApplyError::RowChanged { .. })));
    assert_eq!(h.query(&format!("SELECT a FROM dbo.[{versioned}]")).await, [[Some("1".into())]]);

    let row = |id: &str, set: Vec<CellWrite>, expect: Vec<CellWrite>| {
        let mut edit = Harness::changes(&plain);
        edit.updates.push(RowUpdate { key: vec![cell("id", Some(id))], set, expect });
        edit
    };
    other.ok(&format!("UPDATE dbo.[{plain}] SET a = 2 WHERE id = N'k'")).await;
    assert!(matches!(
        h.apply(row("k", vec![cell("a", Some("5"))], vec![cell("a", Some("1"))])).await,
        Err(ApplyError::RowChanged { .. })
    ));
    other.ok(&format!("UPDATE dbo.[{plain}] SET other = 2 WHERE id = N'k'")).await;
    h.apply(row("k", vec![cell("a", Some("5"))], vec![cell("a", Some("2"))])).await.unwrap();
    assert_eq!(
        h.query(&format!("SELECT a, other FROM dbo.[{plain}] WHERE id = N'k'")).await,
        [[Some("5".into()), Some("2".into())]]
    );

    for (column, shown, stored) in [
        ("bytes", "0x01", "0x0100"),
        ("moment", "2024-01-02 10:00:00.0000000+02:00", "2024-01-02 08:00:00.0000000 +00:00"),
        ("ratio", "0.1", "0.10000000000000002"),
    ] {
        let value = if column == "bytes" { stored.to_owned() } else { format!("'{stored}'") };
        other.ok(&format!("UPDATE dbo.[{plain}] SET {column} = {value} WHERE id = N'abc'")).await;
        let result = h.apply(row("abc", vec![cell("a", Some("9"))], vec![])).await;
        assert!(matches!(result, Err(ApplyError::Failed { .. })), "an incomplete expect is refused: {result:?}");
        let result = h.apply(row("abc", vec![cell(column, Some(shown))], vec![cell(column, Some(shown))])).await;
        assert!(matches!(result, Err(ApplyError::RowChanged { .. })), "{column}: {result:?}");
    }
    for changed in ["ABC", "abc "] {
        other.ok(&format!("UPDATE dbo.[{plain}] SET id = N'{changed}' WHERE id = N'abc'")).await;
        let result = h.apply(row("abc", vec![cell("a", Some("9"))], vec![cell("a", Some("1"))])).await;
        assert!(matches!(result, Err(ApplyError::RowChanged { .. })), "{changed:?}: {result:?}");
        other.ok(&format!("UPDATE dbo.[{plain}] SET id = N'abc' WHERE id = N'{changed}'")).await;
    }
    assert_eq!(h.query(&format!("SELECT a FROM dbo.[{plain}] WHERE id = N'abc'")).await, [[Some("1".into())]]);
    other.ok(&format!("DELETE dbo.[{plain}] WHERE id = N'abc'")).await;
    let result = h.apply(row("abc", vec![cell("a", Some("9"))], vec![cell("a", Some("1"))])).await;
    assert!(matches!(result, Err(ApplyError::RowMismatch { rows: 0, .. })), "{result:?}");
    let mut gone = Harness::changes(&plain);
    gone.deletes.push(RowDelete { key: vec![cell("id", Some("abc"))], expect: vec![] });
    assert!(matches!(h.apply(gone).await, Err(ApplyError::RowMismatch { rows: 0, .. })));
    h.idle().await;
    h.drop_tables(&[&versioned, &plain]).await;
}

#[tokio::test]
async fn edits_roll_back_every_earlier_change_after_a_mid_batch_failure() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    let (table, parent) = (name("edits_failure"), name("edits_failure_parent"));
    h.ok(&format!(
        "CREATE TABLE dbo.[{parent}] (id int PRIMARY KEY); INSERT dbo.[{parent}] VALUES (1);
        CREATE TABLE dbo.[{table}] (id int PRIMARY KEY, n int CHECK (n >= 0), u int UNIQUE, p int REFERENCES dbo.[{parent}](id), short varchar(3));
        INSERT dbo.[{table}] VALUES (1, 1, 1, NULL, NULL), (2, 2, 2, NULL, NULL), (3, 3, 3, NULL, NULL);"
    ))
    .await;
    for (column, value, before) in
        [("n", "-1", "3"), ("u", "1", "3"), ("p", "99", "NULL"), ("short", "long", "NULL"), ("n", "many", "3")]
    {
        let mut edit = Harness::changes(&table);
        edit.updates.push(update("1", vec![cell("n", Some("10"))], vec![cell("n", Some("1"))]));
        edit.updates.push(update("2", vec![cell("n", Some("20"))], vec![cell("n", Some("2"))]));
        let original = (before != "NULL").then_some(before);
        edit.updates.push(update("3", vec![cell(column, Some(value))], vec![cell(column, original)]));
        let result = h.apply(edit).await;
        assert!(
            matches!(&result, Err(ApplyError::Failed { change: ChangeRef { kind: ChangeKind::Update, index: 2 }, .. })),
            "{column} = {value}: {result:?}"
        );
        assert_eq!(
            h.query(&format!("SELECT n FROM dbo.[{table}] ORDER BY id")).await,
            [[Some("1".into())], [Some("2".into())], [Some("3".into())]]
        );
        h.idle().await;
    }
    h.drop_tables(&[&table, &parent]).await;
}

async fn wait_for_work(h: &mut Harness, work: Option<ConsoleWork>) -> ConsoleState {
    let tab = h.tab;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let AppEvent::ConsoleChanged(state) = h.events.recv().await.unwrap()
                && state.tab_id == tab
                && state.work == work
            {
                return state;
            }
        }
    })
    .await
    .expect("the console did not reach the work")
}

async fn waiting_on_a_lock(observer: &mut Harness, pid: i32) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let rows = observer
                .query(&format!(
                    "SELECT COUNT(*) FROM sys.dm_exec_requests WHERE session_id = {pid} AND wait_type LIKE N'LCK%'"
                ))
                .await;
            if rows[0][0].as_deref() == Some("1") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the edit did not wait on the blocker's lock");
}

fn backend(state: &ConsoleState) -> i32 {
    match state.session {
        SessionState::Open { backend_pid } => backend_pid,
        ref other => panic!("no session: {other:?}"),
    }
}

#[tokio::test]
async fn edits_cancel_during_submit() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    let mut blocker = Harness::open(TxnMode::AutoCommit).await;
    let table = name("edits_cancel");
    h.ok(&format!("CREATE TABLE dbo.[{table}] (id int PRIMARY KEY, n int); INSERT dbo.[{table}] VALUES (1, 1)")).await;
    h.refresh().await;
    blocker.ok(&format!("BEGIN TRANSACTION; UPDATE dbo.[{table}] SET n = 1 WHERE id = 1")).await;
    let mut edit = Harness::changes(&table);
    edit.updates.push(update("1", vec![cell("n", Some("2"))], vec![cell("n", Some("1"))]));
    let (engine, tab) = (h.engine.clone(), h.tab);
    let worker = tokio::spawn(async move { engine.table_apply(tab, edit, None).await });
    let busy = wait_for_work(&mut h, Some(ConsoleWork::Edit)).await;
    let mut observer = Harness::open(TxnMode::AutoCommit).await;
    waiting_on_a_lock(&mut observer, backend(&busy)).await;
    let started = std::time::Instant::now();
    h.engine.console_cancel(h.tab).await;
    let result = tokio::time::timeout(Duration::from_secs(9), worker)
        .await
        .expect("Cancel settled within the lock timeout")
        .unwrap();
    assert!(matches!(result, Err(ApplyError::Cancelled)), "{result:?}");
    assert!(started.elapsed() < Duration::from_secs(9));
    blocker.ok("ROLLBACK TRANSACTION").await;
    assert_eq!(h.query(&format!("SELECT n FROM dbo.[{table}]")).await, [[Some("1".into())]]);
    h.idle().await;
    h.drop_tables(&[&table]).await;
}

#[tokio::test]
async fn edits_report_unknown_and_retire_the_session_when_the_connection_dies() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    let mut blocker = Harness::open(TxnMode::AutoCommit).await;
    let table = name("edits_killed");
    h.ok(&format!(
        "CREATE TABLE dbo.[{table}] (id int PRIMARY KEY, n int); INSERT dbo.[{table}] VALUES (1, 1), (2, 2)"
    ))
    .await;
    h.refresh().await;
    blocker.ok(&format!("BEGIN TRANSACTION; UPDATE dbo.[{table}] SET n = 2 WHERE id = 2")).await;
    let mut edit = Harness::changes(&table);
    edit.updates.push(update("1", vec![cell("n", Some("10"))], vec![cell("n", Some("1"))]));
    edit.updates.push(update("2", vec![cell("n", Some("20"))], vec![cell("n", Some("2"))]));
    let (engine, tab) = (h.engine.clone(), h.tab);
    let worker = tokio::spawn(async move { engine.table_apply(tab, edit, None).await });
    let busy = wait_for_work(&mut h, Some(ConsoleWork::Edit)).await;
    let pid = backend(&busy);
    let mut observer = Harness::open(TxnMode::AutoCommit).await;
    waiting_on_a_lock(&mut observer, pid).await;
    observer.ok(&format!("KILL {pid}")).await;
    let result = worker.await.unwrap();
    assert!(matches!(result, Err(ApplyError::OutcomeUnknown { .. })), "{result:?}");
    let state = h.engine.boot().consoles.into_iter().find(|c| c.tab_id == h.tab).unwrap();
    assert!(
        matches!(state.session, SessionState::Lost { transaction: TransactionLoss::Unknown, .. }),
        "{:?}",
        state.session
    );
    blocker.ok("ROLLBACK TRANSACTION").await;
    assert_eq!(
        observer.query(&format!("SELECT n FROM dbo.[{table}] ORDER BY id")).await,
        [[Some("1".into())], [Some("2".into())]]
    );
    observer.drop_tables(&[&table]).await;
}

#[tokio::test]
async fn edits_refuse_open_transactions_implicit_transactions_limited_textsize_and_result_settings() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    let table = name("edits_session");
    h.ok(&format!("CREATE TABLE dbo.[{table}] (id int PRIMARY KEY, n int); INSERT dbo.[{table}] VALUES (1, 1)")).await;
    let mut edit = Harness::changes(&table);
    edit.updates.push(update("1", vec![cell("n", Some("2"))], vec![cell("n", Some("1"))]));
    for (on, off, reason) in [
        ("BEGIN TRANSACTION", "ROLLBACK TRANSACTION", SessionRefusal::OpenTransaction),
        ("SET IMPLICIT_TRANSACTIONS ON", "SET IMPLICIT_TRANSACTIONS OFF", SessionRefusal::ImplicitTransactions),
        ("SET TEXTSIZE 100", "SET TEXTSIZE -1", SessionRefusal::LimitedTextSize),
        ("SET STATISTICS XML ON", "SET STATISTICS XML OFF", SessionRefusal::ResultShape),
        ("SET STATISTICS PROFILE ON", "SET STATISTICS PROFILE OFF", SessionRefusal::ResultShape),
        ("SET SHOWPLAN_XML ON", "SET SHOWPLAN_XML OFF", SessionRefusal::ResultShape),
        ("SET NOEXEC ON", "SET NOEXEC OFF", SessionRefusal::ResultShape),
    ] {
        h.ok(on).await;
        let result = h.apply(edit.clone()).await;
        assert!(matches!(result, Err(ApplyError::Session { reason: r }) if r == reason), "{on}: {result:?}");
        h.ok(off).await;
        assert_eq!(h.query(&format!("SELECT n FROM dbo.[{table}]")).await, [[Some("1".into())]], "{on}");
        h.idle().await;
    }
    h.apply(edit).await.unwrap();
    h.drop_tables(&[&table]).await;
}

#[tokio::test]
async fn edits_keep_the_tabs_session_settings() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    let table = name("edits_settings");
    h.ok(&format!("CREATE TABLE dbo.[{table}] (id int PRIMARY KEY, short varchar(3), amount decimal(5,2)); INSERT dbo.[{table}] VALUES (1, 'a', 1), (2, 'b', 2)")).await;
    h.ok("SET XACT_ABORT OFF; SET NOCOUNT OFF; SET ROWCOUNT 1; SET LOCK_TIMEOUT -1; SET TRANSACTION ISOLATION LEVEL SNAPSHOT; SET ANSI_WARNINGS OFF; SET NUMERIC_ROUNDABORT ON").await;
    let settings = "SELECT @@OPTIONS, @@LOCK_TIMEOUT, @@TEXTSIZE, (SELECT transaction_isolation_level FROM sys.dm_exec_sessions WHERE session_id = @@SPID)";
    let before = h.query(settings).await;
    let mut long = Harness::changes(&table);
    long.updates.push(update("1", vec![cell("short", Some("toolong"))], vec![cell("short", Some("a"))]));
    assert!(matches!(h.apply(long).await, Err(ApplyError::Failed { .. })));
    let mut rounded = Harness::changes(&table);
    rounded.updates.push(update("1", vec![cell("amount", Some("1.005"))], vec![cell("amount", Some("1.00"))]));
    rounded.updates.push(update("2", vec![cell("amount", Some("2.004"))], vec![cell("amount", Some("2.00"))]));
    h.apply(rounded).await.unwrap();
    assert_eq!(h.query(settings).await, before);
    h.ok("SET ROWCOUNT 0; SET TRANSACTION ISOLATION LEVEL READ COMMITTED; SET ANSI_WARNINGS ON; SET NUMERIC_ROUNDABORT OFF").await;
    assert_eq!(
        h.query(&format!("SELECT short, amount FROM dbo.[{table}] ORDER BY id")).await,
        [[Some("a".into()), Some("1.01".into())], [Some("b".into()), Some("2.00".into())]]
    );
    h.drop_tables(&[&table]).await;
}

#[tokio::test]
async fn edits_recheck_a_trigger_installed_after_the_page_loaded() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    let (table, audit) = (name("edits_late"), name("edits_late_audit"));
    h.ok(&format!("CREATE TABLE dbo.[{table}] (id int PRIMARY KEY, n int); INSERT dbo.[{table}] VALUES (1, 1); CREATE TABLE dbo.[{audit}] (n int)")).await;
    editable(&h.support(&table).await);
    h.ok(&format!("CREATE TRIGGER dbo.[{table}_tr] ON dbo.[{table}] AFTER UPDATE AS BEGIN INSERT dbo.[{audit}] VALUES (1); COMMIT; BEGIN TRANSACTION; END")).await;
    let mut edit = Harness::changes(&table);
    edit.updates.push(update("1", vec![cell("n", Some("2"))], vec![cell("n", Some("1"))]));
    assert!(matches!(refused(h.engine.table_apply(h.tab, edit, None).await), EditRefusal::Trigger { .. }));
    assert_eq!(
        h.query(&format!("SELECT (SELECT n FROM dbo.[{table}]), (SELECT COUNT(*) FROM dbo.[{audit}])")).await,
        [[Some("1".into()), Some("0".into())]]
    );
    h.drop_tables(&[&table, &audit]).await;
}

#[tokio::test]
async fn edits_refuse_every_import_reason() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    let f = name("edits_fn");
    let filter = name("edits_filter");
    h.ok(&format!("CREATE FUNCTION dbo.[{f}](@n int) RETURNS int AS BEGIN RETURN @n END")).await;
    h.ok(&format!("CREATE FUNCTION dbo.[{filter}](@n int) RETURNS TABLE WITH SCHEMABINDING AS RETURN SELECT 1 AS ok"))
        .await;
    let alias = name("edits_alias");
    h.ok(&format!("CREATE TYPE dbo.[{alias}] FROM int")).await;
    let rule = name("edits_rule");
    let bound = name("edits_default");
    h.ok(&format!("CREATE RULE dbo.[{rule}] AS @v > 0")).await;
    h.ok(&format!("CREATE DEFAULT dbo.[{bound}] AS 1")).await;
    let mut cleanup = Vec::new();
    for case in 0..13 {
        let t = name("edits_reason");
        let at = |table: &str| format!("dbo.{table}");
        let column = |issue| EditRefusal::ColumnType { table: at(&t), column: "n".into(), issue };
        let kind = |table_kind| EditRefusal::TableKind { table: at(&t), table_kind };
        let function = EditRefusal::Function { table: at(&t), object: at(&f) };
        let (columns, attach, expected) = match case {
            0 => (
                "(id int PRIMARY KEY, n int)".into(),
                format!("CREATE SECURITY POLICY dbo.[{t}_pol] ADD FILTER PREDICATE dbo.[{filter}](n) ON dbo.[{t}] WITH (STATE = OFF)"),
                EditRefusal::SecurityPolicy { table: at(&t), policy: format!("dbo.{t}_pol") },
            ),
            1 => (
                "(id int PRIMARY KEY, n int)".into(),
                format!("CREATE VIEW dbo.[{t}_v] WITH SCHEMABINDING AS SELECT id, n FROM dbo.[{t}]\nGO\nCREATE UNIQUE CLUSTERED INDEX ix ON dbo.[{t}_v](id)"),
                EditRefusal::IndexedView { table: at(&t), view: format!("dbo.{t}_v") },
            ),
            2 => (
                "(id int PRIMARY KEY, n int)".into(),
                format!("CREATE UNIQUE INDEX [{t}_ix] ON dbo.[{t}](n) WITH (IGNORE_DUP_KEY = ON)"),
                EditRefusal::IgnoreDupKey { table: at(&t), index: format!("{t}_ix") },
            ),
            3 => (format!("(id int PRIMARY KEY, n int CHECK (dbo.[{f}](n) >= 0))"), String::new(), function),
            4 => (format!("(id int PRIMARY KEY, n int DEFAULT (dbo.[{f}](1)))"), String::new(), function),
            5 => (format!("(id int PRIMARY KEY, n int, m AS dbo.[{f}](n))"), String::new(), function),
            6 => (
                "(id int PRIMARY KEY, n int)".into(),
                format!("EXEC sp_bindrule N'dbo.[{rule}]', N'dbo.{t}.n'"),
                column(ColumnTypeIssue::BoundRule),
            ),
            7 => (
                "(id int PRIMARY KEY, n int)".into(),
                format!("EXEC sp_bindefault N'dbo.[{bound}]', N'dbo.{t}.n'"),
                column(ColumnTypeIssue::BoundDefault),
            ),
            8 => (format!("(id int PRIMARY KEY, n dbo.[{alias}])"), String::new(), column(ColumnTypeIssue::Alias)),
            9 => (
                "(id int PRIMARY KEY, node hierarchyid)".into(),
                String::new(),
                EditRefusal::ColumnType { table: at(&t), column: "node".into(), issue: ColumnTypeIssue::Clr },
            ),
            10 => ("(id int PRIMARY KEY, n int) AS NODE".into(), String::new(), kind(TableKind::Graph)),
            11 => ("(id int PRIMARY KEY, n int) WITH (LEDGER = ON (APPEND_ONLY = ON))".into(), String::new(), kind(TableKind::Ledger)),
            _ => (
                "(id int PRIMARY KEY, n int, s datetime2 GENERATED ALWAYS AS ROW START, e datetime2 GENERATED ALWAYS AS ROW END, PERIOD FOR SYSTEM_TIME (s, e))".into(),
                format!("ALTER TABLE dbo.[{t}] SET (SYSTEM_VERSIONING = ON (HISTORY_TABLE = dbo.[{t}_history]))"),
                kind(TableKind::Temporal),
            ),
        };
        h.ok(&format!("CREATE TABLE dbo.[{t}] {columns}")).await;
        if !attach.is_empty() {
            h.ok(&attach).await;
        }
        cleanup.push(t.clone());
        assert_eq!(refused_support(h.support(&t).await), expected, "{columns} {attach}");
        let mut edit = Harness::changes(&t);
        edit.inserts.push(vec![cell("id", Some("1"))]);
        assert_eq!(refused(h.apply(edit).await), expected, "{columns} {attach}");
    }
    for table in cleanup {
        h.ok(&format!(
            "IF EXISTS (SELECT 1 FROM sys.security_policies WHERE name = N'{table}_pol') DROP SECURITY POLICY dbo.[{table}_pol];
            DROP VIEW IF EXISTS dbo.[{table}_v];
            IF OBJECTPROPERTY(OBJECT_ID(N'dbo.[{table}]'), 'TableTemporalType') = 2 ALTER TABLE dbo.[{table}] SET (SYSTEM_VERSIONING = OFF);
            DROP TABLE IF EXISTS dbo.[{table}_history]; DROP TABLE IF EXISTS dbo.[{table}];"
        ))
        .await;
    }
    h.ok(&format!("DROP FUNCTION dbo.[{f}]; DROP FUNCTION dbo.[{filter}]; DROP TYPE dbo.[{alias}]; DROP RULE dbo.[{rule}]; DROP DEFAULT dbo.[{bound}]")).await;
}

#[tokio::test]
async fn edits_mark_server_owned_and_inexact_columns_read_only() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    let table = name("edits_columns");
    h.ok(&format!(
        "CREATE TABLE dbo.[{table}] (id int IDENTITY PRIMARY KEY, n int, doubled AS (n * 2), variant sql_variant, old text, blob image,
            secret nvarchar(20) MASKED WITH (FUNCTION = 'default()'), sparse_one int SPARSE NULL, everything xml COLUMN_SET FOR ALL_SPARSE_COLUMNS,
            jp varchar(10) COLLATE Japanese_CI_AS);
        INSERT dbo.[{table}] (n, jp) VALUES (1, N'日本');"
    ))
    .await;
    let support = h.support(&table).await;
    let (row_check, columns, _) = editable(&support);
    assert_eq!(row_check, &RowCheck::Values);
    for (column, reason) in [
        ("id", ReadOnlyReason::Identity),
        ("doubled", ReadOnlyReason::Computed),
        ("variant", ReadOnlyReason::SqlVariant),
        ("old", ReadOnlyReason::LegacyLob),
        ("blob", ReadOnlyReason::LegacyLob),
        ("secret", ReadOnlyReason::Masked),
        ("everything", ReadOnlyReason::ColumnSet),
    ] {
        assert_eq!(read_only(columns, column), &Some(reason), "{column}");
    }
    assert_eq!(
        (read_only(columns, "n"), read_only(columns, "jp"), read_only(columns, "sparse_one")),
        (&None, &None, &None)
    );
    let base = || Harness::changes(&table);
    let mut forged = base();
    forged.inserts.push(vec![cell("id", Some("5"))]);
    let mut repeated = base();
    repeated.updates.push(update("1", vec![cell("n", Some("2"))], vec![cell("n", Some("1"))]));
    repeated.deletes.push(delete("1", vec![]));
    let mut incomplete = base();
    incomplete.updates.push(update("1", vec![cell("n", Some("2"))], vec![]));
    let mut extra = base();
    extra.deletes.push(delete("1", vec![cell("n", Some("1"))]));
    let mut unicode = base();
    unicode.inserts.push(vec![cell("jp", Some("日本"))]);
    for (label, edit) in [
        ("read-only", forged),
        ("repeated key", repeated),
        ("incomplete expect", incomplete),
        ("delete expect", extra),
        ("unicode varchar", unicode),
    ] {
        let result = h.apply(edit).await;
        assert!(matches!(result, Err(ApplyError::Failed { .. })), "{label}: {result:?}");
    }
    let mut double_byte = base();
    double_byte.updates.push(update("1", vec![cell("jp", Some("abc"))], vec![cell("jp", Some("日本"))]));
    assert!(matches!(h.apply(double_byte).await, Err(ApplyError::Unverifiable { ref column, .. }) if column == "jp"));
    assert_eq!(h.query(&format!("SELECT n, jp FROM dbo.[{table}]")).await, [[Some("1".into()), Some("日本".into())]]);
    h.idle().await;
    h.drop_tables(&[&table]).await;
}

#[tokio::test]
async fn edits_on_production_ask_once_and_bind_values_and_originals() {
    let mut h = Harness::open(TxnMode::AutoCommit).await;
    let table = name("edits_production");
    h.ok(&format!(
        "CREATE TABLE dbo.[{table}] (id int PRIMARY KEY, n int); INSERT dbo.[{table}] VALUES (1, 1), (2, 2)"
    ))
    .await;
    set_environment(&h, Some(Environment::Production)).await;
    let mut edit = Harness::changes(&table);
    edit.updates.push(update("1", vec![cell("n", Some("10"))], vec![cell("n", Some("1"))]));
    edit.deletes.push(delete("2", vec![]));
    edit.inserts.push(vec![cell("id", Some("3")), cell("n", Some("3"))]);
    let Err(ApplyError::ConfirmWrites { confirmation }) = h.apply(edit.clone()).await else {
        panic!("Production must ask")
    };
    assert_eq!(confirmation.sends.count, 3);
    assert!(
        confirmation.sends.statements[0]
            .starts_with(&format!("DELETE [dbo].[{table}] WHERE [id] = CONVERT(int, N'2')")),
        "{:?}",
        confirmation.sends.statements
    );
    assert!(
        confirmation.sends.statements[1].contains("SET [n] = CONVERT(int, N'10')"),
        "{:?}",
        confirmation.sends.statements
    );
    assert_eq!(h.query(&format!("SELECT COUNT(*) FROM dbo.[{table}] WHERE n IN (1, 2)")).await, [[Some("2".into())]]);
    let mut value = edit.clone();
    value.updates[0].set[0].value = Some("11".into());
    let mut original = edit.clone();
    original.updates[0].expect[0].value = Some("0".into());
    for changed in [value, original] {
        let result = h.engine.table_apply(h.tab, changed, Some(confirmation.digest.clone())).await;
        assert!(matches!(result, Err(ApplyError::ConfirmWrites { .. })), "{result:?}");
    }
    h.engine.table_apply(h.tab, edit, Some(confirmation.digest)).await.unwrap();
    assert_eq!(
        h.query(&format!("SELECT id, n FROM dbo.[{table}] ORDER BY id")).await,
        [[Some("1".into()), Some("10".into())], [Some("3".into()), Some("3".into())]]
    );
    set_environment(&h, None).await;
    h.drop_tables(&[&table]).await;
}
