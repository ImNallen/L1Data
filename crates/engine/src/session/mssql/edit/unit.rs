use super::*;

fn column(id: i32, name: &str, type_name: &str) -> ShapeColumn {
    ShapeColumn {
        id,
        name: name.into(),
        type_name: type_name.into(),
        max_length: if type_name == "varchar" { 10 } else { 4 },
        precision: 10,
        scale: 0,
        identity: false,
        computed: false,
        generated: false,
        column_set: false,
        masked: false,
        pk: false,
        collation: (type_name == "varchar").then(|| "Latin1_General_CI_AS".into()),
        code_page: (type_name == "varchar").then_some(1252),
        depends: vec![],
    }
}

fn edge(name: &str, parent: i32, referenced: i32, deletes: bool, updates: bool, columns: Vec<i32>) -> Edge {
    Edge { name: name.into(), parent, referenced, deletes, updates, columns }
}

fn table(id: i32, name: &str) -> Table {
    Table { id, name: name.into() }
}

fn trigger(table: &str) -> EditRefusal {
    EditRefusal::Trigger { table: table.into(), trigger: format!("{table}_tr") }
}

/// Target 1 (`dbo.p`) with key `id` and columns `code` and `k AS UPPER(code)`.
fn check(edges: Vec<Edge>, violations: Vec<(i32, EditRefusal)>) -> EditCheck {
    let mut id = column(1, "id", "int");
    id.pk = true;
    let mut k = column(3, "k", "varchar");
    k.computed = true;
    k.depends = vec![2];
    EditCheck {
        target: 1,
        tables: vec![table(1, "dbo.p"), table(2, "dbo.c"), table(3, "dbo.g")],
        unlocked: 0,
        edges,
        violations,
        columns: vec![id, column(2, "code", "varchar"), k],
    }
}

fn verdicts(check: &EditCheck) -> (Vec<Option<ReadOnlyReason>>, Option<EditRefusal>) {
    let EditSupport::Editable { columns, delete, .. } = check.support() else { panic!("refused") };
    (columns.into_iter().map(|c| c.read_only).collect(), delete)
}

#[test]
fn a_delete_follows_every_action_edge_after_its_first() {
    let set_null_then_update = check(
        vec![edge("fk_c", 2, 1, true, false, vec![1]), edge("fk_g", 3, 2, false, true, vec![5])],
        vec![(3, trigger("dbo.g"))],
    );
    let (_, delete) = verdicts(&set_null_then_update);
    assert_eq!(
        delete,
        Some(EditRefusal::Cascade {
            foreign_key: "fk_c".into(),
            table: "dbo.g".into(),
            cause: Box::new(trigger("dbo.g"))
        })
    );
    let update_only = check(vec![edge("fk_c", 2, 1, false, true, vec![1])], vec![(2, trigger("dbo.c"))]);
    assert_eq!(verdicts(&update_only).1, None, "an update action does not run on delete");
}

#[test]
fn a_column_is_read_only_when_an_update_action_references_it_or_a_computed_column_built_from_it() {
    let through_computed = check(vec![edge("fk_c", 2, 1, false, true, vec![3])], vec![(2, trigger("dbo.c"))]);
    let (columns, delete) = verdicts(&through_computed);
    let cascade = ReadOnlyReason::Cascade {
        foreign_key: "fk_c".into(),
        table: "dbo.c".into(),
        cause: Box::new(trigger("dbo.c")),
    };
    assert_eq!(columns, [None, Some(cascade.clone()), Some(ReadOnlyReason::Computed)]);
    assert_eq!(delete, None);
    let deeper = check(
        vec![
            edge("fk_c", 2, 1, false, true, vec![2]),
            edge("fk_g", 3, 2, false, true, vec![9]),
            edge("fk_x", 3, 2, true, false, vec![9]),
        ],
        vec![(3, trigger("dbo.g"))],
    );
    let (columns, _) = verdicts(&deeper);
    assert!(matches!(&columns[1], Some(ReadOnlyReason::Cascade { table, .. }) if table == "dbo.g"));
    let clean = check(vec![edge("fk_c", 2, 1, true, true, vec![2])], vec![]);
    assert_eq!(verdicts(&clean), (vec![None, None, Some(ReadOnlyReason::Computed)], None));
}

#[test]
fn the_target_itself_refuses_and_a_wide_closure_refuses() {
    assert_eq!(check(vec![], vec![(1, trigger("dbo.p"))]).support(), EditSupport::Refused { reason: trigger("dbo.p") });
    let mut wide = check(vec![], vec![]);
    wide.unlocked = 3;
    assert_eq!(wide.support(), EditSupport::Refused { reason: EditRefusal::CascadeTooWide });
    let mut keyless = check(vec![], vec![]);
    keyless.columns[0].pk = false;
    assert_eq!(keyless.support(), EditSupport::Refused { reason: EditRefusal::NoPrimaryKey });
    let mut strange = check(vec![], vec![]);
    strange.columns[1].type_name = "vector".into();
    assert!(matches!(
        strange.support(),
        EditSupport::Refused { reason: EditRefusal::ColumnType { issue: ColumnTypeIssue::Unsupported { .. }, .. } }
    ));
}

fn cell(column: &str, value: Option<&str>) -> CellWrite {
    CellWrite { column: column.into(), value: value.map(Into::into) }
}

#[test]
fn statements_quote_names_number_parameters_and_display_literals() {
    let mut check = check(vec![], vec![]);
    check.columns[1].name = "co]de".into();
    check.columns.push(column(4, "ratio", "float"));
    let support = check.support();
    let changes = TableChanges {
        schema: "dbo".into(),
        table: "we]ird".into(),
        inserts: vec![vec![cell("co]de", Some("x")), cell("ratio", Some("0.1"))], vec![]],
        updates: vec![RowUpdate {
            key: vec![cell("id", Some("7"))],
            set: vec![cell("ratio", None)],
            expect: vec![cell("ratio", Some("2.5"))],
        }],
        deletes: vec![RowDelete { key: vec![cell("id", Some("8"))], expect: vec![] }],
    };
    let plan = plan(&changes, &check, &support, "[dbo].[we]]ird]", LOCK_TIMEOUT).unwrap();
    let shown: Vec<&str> = plan.writes.iter().map(|w| w.display.as_str()).collect();
    assert_eq!(
        shown,
        [
            "DELETE [dbo].[we]]ird] WHERE [id] = CONVERT(int, N'8')",
            "UPDATE [dbo].[we]]ird] SET [ratio] = NULL WHERE [id] = CONVERT(int, N'7')",
            "INSERT [dbo].[we]]ird] ([co]]de], [ratio]) VALUES (N'x', CONVERT(float, N'0.1'))",
            "INSERT [dbo].[we]]ird] DEFAULT VALUES",
        ]
    );
    assert!(plan.writes[1].sql.contains("UPDATE [dbo].[we]]ird] SET [ratio] = @P2 WHERE [id] = CONVERT(int, @P3);"));
    assert_eq!(plan.writes[1].params, [Param::Float(None), Param::Text(Some("7".into()))]);
    assert_eq!(plan.writes[2].params, [Param::Text(Some("x".into())), Param::Float(Some(0.1))]);
    assert!(plan.writes[0].sql.starts_with("DECLARE @fence bigint = @P1;"));
    assert_eq!(plan.reads.len(), 1);
    assert!(
        plan.reads[0]
            .sql
            .contains("JOIN (VALUES (1, CONVERT(int, @P2)), (2, CONVERT(int, @P3))) AS k(ord, k1) ON t.[id] = k.k1;"),
        "{}",
        plan.reads[0].sql
    );
    assert!(plan.reads[0].sql.contains("SELECT k.ord AS [l1:row], t.[id], t.[ratio] FROM [dbo].[we]]ird] AS t WITH (UPDLOCK, HOLDLOCK, ROWLOCK, FORCESEEK)"));
    assert_eq!(
        plan.reads[0].rows,
        [
            (ChangeRef { kind: ChangeKind::Delete, index: 0 }, vec![Some(Some("8".into())), None]),
            (ChangeRef { kind: ChangeKind::Update, index: 0 }, vec![Some(Some("7".into())), Some(Some("2.5".into()))]),
        ]
    );
}

#[test]
fn reads_split_under_the_parameter_limit_and_values_are_checked_before_sending() {
    let check = check(vec![], vec![]);
    let support = check.support();
    let deletes: Vec<RowDelete> =
        (0..2500).map(|i| RowDelete { key: vec![cell("id", Some(&i.to_string()))], expect: vec![] }).collect();
    let changes = TableChanges { schema: "dbo".into(), table: "p".into(), inserts: vec![], updates: vec![], deletes };
    let split = plan(&changes, &check, &support, "[dbo].[p]", LOCK_TIMEOUT).unwrap();
    assert_eq!(split.reads.iter().map(|r| r.params.len()).collect::<Vec<_>>(), [1999, 501]);

    let mut real = check.clone();
    real.columns.push(column(4, "small", "real"));
    let support = real.support();
    for (value, ok) in [("3.4028235e38", true), ("1e39", false), ("nan", false), ("0.1", true), ("x", false)] {
        let changes = TableChanges {
            schema: "dbo".into(),
            table: "p".into(),
            inserts: vec![vec![cell("small", Some(value))]],
            updates: vec![],
            deletes: vec![],
        };
        assert_eq!(plan(&changes, &real, &support, "[dbo].[p]", LOCK_TIMEOUT).is_ok(), ok, "{value}");
    }
}

#[test]
fn the_reader_refuses_extra_missing_and_untagged_sets() {
    let set = |tag: &str| Set { tag: tag.into(), rows: vec![] };
    let reply = |sets: Vec<Set>| Reply { sets, rolled_back: None };
    assert!(reply(vec![set("l1:count")]).single("l1:count").is_ok());
    assert!(reply(vec![set("Microsoft SQL Server 2005 XML Showplan"), set("l1:count")]).single("l1:count").is_err());
    assert!(reply(vec![set("l1:count"), set("")]).single("l1:count").is_err());
    assert!(reply(vec![]).single("l1:count").is_err());
}
