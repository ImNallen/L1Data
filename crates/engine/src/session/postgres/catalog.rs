use std::collections::HashMap;

use tokio_postgres::Client;

use super::types;
use crate::connection::DatabaseInfo;
use crate::model::{
    Catalog, ColumnInfo, DatabaseKind, DatabaseRef, DbAccess, ExtensionInfo, FnKind, FunctionInfo,
    RelKind, Relation, SchemaInfo, SequenceInfo, TypeField, TypeInfo, TypeKind,
};
use crate::text::{quote_function_if_needed, quote_if_needed};

/// Postgres reserves the `pg_` prefix for its own schemas. `view::is_system_schema` is the same
/// rule in Rust.
const SYSTEM_SCHEMA: &str = "(n.nspname = 'information_schema' OR n.nspname LIKE 'pg\\_%')";
/// Toast and temp schemas are listed nowhere.
const UNLISTED_SCHEMA: &str = "(n.nspname = 'pg_toast' OR n.nspname LIKE 'pg\\_temp\\_%' OR n.nspname LIKE 'pg\\_toast\\_temp\\_%')";
/// Relations and columns: every user schema, plus the system schemas named in `$1`.
const RELATION_SCHEMA_FILTER: &str = "NOT (n.nspname = 'pg_toast' OR n.nspname LIKE 'pg\\_temp\\_%' OR n.nspname LIKE 'pg\\_toast\\_temp\\_%')
    AND (NOT (n.nspname = 'information_schema' OR n.nspname LIKE 'pg\\_%') OR n.nspname = ANY($1))";
/// Functions: user schemas only. `pg_catalog` alone has about 3,000.
const FUNCTION_SCHEMA_FILTER: &str = "NOT (n.nspname = 'information_schema' OR n.nspname LIKE 'pg\\_%')";

/// Every database on the server, plus the server's spelling of the one this session is on.
pub async fn list_databases(client: &Client) -> Result<(Vec<DatabaseInfo>, String), tokio_postgres::Error> {
    let rows = client
        .query(
            "SELECT datname, datistemplate OR NOT datallowconn, datallowconn, has_database_privilege(datname, 'CONNECT'),
                    datname = current_database()
             FROM pg_database ORDER BY datname",
            &[],
        )
        .await?;
    let mut home = String::new();
    let databases = rows
        .iter()
        .map(|r| {
            let name: String = r.get(0);
            if r.get::<_, bool>(4) {
                home = name.clone();
            }
            let access = if !r.get::<_, bool>(2) {
                DbAccess::NoConnections
            } else if !r.get::<_, bool>(3) {
                DbAccess::NoPrivilege
            } else {
                DbAccess::Ok
            };
            DatabaseInfo { name, system: r.get(1), access }
        })
        .collect();
    Ok((databases, home))
}

/// The non-system schema count, run on a short-lived probe session.
pub async fn count_schemas(client: &Client) -> Result<u32, tokio_postgres::Error> {
    let row = client.query_one(&format!("SELECT count(*) FROM pg_namespace n WHERE NOT {SYSTEM_SCHEMA}"), &[]).await?;
    Ok(row.get::<_, i64>(0) as u32)
}

/// The first non-system schema of the search path that exists, else `public`, else the first
/// non-system schema by name. A search path that starts with `pg_catalog` must not make the
/// default a system schema: "Default only" would then show a schema the picker hides.
fn default_schema(search_path: &[String], schemas: &[SchemaInfo]) -> Option<String> {
    let user = |name: &str| schemas.iter().find(|s| s.name == name && !s.system);
    search_path
        .iter()
        .find_map(|s| user(s))
        .or_else(|| user("public"))
        .or_else(|| schemas.iter().find(|s| !s.system))
        .map(|s| s.name.clone())
}

/// Eight queries on one database's metadata session. Every schema is listed with its `system` flag
/// and table count. Relations and columns load for user schemas and for the system schemas in
/// `extra_schemas`. Functions and sequences load for user schemas only. Types load for user
/// schemas, plus the base and pseudo types of a system schema named in `extra_schemas`. Each query
/// reads its own snapshot, so a schema whose relations load counts its tables from them, and DDL
/// between the queries cannot make the count disagree with the list.
pub async fn load(
    client: &Client,
    db: DatabaseRef,
    version: u64,
    extra_schemas: &[String],
) -> Result<Catalog, tokio_postgres::Error> {
    let schema_rows = client
        .query(
            &format!(
                "SELECT n.nspname, {SYSTEM_SCHEMA},
                        (SELECT count(*) FROM pg_class c WHERE c.relnamespace = n.oid AND c.relkind IN ('r', 'p', 'f'))::int4
                 FROM pg_namespace n WHERE NOT {UNLISTED_SCHEMA} ORDER BY n.nspname"
            ),
            &[],
        )
        .await?;
    let mut schemas: Vec<SchemaInfo> = schema_rows
        .iter()
        .map(|r| SchemaInfo {
            name: r.get(0),
            quoted: quote_if_needed(DatabaseKind::Postgres, r.get(0)),
            system: r.get(1),
            table_count: r.get::<_, i32>(2) as u32,
            relations: Vec::new(),
            functions: Vec::new(),
            sequences: Vec::new(),
            types: Vec::new(),
            synonyms: Vec::new(),
        })
        .collect();
    let schema_index: HashMap<String, usize> = schemas.iter().enumerate().map(|(i, s)| (s.name.clone(), i)).collect();

    let search_path: Vec<String> = client.query_one("SELECT current_schemas(false)::text[]", &[]).await?.get(0);
    let default_schema = default_schema(&search_path, &schemas);

    let relation_rows = client
        .query(
            &format!(
                "SELECT c.oid, n.nspname, c.relname, c.relkind::text, c.reltuples::float8
                 FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
                 WHERE c.relkind IN ('r', 'p', 'v', 'm', 'f') AND {RELATION_SCHEMA_FILTER}
                 ORDER BY c.relname"
            ),
            &[&extra_schemas],
        )
        .await?;
    let mut relation_index: HashMap<u32, (usize, usize)> = HashMap::new();
    for r in &relation_rows {
        let oid: u32 = r.get(0);
        let schema: String = r.get(1);
        let kind = match r.get::<_, String>(3).as_str() {
            "p" => RelKind::PartitionedTable,
            "v" => RelKind::View,
            "m" => RelKind::MaterializedView,
            "f" => RelKind::ForeignTable,
            _ => RelKind::Table,
        };
        let reltuples: f64 = r.get(4);
        let Some(&si) = schema_index.get(&schema) else { continue };
        relation_index.insert(oid, (si, schemas[si].relations.len()));
        schemas[si].relations.push(Relation {
            oid,
            name: r.get(2),
            quoted: quote_if_needed(DatabaseKind::Postgres, r.get(2)),
            qualified_name: format!("{}.{}", crate::text::quote_ident(&schema), crate::text::quote_ident(r.get(2))),
            kind,
            row_estimate: (reltuples >= 0.0).then_some(reltuples.round() as i64),
            columns: Vec::new(),
        });
    }
    for schema in schemas.iter_mut().filter(|s| !s.system || extra_schemas.contains(&s.name)) {
        schema.table_count = schema.relations.iter().filter(|r| !matches!(r.kind, RelKind::View | RelKind::MaterializedView)).count() as u32;
    }

    let column_rows = client
        .query(
            &format!(
                "SELECT a.attrelid, a.attnum, a.attname, format_type(a.atttypid, a.atttypmod), a.attnotnull,
                        EXISTS (SELECT 1 FROM pg_index i WHERE i.indrelid = a.attrelid AND i.indisprimary AND a.attnum = ANY (i.indkey)),
                        EXISTS (SELECT 1 FROM pg_constraint k WHERE k.conrelid = a.attrelid AND k.contype = 'f' AND a.attnum = ANY (k.conkey)),
                        t.oid, t.typtype, t.typcategory, a.attgenerated = '' AND a.attidentity <> 'a'
                 FROM pg_attribute a
                 JOIN pg_class c ON c.oid = a.attrelid JOIN pg_namespace n ON n.oid = c.relnamespace
                 JOIN pg_type declared ON declared.oid = a.atttypid
                 JOIN pg_type t ON t.oid = CASE WHEN declared.typtype = 'd' THEN declared.typbasetype ELSE declared.oid END
                 WHERE a.attnum > 0 AND NOT a.attisdropped AND c.relkind IN ('r', 'p', 'v', 'm', 'f') AND {RELATION_SCHEMA_FILTER}
                 ORDER BY a.attrelid, a.attnum"
            ),
            &[&extra_schemas],
        )
        .await?;
    for r in &column_rows {
        let Some(&(si, ri)) = relation_index.get(&r.get::<_, u32>(0)) else { continue };
        schemas[si].relations[ri].columns.push(ColumnInfo {
            writable: r.get(10),
            attnum: r.get(1),
            name: r.get(2),
            quoted: quote_if_needed(DatabaseKind::Postgres, r.get(2)),
            type_display: r.get(3),
            category: types::catalog_category(r.get(7), r.get(8), r.get(9)),
            not_null: r.get(4),
            pk: r.get(5),
            fk: r.get(6),
        });
    }

    let function_rows = client
        .query(
            &format!(
                "SELECT p.oid, n.nspname, p.proname, pg_get_function_identity_arguments(p.oid),
                        coalesce(pg_get_function_result(p.oid), ''), p.prokind::text
                 FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
                 WHERE {FUNCTION_SCHEMA_FILTER}
                 ORDER BY p.proname, p.oid"
            ),
            &[],
        )
        .await?;
    for r in &function_rows {
        let Some(&si) = schema_index.get(&r.get::<_, String>(1)) else { continue };
        // NULL for a function dropped after this query took its snapshot.
        let Some(args) = r.get::<_, Option<String>>(3) else { continue };
        let kind = match r.get::<_, String>(5).as_str() {
            "p" => FnKind::Procedure,
            "a" => FnKind::Aggregate,
            "w" => FnKind::Window,
            _ => FnKind::Function,
        };
        schemas[si].functions.push(FunctionInfo {
            oid: r.get(0),
            name: r.get(2),
            quoted: quote_function_if_needed(DatabaseKind::Postgres, r.get(2)),
            args,
            returns: r.get(4),
            kind,
            insert_text: format!("{}()", quote_function_if_needed(DatabaseKind::Postgres, r.get(2))),
        });
    }

    let sequence_rows = client
        .query(
            &format!(
                "SELECT c.oid, n.nspname, c.relname, format_type(s.seqtypid, NULL)
                 FROM pg_class c
                 JOIN pg_namespace n ON n.oid = c.relnamespace
                 JOIN pg_sequence s ON s.seqrelid = c.oid
                 WHERE c.relkind = 'S' AND {FUNCTION_SCHEMA_FILTER}
                 ORDER BY c.relname, c.oid"
            ),
            &[],
        )
        .await?;
    for r in &sequence_rows {
        let Some(&si) = schema_index.get(&r.get::<_, String>(1)) else {
            continue;
        };
        let name: String = r.get(2);
        schemas[si].sequences.push(SequenceInfo {
            oid: r.get(0),
            quoted: quote_if_needed(DatabaseKind::Postgres, &name),
            name,
            type_name: r
                .get::<_, Option<String>>(3)
                .unwrap_or_else(|| "unknown".into()),
        });
    }

    // chr(31) and chr(30) separate labels and fields. `format!` would eat braces, and a label can contain a comma.
    let type_rows = client
        .query(
            &format!(
                "SELECT t.oid, n.nspname, t.typname, t.typtype::text,
                        CASE
                          WHEN t.typtype = 'e' THEN COALESCE((SELECT string_agg(e.enumlabel, chr(31) ORDER BY e.enumsortorder) FROM pg_enum e WHERE e.enumtypid = t.oid), '')
                          WHEN t.typtype = 'd' THEN format_type(t.typbasetype, t.typtypmod)
                          WHEN t.typtype = 'c' THEN COALESCE((SELECT string_agg(a.attname || chr(31) || format_type(a.atttypid, a.atttypmod), chr(30) ORDER BY a.attnum) FROM pg_attribute a WHERE a.attrelid = t.typrelid AND a.attnum > 0 AND NOT a.attisdropped), '')
                          WHEN t.typtype = 'r' THEN COALESCE(format_type(r.rngsubtype, NULL), '')
                          ELSE ''
                        END
                 FROM pg_type t
                 JOIN pg_namespace n ON n.oid = t.typnamespace
                 LEFT JOIN pg_range r ON r.rngtypid = t.oid
                 WHERE NOT {UNLISTED_SCHEMA}
                   AND (NOT {SYSTEM_SCHEMA} OR n.nspname = ANY($1))
                   AND t.typisdefined
                   AND t.typtype IN ('b', 'c', 'd', 'e', 'p', 'r')
                   AND NOT (t.typtype IN ('b', 'p') AND t.typcategory = 'A')
                   AND NOT EXISTS (SELECT 1 FROM pg_class rel WHERE rel.oid = t.typrelid AND rel.relkind <> 'c')
                 ORDER BY t.typname, t.oid"
            ),
            &[&extra_schemas],
        )
        .await?;
    for r in &type_rows {
        let Some(&si) = schema_index.get(&r.get::<_, String>(1)) else {
            continue;
        };
        let Some(kind) = type_kind(&r.get::<_, String>(3), &r.get::<_, String>(4)) else {
            continue;
        };
        let name: String = r.get(2);
        schemas[si].types.push(TypeInfo {
            oid: r.get(0),
            quoted: quote_if_needed(DatabaseKind::Postgres, &name),
            name,
            kind,
        });
    }

    let extension_rows = client
        .query(
            "SELECT e.oid, e.extname, e.extversion
             FROM pg_extension e
             ORDER BY e.extname, e.oid",
            &[],
        )
        .await?;
    let extensions = extension_rows
        .iter()
        .map(|r| {
            let name: String = r.get(1);
            ExtensionInfo {
                oid: r.get(0),
                quoted: quote_if_needed(DatabaseKind::Postgres, &name),
                name,
                version: r.get::<_, Option<String>>(2).unwrap_or_default(),
            }
        })
        .collect();

    Ok(Catalog {
        database_kind: DatabaseKind::Postgres,
        db,
        version,
        default_schema,
        schemas,
        extensions,
    })
}

fn type_kind(typtype: &str, payload: &str) -> Option<TypeKind> {
    match typtype {
        "e" => Some(TypeKind::Enum {
            labels: split_packed(payload, '\u{1f}'),
        }),
        "d" => Some(TypeKind::Domain {
            base: payload.to_owned(),
        }),
        "c" => Some(TypeKind::Composite {
            fields: composite_fields(payload),
        }),
        "r" => Some(TypeKind::Range {
            subtype: payload.to_owned(),
        }),
        "b" | "p" => Some(TypeKind::Builtin),
        _ => None,
    }
}

fn split_packed(payload: &str, sep: char) -> Vec<String> {
    if payload.is_empty() {
        Vec::new()
    } else {
        payload.split(sep).map(str::to_owned).collect()
    }
}

fn composite_fields(payload: &str) -> Vec<TypeField> {
    split_packed(payload, '\u{1e}')
        .into_iter()
        .filter_map(|field| {
            let (name, type_name) = field.split_once('\u{1f}')?;
            Some(TypeField {
                name: name.to_owned(),
                type_name: type_name.to_owned(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema(name: &str, system: bool) -> SchemaInfo {
        SchemaInfo {
            name: name.into(),
            quoted: name.into(),
            system,
            table_count: 0,
            relations: Vec::new(),
            functions: Vec::new(),
            sequences: Vec::new(),
            types: Vec::new(),
            synonyms: Vec::new(),
        }
    }

    fn path(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| (*n).to_owned()).collect()
    }

    #[test]
    fn the_default_schema_is_never_a_system_schema() {
        let schemas = [schema("audit", false), schema("information_schema", true), schema("pg_catalog", true), schema("public", false)];
        assert_eq!(default_schema(&path(&["pg_catalog", "public"]), &schemas).as_deref(), Some("public"));
        assert_eq!(default_schema(&path(&["pg_catalog", "audit"]), &schemas).as_deref(), Some("audit"));
        assert_eq!(default_schema(&path(&["missing"]), &schemas).as_deref(), Some("public"), "a search path with nothing to show falls back to public");
        let no_public = [schema("audit", false), schema("pg_catalog", true)];
        assert_eq!(default_schema(&path(&["pg_catalog"]), &no_public).as_deref(), Some("audit"), "then the first user schema");
        assert_eq!(default_schema(&path(&["pg_catalog"]), &[schema("pg_catalog", true)]), None, "a database with only system schemas has no default");
    }

    #[test]
    fn type_payloads_become_the_kind_the_row_draws() {
        assert_eq!(
            type_kind("e", "sad\u{1f}ok\u{1f}happy"),
            Some(TypeKind::Enum {
                labels: vec!["sad".into(), "ok".into(), "happy".into()]
            })
        );
        assert_eq!(
            type_kind("e", ""),
            Some(TypeKind::Enum { labels: Vec::new() })
        );
        assert_eq!(
            type_kind("d", "text"),
            Some(TypeKind::Domain {
                base: "text".into()
            })
        );
        assert_eq!(
            type_kind("c", "id\u{1f}integer\u{1e}kind\u{1f}text"),
            Some(TypeKind::Composite {
                fields: vec![
                    TypeField {
                        name: "id".into(),
                        type_name: "integer".into()
                    },
                    TypeField {
                        name: "kind".into(),
                        type_name: "text".into()
                    }
                ]
            })
        );
        assert_eq!(
            type_kind("c", ""),
            Some(TypeKind::Composite { fields: Vec::new() })
        );
        assert_eq!(
            type_kind("r", "integer"),
            Some(TypeKind::Range {
                subtype: "integer".into()
            })
        );
        assert_eq!(type_kind("b", ""), Some(TypeKind::Builtin));
        assert_eq!(type_kind("p", ""), Some(TypeKind::Builtin));
        assert_eq!(type_kind("m", "int4multirange"), None);
    }
}
