use std::collections::{HashMap, HashSet};

use crate::Engine;
use crate::model::*;

pub(crate) const KEY_LIMIT: usize = 2_000;
pub(crate) const KEY_READ_LIMIT: usize = KEY_LIMIT + 1;
pub(crate) const TABLE_LIMIT: usize = 500;
pub(crate) const PARTITION_LIMIT: usize = 10_000;
pub(crate) const PARTITION_READ_LIMIT: usize = PARTITION_LIMIT + 1;

pub(crate) struct SchemaKeys {
    pub keys: Vec<DeclaredKey>,
    pub partitions: Vec<Partition>,
}

pub(crate) struct Partition {
    pub partition: u32,
    pub parent: u32,
}

pub(crate) struct DeclaredKey {
    pub id: u32,
    pub name: String,
    pub source: u32,
    pub target: u32,
    pub columns: Vec<MapColumnPair>,
    pub enabled: bool,
    pub validated: bool,
}

impl SchemaKeys {
    pub fn pair(&mut self, key: DeclaredKey, pair: MapColumnPair) {
        match self.keys.iter_mut().rev().find(|k| k.id == key.id) {
            Some(held) => held.columns.push(pair),
            None => self.keys.push(DeclaredKey { columns: vec![pair], ..key }),
        }
    }
}

impl Engine {
    /// `schema: None` maps the catalog's default schema. `Stale` when the slot's catalog is not at
    /// `catalog_version` before or after the key read, or the instance or slot was retired meanwhile.
    pub async fn schema_map(
        &self,
        db: DatabaseRef,
        schema: Option<String>,
        catalog_version: u64,
    ) -> Result<SchemaMap, MapError> {
        let shared = &self.shared;
        let (instance, slot) = shared.slot(&db).map_err(|error| MapError::Catalog { error })?;
        if slot.last().is_none() {
            shared.ensure_catalog(&instance, &slot).await.map_err(|error| MapError::Catalog { error })?;
        }
        let mut meta = slot.meta.lock().await;
        if !shared.map_current(&instance, &slot, catalog_version) {
            return Err(MapError::Stale);
        }
        let catalog = slot.last().ok_or(MapError::Stale)?;
        let name = schema.or_else(|| catalog.default_schema.clone()).ok_or(MapError::Gone)?;
        let schema = catalog.schemas.iter().find(|s| s.name == name).ok_or(MapError::Gone)?;
        let session = shared
            .session_in(&instance, &slot, &mut meta)
            .await
            .map_err(|e| MapError::Failed { message: crate::connection::connect_error_text(&e) })?;
        let read = session.schema_keys(&name).await.map_err(|e| MapError::Failed { message: e.message })?;
        if !shared.map_current(&instance, &slot, catalog_version) {
            return Err(MapError::Stale);
        }
        Ok(build(&catalog, schema, read))
    }
}

pub(crate) fn build(catalog: &Catalog, schema: &SchemaInfo, read: SchemaKeys) -> SchemaMap {
    let relations: HashMap<u32, (&str, &Relation)> =
        catalog.schemas.iter().flat_map(|s| s.relations.iter().map(move |r| (r.oid, (s.name.as_str(), r)))).collect();
    let mut read_partitions = read.partitions;
    let partitions_truncated = read_partitions.len() > PARTITION_LIMIT;
    read_partitions.truncate(PARTITION_LIMIT);
    let top = top_parents(&read_partitions);
    let mut partitions: HashMap<u32, u32> = HashMap::new();
    for parent in top.values() {
        *partitions.entry(*parent).or_default() += 1;
    }
    let drawn =
        |r: &Relation| !matches!(r.kind, RelKind::View | RelKind::MaterializedView) && !top.contains_key(&r.oid);
    let mut own: Vec<&Relation> = schema.relations.iter().filter(|r| drawn(r)).collect();
    let own_oids: HashSet<u32> = own.iter().map(|r| r.oid).collect();

    let mut keys = read.keys;
    let inferred = infer(&own, &keys);
    let keys_truncated = keys.len() > KEY_LIMIT;
    keys.truncate(KEY_LIMIT);
    let lists = |oid: u32, column: &str| {
        relations.get(&oid).is_some_and(|(_, r)| drawn(r) && r.columns.iter().any(|c| c.name == column))
    };
    let catalog_names_every_column =
        |k: &DeclaredKey| k.columns.iter().all(|p| lists(k.source, &p.source) && lists(k.target, &p.target));
    keys.retain(|k| (own_oids.contains(&k.source) || own_oids.contains(&k.target)) && catalog_names_every_column(k));

    let mut external: Vec<(&str, &Relation)> = keys
        .iter()
        .flat_map(|k| [k.source, k.target])
        .filter(|oid| !own_oids.contains(oid))
        .collect::<HashSet<_>>()
        .into_iter()
        .map(|oid| relations[&oid])
        .collect();
    external.sort_by(|a, b| (a.0, &a.1.name).cmp(&(b.0, &b.1.name)));
    let total = own.len() + external.len();
    own.truncate(TABLE_LIMIT);
    external.truncate(TABLE_LIMIT - own.len());
    let kept: HashSet<u32> = own.iter().copied().chain(external.iter().map(|(_, r)| *r)).map(|r| r.oid).collect();
    keys.retain(|k| kept.contains(&k.source) && kept.contains(&k.target));

    let named: HashSet<(u32, &str)> = keys
        .iter()
        .flat_map(|k| {
            k.columns.iter().flat_map(move |p| [(k.source, p.source.as_str()), (k.target, p.target.as_str())])
        })
        .collect();
    let table = |schema: &str, r: &Relation, external: bool| SchemaMapTable {
        oid: r.oid,
        schema: schema.into(),
        name: r.name.clone(),
        kind: r.kind,
        row_estimate: r.row_estimate,
        partitions: partitions.get(&r.oid).copied().unwrap_or(0),
        external,
        columns: r
            .columns
            .iter()
            .filter(|c| !external || c.pk || named.contains(&(r.oid, c.name.as_str())))
            .map(|c| SchemaMapColumn {
                attnum: c.attnum,
                name: c.name.clone(),
                type_display: c.type_display.clone(),
                category: c.category,
                not_null: c.not_null,
                pk: c.pk,
            })
            .collect(),
    };
    let tables = own
        .iter()
        .map(|r| table(&schema.name, r, false))
        .chain(external.iter().map(|(s, r)| table(s, r, true)))
        .collect();
    let links = keys
        .into_iter()
        .map(|k| SchemaMapLink {
            id: format!("k{}", k.id),
            basis: LinkBasis::Declared,
            name: Some(k.name),
            source: k.source,
            target: k.target,
            columns: k.columns,
            enabled: k.enabled,
            validated: k.validated,
        })
        .chain(inferred.into_iter().filter(|l| kept.contains(&l.source) && kept.contains(&l.target)))
        .collect();
    SchemaMap {
        db: catalog.db.clone(),
        catalog_version: catalog.version,
        schema: schema.name.clone(),
        tables,
        links,
        keys_truncated,
        partitions_truncated,
        omitted_tables: (total - kept.len()) as u32,
    }
}

/// Skips every column `declared` names as a key source, also when the catalog has not seen that key yet.
pub(crate) fn infer(tables: &[&Relation], declared: &[DeclaredKey]) -> Vec<SchemaMapLink> {
    let declared: HashSet<(u32, &str)> =
        declared.iter().flat_map(|k| k.columns.iter().map(move |p| (k.source, p.source.as_str()))).collect();
    let mut by_name: HashMap<String, Vec<Target>> = HashMap::new();
    for table in tables {
        let Some(key) = single_key(table) else {
            continue;
        };
        for (name, matched) in referencing_names(table, key) {
            by_name.entry(name).or_default().push(Target { table, key, matched });
        }
    }
    let mut links = vec![];
    for source in tables {
        for column in source.columns.iter().filter(|c| !c.fk && !declared.contains(&(source.oid, c.name.as_str()))) {
            let Some(candidates) = by_name.get(&column.name.to_ascii_lowercase()) else {
                continue;
            };
            let mut matches = candidates.iter().filter(|t| {
                t.table.oid != source.oid
                    && t.key.category == column.category
                    && !(t.matched == Match::SharedName && is_whole_key(source, column))
            });
            if let (Some(Target { table: target, key, .. }), None) = (matches.next(), matches.next()) {
                links.push(SchemaMapLink {
                    id: format!("i{}.{}", source.oid, column.attnum),
                    basis: LinkBasis::Inferred,
                    name: None,
                    source: source.oid,
                    target: target.oid,
                    columns: vec![MapColumnPair { source: column.name.clone(), target: key.name.clone() }],
                    enabled: false,
                    validated: false,
                });
            }
        }
    }
    links
}

struct Target<'a> {
    table: &'a Relation,
    key: &'a ColumnInfo,
    matched: Match,
}

#[derive(Clone, Copy, PartialEq)]
enum Match {
    Stem,
    SharedName,
}

fn referencing_names(table: &Relation, key: &ColumnInfo) -> Vec<(String, Match)> {
    let key_name = key.name.to_ascii_lowercase();
    if key_name == "id" {
        stems(&table.name)
            .into_iter()
            .flat_map(|stem| [format!("{stem}_id"), format!("{stem}id")])
            .map(|name| (name, Match::Stem))
            .collect()
    } else if key_name.ends_with("id") {
        vec![(key_name, Match::SharedName)]
    } else {
        vec![]
    }
}

fn is_whole_key(table: &Relation, column: &ColumnInfo) -> bool {
    single_key(table).is_some_and(|k| k.attnum == column.attnum)
}

fn single_key(table: &Relation) -> Option<&ColumnInfo> {
    let mut keys = table.columns.iter().filter(|c| c.pk);
    match (keys.next(), keys.next()) {
        (Some(key), None) => Some(key),
        _ => None,
    }
}

fn stems(table: &str) -> Vec<String> {
    let table = table.to_ascii_lowercase();
    let mut stems = vec![table.clone()];
    for (plural, singular) in [("ies", "y"), ("ses", "s"), ("xes", "x"), ("s", "")] {
        if let Some(base) = table.strip_suffix(plural) {
            stems.push(format!("{base}{singular}"));
        }
    }
    stems
}

fn top_parents(partitions: &[Partition]) -> HashMap<u32, u32> {
    let parent: HashMap<u32, u32> = partitions.iter().map(|p| (p.partition, p.parent)).collect();
    parent
        .keys()
        .map(|&child| {
            let mut top = parent[&child];
            while let Some(&up) = parent.get(&top) {
                top = up;
            }
            (child, top)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Col {
        name: &'static str,
        category: TypeCategory,
        pk: bool,
        fk: bool,
    }

    fn int(name: &'static str) -> Col {
        Col { name, category: TypeCategory::Integer, pk: false, fk: false }
    }
    fn pk(name: &'static str) -> Col {
        Col { pk: true, ..int(name) }
    }
    fn fk(name: &'static str) -> Col {
        Col { fk: true, ..int(name) }
    }
    fn text(name: &'static str) -> Col {
        Col { category: TypeCategory::Text, ..int(name) }
    }

    fn relation(oid: u32, name: &str, kind: RelKind, columns: Vec<Col>) -> Relation {
        Relation {
            qualified_name: name.into(),
            oid,
            name: name.into(),
            quoted: name.into(),
            kind,
            row_estimate: Some(10),
            columns: columns
                .into_iter()
                .enumerate()
                .map(|(i, c)| ColumnInfo {
                    writable: true,
                    attnum: i as i16 + 1,
                    name: c.name.into(),
                    quoted: c.name.into(),
                    type_display: format!("{:?}", c.category),
                    category: c.category,
                    not_null: c.pk,
                    pk: c.pk,
                    fk: c.fk,
                })
                .collect(),
        }
    }
    fn table(oid: u32, name: &str, columns: Vec<Col>) -> Relation {
        relation(oid, name, RelKind::Table, columns)
    }

    fn schema(name: &str, relations: Vec<Relation>) -> SchemaInfo {
        SchemaInfo {
            name: name.into(),
            quoted: name.into(),
            system: false,
            table_count: relations.len() as u32,
            relations,
            functions: vec![],
            sequences: vec![],
            types: vec![],
            synonyms: vec![],
        }
    }

    fn catalog(schemas: Vec<SchemaInfo>) -> Catalog {
        Catalog {
            database_kind: DatabaseKind::Postgres,
            db: DatabaseRef { connection_id: ConnectionId(uuid::Uuid::nil()), database: "shop".into() },
            version: 7,
            default_schema: Some("public".into()),
            schemas,
            extensions: vec![],
        }
    }

    fn key(id: u32, source: u32, target: u32, pairs: &[(&str, &str)]) -> DeclaredKey {
        DeclaredKey {
            id,
            name: format!("fk_{id}"),
            source,
            target,
            columns: pairs.iter().map(|(s, t)| MapColumnPair { source: (*s).into(), target: (*t).into() }).collect(),
            enabled: true,
            validated: true,
        }
    }

    fn inferred(tables: &[Relation]) -> Vec<(u32, String, u32, String)> {
        infer(&tables.iter().collect::<Vec<_>>(), &[])
            .into_iter()
            .map(|l| {
                assert_eq!(l.basis, LinkBasis::Inferred);
                assert_eq!(l.columns.len(), 1);
                let pair = &l.columns[0];
                (l.source, pair.source.clone(), l.target, pair.target.clone())
            })
            .collect()
    }

    fn link(source: u32, column: &str, target: u32, key: &str) -> (u32, String, u32, String) {
        (source, column.into(), target, key.into())
    }

    #[test]
    fn infer_links_a_stem_and_id_column_to_the_table_it_names() {
        let tables = [
            table(1, "orders", vec![pk("id")]),
            table(2, "order_audit", vec![pk("id"), int("order_id")]),
            table(3, "categories", vec![pk("id"), int("parent_id")]),
            table(4, "products", vec![pk("id"), int("category_id")]),
            table(5, "addresses", vec![pk("id")]),
            table(6, "boxes", vec![pk("id")]),
            table(7, "shipments", vec![pk("id"), int("address_id"), int("box_id")]),
            table(8, "Customers", vec![pk("Id")]),
            table(9, "Invoices", vec![pk("Id"), int("CustomerId")]),
        ];
        assert_eq!(
            inferred(&tables),
            [
                link(2, "order_id", 1, "id"),
                link(4, "category_id", 3, "id"),
                link(7, "address_id", 5, "id"),
                link(7, "box_id", 6, "id"),
                link(9, "CustomerId", 8, "Id"),
            ]
        );
    }

    #[test]
    fn infer_links_a_shared_key_name_only_when_it_reads_as_a_reference() {
        let tables = [
            table(1, "Customer", vec![pk("CustomerID")]),
            table(2, "SalesOrderHeader", vec![pk("SalesOrderID"), int("CustomerID")]),
            table(3, "SalesOrderDetail", vec![pk("SalesOrderID"), pk("LineID")]),
            table(4, "coupons", vec![Col { pk: true, ..text("code") }]),
            table(5, "tickets", vec![pk("id"), text("code")]),
            table(6, "Person", vec![pk("PersonID")]),
            table(7, "Employee", vec![pk("PersonID")]),
        ];
        assert_eq!(
            inferred(&tables),
            [link(2, "CustomerID", 1, "CustomerID"), link(3, "SalesOrderID", 2, "SalesOrderID"),],
            "the header's own key does not point back at the detail, two tables keyed by PersonID do not point at each other, and tickets.code is no reference"
        );
    }

    #[test]
    fn infer_draws_nothing_for_two_candidates_a_declared_column_a_type_mismatch_or_itself() {
        let tables = [
            table(1, "products", vec![pk("id")]),
            table(2, "inventory", vec![pk("product_id")]),
            table(3, "reviews", vec![pk("id"), int("product_id")]),
            table(4, "orders", vec![pk("id"), int("order_id")]),
            table(5, "payments", vec![pk("id"), fk("order_id"), text("customer_id")]),
            table(6, "customers", vec![pk("id")]),
            table(7, "status", vec![pk("id")]),
            table(8, "events", vec![pk("event_id"), pk("at"), int("status_id")]),
        ];
        assert_eq!(
            inferred(&tables),
            [link(2, "product_id", 1, "id"), link(8, "status_id", 7, "id"),],
            "reviews.product_id has two candidates, orders.order_id is its own table, payments.order_id is declared, and payments.customer_id is text"
        );
    }

    #[test]
    fn build_folds_partitions_drops_views_and_draws_external_ends_with_their_key_columns() {
        let catalog = catalog(vec![
            schema(
                "public",
                vec![
                    table(1, "orders", vec![pk("id"), fk("customer_id"), int("total")]),
                    relation(2, "events", RelKind::PartitionedTable, vec![int("order_id")]),
                    table(3, "events_h1", vec![int("order_id")]),
                    relation(4, "events_h2", RelKind::PartitionedTable, vec![int("order_id")]),
                    table(5, "events_h2_a", vec![int("order_id")]),
                    relation(6, "paid", RelKind::View, vec![pk("id")]),
                    relation(7, "daily", RelKind::MaterializedView, vec![int("order_id")]),
                ],
            ),
            schema("crm", vec![table(10, "customers", vec![pk("id"), text("name"), int("region_id")])]),
        ]);
        let read = SchemaKeys {
            keys: vec![key(100, 1, 10, &[("customer_id", "id")])],
            partitions: vec![
                Partition { partition: 3, parent: 2 },
                Partition { partition: 4, parent: 2 },
                Partition { partition: 5, parent: 4 },
            ],
        };
        let map = build(&catalog, &catalog.schemas[0], read);
        let tables: Vec<_> =
            map.tables.iter().map(|t| (t.schema.as_str(), t.name.as_str(), t.partitions, t.external)).collect();
        assert_eq!(
            tables,
            [("public", "orders", 0, false), ("public", "events", 3, false), ("crm", "customers", 0, true),]
        );
        let customers = &map.tables[2];
        assert_eq!(
            customers.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            ["id"],
            "an external table lists its key columns only"
        );
        assert_eq!(map.links.len(), 2);
        let declared = &map.links[0];
        assert_eq!(
            (declared.id.as_str(), declared.basis, declared.source, declared.target),
            ("k100", LinkBasis::Declared, 1, 10)
        );
        let inferred = &map.links[1];
        assert_eq!(
            (inferred.id.as_str(), inferred.basis, inferred.source, inferred.target),
            ("i2.1", LinkBasis::Inferred, 2, 1)
        );
        assert!(!map.keys_truncated);
        assert!(!map.partitions_truncated);
        assert_eq!(map.omitted_tables, 0);
        assert_eq!((map.schema.as_str(), map.catalog_version), ("public", 7));
    }

    #[test]
    fn build_keeps_composite_pairs_in_order_and_drops_keys_the_catalog_cannot_name() {
        let catalog = catalog(vec![schema(
            "public",
            vec![
                table(1, "addresses", vec![pk("customer_id"), pk("id")]),
                table(2, "orders", vec![pk("id"), fk("customer_id"), fk("ship_address_id")]),
            ],
        )]);
        let read = SchemaKeys {
            keys: vec![
                key(10, 2, 1, &[("customer_id", "customer_id"), ("ship_address_id", "id")]),
                key(11, 2, 99, &[("customer_id", "id")]),
                key(12, 2, 1, &[("renamed", "id")]),
            ],
            partitions: vec![],
        };
        let map = build(&catalog, &catalog.schemas[0], read);
        let links: Vec<_> = map
            .links
            .iter()
            .map(|l| {
                let pairs: Vec<_> = l.columns.iter().map(|p| (p.source.as_str(), p.target.as_str())).collect();
                (l.id.as_str(), pairs)
            })
            .collect();
        assert_eq!(links, [("k10", vec![("customer_id", "customer_id"), ("ship_address_id", "id")])]);
    }

    #[test]
    fn build_reports_partitions_past_the_limit_and_draws_them_as_tables() {
        let catalog = catalog(vec![schema(
            "public",
            vec![
                relation(2, "events", RelKind::PartitionedTable, vec![int("order_id")]),
                table(3, "events_late", vec![int("order_id")]),
            ],
        )]);
        let partitions = (0..PARTITION_LIMIT as u32)
            .map(|i| Partition { partition: 1_000 + i, parent: 2 })
            .chain([Partition { partition: 3, parent: 2 }])
            .collect();
        let map = build(&catalog, &catalog.schemas[0], SchemaKeys { keys: vec![], partitions });
        assert!(map.partitions_truncated);
        let tables: Vec<_> = map.tables.iter().map(|t| (t.name.as_str(), t.partitions)).collect();
        assert_eq!(tables, [("events", PARTITION_LIMIT as u32), ("events_late", 0)]);
    }

    #[test]
    fn build_infers_nothing_for_a_column_a_key_read_after_the_catalog_declares() {
        let catalog = catalog(vec![schema(
            "public",
            vec![table(1, "orders", vec![pk("id")]), table(2, "payments", vec![pk("id"), int("order_id")])],
        )]);
        let read = SchemaKeys { keys: vec![key(50, 2, 1, &[("order_id", "id")])], partitions: vec![] };
        let map = build(&catalog, &catalog.schemas[0], read);
        let links: Vec<_> = map.links.iter().map(|l| (l.id.as_str(), l.basis)).collect();
        assert_eq!(links, [("k50", LinkBasis::Declared)]);
    }

    #[test]
    fn build_infers_nothing_when_a_competing_target_falls_past_the_table_limit() {
        let tables = |with_competitor: bool| {
            let mut own =
                vec![table(1, "reviews", vec![pk("id"), int("product_id")]), table(2, "products", vec![pk("id")])];
            own.extend((3..=TABLE_LIMIT as u32).map(|i| table(i, &format!("filler{i}"), vec![pk("id")])));
            if with_competitor {
                own.push(table(9_000, "inventory", vec![pk("product_id")]));
            }
            catalog(vec![schema("public", own)])
        };
        let links = |catalog: Catalog| {
            let map = build(&catalog, &catalog.schemas[0], SchemaKeys { keys: vec![], partitions: vec![] });
            (map.links.iter().map(|l| (l.source, l.target)).collect::<Vec<_>>(), map.omitted_tables)
        };
        assert_eq!(links(tables(false)), (vec![(1, 2)], 0));
        assert_eq!(
            links(tables(true)),
            (vec![], 1),
            "inventory.product_id makes reviews.product_id ambiguous although inventory is left out"
        );
    }

    #[test]
    fn build_reports_keys_and_tables_past_their_limits() {
        let own: Vec<Relation> = (0..TABLE_LIMIT as u32)
            .map(|i| table(i + 1, &format!("t{i:03}"), vec![pk("id"), fk("other_id")]))
            .collect();
        let catalog = catalog(vec![schema("public", own), schema("other", vec![table(9_000, "far", vec![pk("id")])])]);
        let keys = (0..=KEY_LIMIT as u32)
            .map(|i| key(i + 1, 1 + i % TABLE_LIMIT as u32, 9_000, &[("other_id", "id")]))
            .collect();
        let map = build(&catalog, &catalog.schemas[0], SchemaKeys { keys, partitions: vec![] });
        assert!(map.keys_truncated);
        assert_eq!(map.tables.len(), TABLE_LIMIT);
        assert_eq!(map.omitted_tables, 1, "the external table past the limit");
        assert!(map.tables.iter().all(|t| !t.external));
        assert!(map.links.is_empty(), "every key reached the omitted table");
    }
}
