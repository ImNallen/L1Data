use engine::model::*;

pub fn fixture(schema: &str, stem: &str, kind: DatabaseKind) -> String {
    let fan_columns = (0..35)
        .map(|i| format!("c{i} int, CONSTRAINT {stem}_fan_{i} FOREIGN KEY(c{i}) REFERENCES {schema}.{stem}_root(a)"))
        .collect::<Vec<_>>()
        .join(",");
    let mut sql = format!("CREATE TABLE {schema}.{stem}_anchor(id int PRIMARY KEY);
        CREATE TABLE {schema}.{stem}_root(a int NOT NULL,b int NOT NULL,other int,
            PRIMARY KEY(a,b),UNIQUE(a),CONSTRAINT {stem}_outgoing FOREIGN KEY(other) REFERENCES {schema}.{stem}_anchor(id),
            CONSTRAINT {stem}_self_pair FOREIGN KEY(a,b) REFERENCES {schema}.{stem}_root(a,b));
        CREATE TABLE {schema}.{stem}_child(x int,y int,CONSTRAINT {stem}_composite FOREIGN KEY(x,y) REFERENCES {schema}.{stem}_root(a,b));
        CREATE TABLE {schema}.{stem}_fan({fan_columns});
        CREATE TABLE {schema}.{stem}_alone(id int);");
    if kind == DatabaseKind::SqlServer {
        sql.push_str(&format!("ALTER TABLE {schema}.{stem}_fan NOCHECK CONSTRAINT {stem}_fan_0;"));
    }
    sql
}

async fn wait_for_current_neighborhood(
    engine: &engine::Engine,
    db: &DatabaseRef,
    schema: &str,
    stem: &str,
) -> (std::sync::Arc<Catalog>, MapNeighborhood) {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let catalog = engine.catalog(db.clone()).await.unwrap();
            let root = catalog
                .schemas
                .iter()
                .find(|s| s.name == schema)
                .unwrap()
                .relations
                .iter()
                .find(|r| r.name == format!("{stem}_root"))
                .unwrap()
                .oid;
            match engine.database_map_neighborhood(db.clone(), root, catalog.version, None).await {
                Ok(first) => break (catalog, first),
                Err(MapError::Stale) => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
                Err(e) => panic!("{e:?}"),
            }
        }
    })
    .await
    .unwrap()
}

pub async fn verify(engine: &engine::Engine, db: DatabaseRef, schema: &str, stem: &str, kind: DatabaseKind) {
    engine.refresh_catalog(db.clone()).await.unwrap();
    let (catalog, first) = wait_for_current_neighborhood(engine, &db, schema, stem).await;
    let relations = &catalog.schemas.iter().find(|s| s.name == schema).unwrap().relations;
    let root = relations.iter().find(|r| r.name == format!("{stem}_root")).unwrap().oid;
    let alone = relations.iter().find(|r| r.name == format!("{stem}_alone")).unwrap().oid;
    assert_eq!(first.foreign_keys.len(), 32);
    assert_eq!(first.root.name, format!("{stem}_root"));
    for relation in &first.relations {
        let mut expected: Vec<_> = first
            .foreign_keys
            .iter()
            .flat_map(|k| {
                k.columns.iter().flat_map(move |p| [(k.source, p.source.clone()), (k.target, p.target.clone())])
            })
            .filter(|(oid, _)| *oid == relation.oid)
            .map(|(_, column)| column)
            .collect();
        expected.sort();
        expected.dedup();
        let mut actual = relation.columns.clone();
        actual.sort();
        assert_eq!(actual, expected, "only participating columns in the current page");
    }
    let cursor = first.after.clone().expect("pagination");
    for invalid in [
        MapCursor { root: alone, ..cursor.clone() },
        MapCursor { db: DatabaseRef { database: "reporting".into(), ..db.clone() }, ..cursor.clone() },
        MapCursor { catalog_version: catalog.version + 1, ..cursor.clone() },
    ] {
        assert!(matches!(
            engine.database_map_neighborhood(db.clone(), root, catalog.version, Some(invalid)).await,
            Err(MapError::Stale)
        ));
    }
    let second = engine.database_map_neighborhood(db.clone(), root, catalog.version, Some(cursor)).await.unwrap();
    assert!(second.after.is_none());
    let keys = first.foreign_keys.into_iter().chain(second.foreign_keys).collect::<Vec<_>>();
    assert_eq!(keys.len(), 38);
    assert!(keys.windows(2).all(|k| k[0].id < k[1].id));
    let composite = keys.iter().find(|k| k.name == format!("{stem}_composite")).unwrap();
    assert_eq!(composite.target, root);
    assert_eq!(
        composite.columns.iter().map(|p| (p.source.as_str(), p.target.as_str())).collect::<Vec<_>>(),
        [("x", "a"), ("y", "b")]
    );
    let self_key = keys.iter().find(|k| k.name == format!("{stem}_self_pair")).unwrap();
    assert_eq!((self_key.source, self_key.target), (root, root));
    assert_eq!(self_key.columns.len(), 2);
    assert_eq!(
        keys.iter().filter(|k| k.name.starts_with(&format!("{stem}_fan_"))).count(),
        35,
        "parallel constraints stay separate"
    );
    assert!(keys.iter().any(|k| k.source == root && k.target != root), "outgoing key");
    if kind == DatabaseKind::SqlServer {
        let disabled = keys.iter().find(|k| k.name == format!("{stem}_fan_0")).unwrap();
        assert!(!disabled.enabled && !disabled.validated);
    }
    let sql = format!("SELECT '😀' FROM {schema}.{stem}_root r JOIN {schema}.{stem}_child c ON r.a=c.x AND r.b=c.y");
    let analysis = engine
        .database_map_query(db.clone(), sql.clone(), DocOffset(sql.encode_utf16().count() as u32), catalog.version)
        .await
        .unwrap();
    assert_eq!(analysis.occurrences.len(), 2);
    assert_eq!(analysis.joins.len(), 2);
    assert!(!analysis.incomplete);
    let isolated = engine.database_map_neighborhood(db.clone(), alone, catalog.version, None).await.unwrap();
    assert_eq!(isolated.root.name, format!("{stem}_alone"));
    assert!(isolated.foreign_keys.is_empty() && isolated.after.is_none());
    assert!(matches!(
        engine.database_map_neighborhood(db.clone(), 0, catalog.version, None).await,
        Err(MapError::Gone)
    ));
    engine.refresh_catalog(db.clone()).await.unwrap();
    assert!(matches!(engine.database_map_neighborhood(db, root, catalog.version, None).await, Err(MapError::Stale)));
}
