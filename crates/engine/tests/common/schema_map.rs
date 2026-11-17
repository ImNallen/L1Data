use engine::model::*;

pub fn fixture(schema: &str, customers_schema: &str, kind: DatabaseKind) -> [String; 2] {
    let unchecked = match kind {
        DatabaseKind::Postgres => format!(
            "ALTER TABLE {schema}.widget_parts ADD CONSTRAINT {schema}_unchecked FOREIGN KEY(lot) REFERENCES {schema}.widgets(id) NOT VALID;"
        ),
        DatabaseKind::SqlServer => format!(
            "ALTER TABLE {schema}.widget_parts WITH NOCHECK ADD CONSTRAINT {schema}_unchecked FOREIGN KEY(lot) REFERENCES {schema}.widgets(id);"
        ),
    };
    [
        format!("CREATE SCHEMA {schema}"),
        format!(
            "CREATE TABLE {schema}.widgets(id int PRIMARY KEY, label varchar(20));
             CREATE TABLE {schema}.pairs(p int NOT NULL, q int NOT NULL, PRIMARY KEY(p,q));
             CREATE TABLE {schema}.widget_parts(id int PRIMARY KEY, widget_id int, b int, a int, lot int, customer_id int,
                 CONSTRAINT {schema}_pair FOREIGN KEY(b,a) REFERENCES {schema}.pairs(p,q),
                 CONSTRAINT {schema}_customer FOREIGN KEY(customer_id) REFERENCES {customers_schema}.customers(id));
             {unchecked}"
        ),
    ]
}

pub async fn current(engine: &engine::Engine, db: &DatabaseRef, schema: Option<&str>) -> SchemaMap {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let version = engine.catalog(db.clone()).await.unwrap().version;
            match engine.schema_map(db.clone(), schema.map(Into::into), version).await {
                Ok(map) => break map,
                Err(MapError::Stale) => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
                Err(e) => panic!("{e:?}"),
            }
        }
    })
    .await
    .unwrap()
}

pub fn table<'a>(map: &'a SchemaMap, schema: &str, name: &str) -> &'a SchemaMapTable {
    map.tables
        .iter()
        .find(|t| t.schema == schema && t.name == name)
        .unwrap_or_else(|| panic!("{schema}.{name} is not drawn"))
}

pub type Link = (LinkBasis, Option<String>, Vec<(String, String)>);

pub fn links(map: &SchemaMap, source: &SchemaMapTable, target: &SchemaMapTable) -> Vec<Link> {
    map.links
        .iter()
        .filter(|l| l.source == source.oid && l.target == target.oid)
        .map(|l| {
            let pairs = l.columns.iter().map(|p| (p.source.clone(), p.target.clone())).collect();
            (l.basis, l.name.clone(), pairs)
        })
        .collect()
}

pub fn pairs(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs.iter().map(|(s, t)| ((*s).into(), (*t).into())).collect()
}

pub async fn verify(engine: &engine::Engine, db: DatabaseRef, schema: &str, customers_schema: &str) {
    engine.refresh_catalog(db.clone()).await.unwrap();
    let map = current(engine, &db, Some(schema)).await;
    assert_eq!(map.schema, schema);
    let mut drawn: Vec<_> = map.tables.iter().map(|t| (t.schema.as_str(), t.name.as_str(), t.external)).collect();
    assert_eq!(drawn.pop(), Some((customers_schema, "customers", true)), "external tables come last");
    drawn.sort();
    assert_eq!(drawn, [(schema, "pairs", false), (schema, "widget_parts", false), (schema, "widgets", false)]);
    let (parts, widgets, pairs_table, customers) = (
        table(&map, schema, "widget_parts"),
        table(&map, schema, "widgets"),
        table(&map, schema, "pairs"),
        table(&map, customers_schema, "customers"),
    );
    assert_eq!(
        customers.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
        ["id"],
        "an external table lists its key columns only"
    );
    assert_eq!(
        parts.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
        ["id", "widget_id", "b", "a", "lot", "customer_id"]
    );
    assert!(parts.columns[0].pk && parts.columns[0].not_null);
    assert_eq!(parts.columns[1].category, TypeCategory::Integer);
    assert_eq!(
        links(&map, parts, pairs_table),
        [(LinkBasis::Declared, Some(format!("{schema}_pair")), pairs(&[("b", "p"), ("a", "q")]))],
        "composite pairs keep constraint order"
    );
    assert_eq!(
        links(&map, parts, customers),
        [(LinkBasis::Declared, Some(format!("{schema}_customer")), pairs(&[("customer_id", "id")]))]
    );
    assert_eq!(
        links(&map, parts, widgets),
        [
            (LinkBasis::Declared, Some(format!("{schema}_unchecked")), pairs(&[("lot", "id")])),
            (LinkBasis::Inferred, None, pairs(&[("widget_id", "id")])),
        ]
    );
    assert_eq!(map.links.len(), 4);
    let unchecked = map.links.iter().find(|l| l.name.as_deref() == Some(&format!("{schema}_unchecked"))).unwrap();
    assert!(unchecked.enabled && !unchecked.validated);
    let pair = map.links.iter().find(|l| l.name.as_deref() == Some(&format!("{schema}_pair"))).unwrap();
    assert!(pair.enabled && pair.validated);
    let mut ids: Vec<_> = map.links.iter().map(|l| l.id.as_str()).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 4, "link ids are unique");
    assert!(!map.keys_truncated);
    assert_eq!(map.omitted_tables, 0);

    let version = map.catalog_version;
    assert_eq!(
        engine.schema_map(db.clone(), None, version).await.unwrap().schema,
        customers_schema,
        "no schema maps the default one"
    );
    assert!(matches!(
        engine.schema_map(db.clone(), Some(format!("{schema}_missing")), version).await,
        Err(MapError::Gone)
    ));
    engine.refresh_catalog(db.clone()).await.unwrap();
    assert!(matches!(engine.schema_map(db, Some(schema.into()), version).await, Err(MapError::Stale)));
}
