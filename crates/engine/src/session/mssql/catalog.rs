use super::{MsSession, error, number, object_id, required};
use crate::connection::DatabaseInfo;
use crate::model::*;
use crate::sql::tsql::{quote_ident, quote_literal};
use crate::text::quote_if_needed;
use std::collections::HashMap;

const SYSTEM: &str = "(s.name IN ('sys', 'INFORMATION_SCHEMA') OR s.schema_id BETWEEN 16384 AND 16393)";

fn routine_insert_text(schema: &str, name: &str, kind: FnKind, arguments: &[String]) -> String {
    let qualified = format!("{}.{}", quote_ident(schema), quote_ident(name));
    let args = arguments.join(", ");
    match kind {
        FnKind::Procedure if arguments.is_empty() => format!("EXEC {qualified}"),
        FnKind::Procedure => format!("EXEC {qualified} {args}"),
        FnKind::Function | FnKind::Aggregate | FnKind::Window => format!("{qualified}({args})"),
    }
}

pub(super) fn category(name: &str) -> TypeCategory {
    match name {
        "tinyint" | "smallint" | "int" | "bigint" => TypeCategory::Integer,
        "decimal" | "numeric" | "money" | "smallmoney" | "float" | "real" => TypeCategory::Numeric,
        "bit" => TypeCategory::Bool,
        "date" | "time" | "smalldatetime" | "datetime" | "datetime2" | "datetimeoffset" => TypeCategory::Temporal,
        "binary" | "varbinary" | "image" | "timestamp" | "rowversion" => TypeCategory::Binary,
        "uniqueidentifier" => TypeCategory::Uuid,
        "char" | "varchar" | "nchar" | "nvarchar" | "text" | "ntext" | "xml" => TypeCategory::Text,
        _ => TypeCategory::Other,
    }
}

pub(super) fn display(name: &str, length: i32, precision: u8, scale: u8) -> String {
    match name {
        "nvarchar" | "nchar" | "varchar" | "char" | "varbinary" | "binary" => {
            let size = if length == -1 {
                "max".into()
            } else {
                (if name.starts_with('n') { length / 2 } else { length }).to_string()
            };
            format!("{name}({size})")
        }
        "decimal" | "numeric" => format!("{name}({precision},{scale})"),
        "time" | "datetime2" | "datetimeoffset" => format!("{name}({scale})"),
        _ => name.to_owned(),
    }
}

impl MsSession {
    pub(in crate::session) async fn list_databases(&mut self) -> Result<(Vec<DatabaseInfo>, String), QueryError> {
        let rows = self.rows("SELECT name, CASE WHEN database_id <= 4 THEN 1 ELSE 0 END, state, HAS_DBACCESS(name) FROM sys.databases ORDER BY name").await?;
        let databases = rows
            .iter()
            .map(|r| {
                Ok(DatabaseInfo {
                    name: required(r, 0)?.to_owned(),
                    system: number::<u8>(r, 1)? != 0,
                    access: if number::<u8>(r, 2)? != 0 {
                        DbAccess::NoConnections
                    } else if r[3].as_deref() != Some("1") {
                        DbAccess::NoPrivilege
                    } else {
                        DbAccess::Ok
                    },
                })
            })
            .collect::<Result<Vec<_>, QueryError>>()?;
        Ok((databases, self.database.clone()))
    }
    pub(in crate::session) async fn count_schemas(&mut self) -> Result<u32, QueryError> {
        let rows = self.rows(&format!("SELECT COUNT(*) FROM sys.schemas s WHERE NOT {SYSTEM}")).await?;
        number(rows.first().ok_or_else(|| error("Schema count returned no row"))?, 0)
    }
    pub(in crate::session) async fn catalog(
        &mut self,
        db: DatabaseRef,
        version: u64,
        extra: &[String],
    ) -> Result<Catalog, QueryError> {
        let rows = self.rows(&format!("SELECT s.name, CASE WHEN {SYSTEM} THEN 1 ELSE 0 END, (SELECT COUNT(*) FROM sys.all_objects o WHERE o.schema_id=s.schema_id AND o.type='U') FROM sys.schemas s ORDER BY s.name")).await?;
        let mut schemas = rows
            .iter()
            .map(|r| {
                let name = required(r, 0)?;
                Ok(SchemaInfo {
                    name: name.into(),
                    quoted: quote_if_needed(DatabaseKind::SqlServer, name),
                    system: number::<u8>(r, 1)? != 0,
                    table_count: number(r, 2)?,
                    relations: vec![],
                    functions: vec![],
                    sequences: vec![],
                    types: vec![],
                    synonyms: vec![],
                })
            })
            .collect::<Result<Vec<_>, QueryError>>()?;
        let schema_index: HashMap<String, usize> =
            schemas.iter().enumerate().map(|(i, s)| (s.name.clone(), i)).collect();
        let defaults = self.rows("SELECT SCHEMA_NAME()").await?;
        let default_name = defaults.first().and_then(|r| r[0].as_deref());
        let default_schema = schemas
            .iter()
            .find(|s| Some(s.name.as_str()) == default_name && !s.system)
            .or_else(|| schemas.iter().find(|s| s.name == "dbo"))
            .or_else(|| schemas.iter().find(|s| !s.system))
            .map(|s| s.name.clone());
        let extra = if extra.is_empty() {
            "NULL".into()
        } else {
            extra.iter().map(|s| quote_literal(s)).collect::<Vec<_>>().join(",")
        };
        let filter = format!("(NOT {SYSTEM} OR s.name IN ({extra}))");
        let rows = self.rows(&format!("SELECT o.object_id, s.name, o.name, o.type, (SELECT SUM(p.rows) FROM sys.partitions p WHERE p.object_id=o.object_id AND p.index_id IN (0,1)) FROM sys.all_objects o JOIN sys.schemas s ON s.schema_id=o.schema_id WHERE o.type IN ('U','V') AND {filter} ORDER BY o.name")).await?;
        let mut relation_index = HashMap::new();
        for r in rows {
            let oid = object_id(&r, 0)?;
            let schema = required(&r, 1)?;
            let name = required(&r, 2)?.to_owned();
            let si = *schema_index.get(schema).ok_or_else(|| error("Catalog schema changed during load"))?;
            relation_index.insert(oid, (si, schemas[si].relations.len()));
            schemas[si].relations.push(Relation {
                oid,
                qualified_name: format!("{}.{}", quote_ident(schema), quote_ident(&name)),
                quoted: quote_if_needed(DatabaseKind::SqlServer, &name),
                name,
                kind: if required(&r, 3)?.trim() == "V" { RelKind::View } else { RelKind::Table },
                row_estimate: r[4].as_deref().map(str::parse).transpose().map_err(|_| error("Invalid row estimate"))?,
                columns: vec![],
            });
        }
        let rows = self.rows(&format!("SELECT c.object_id,c.column_id,c.name,COALESCE(t.name,declared.name,'unknown'),c.max_length,c.precision,c.scale,c.is_nullable,c.is_identity,c.is_computed,
            CASE WHEN EXISTS (SELECT 1 FROM sys.indexes i JOIN sys.index_columns ic ON ic.object_id=i.object_id AND ic.index_id=i.index_id WHERE i.object_id=c.object_id AND i.is_primary_key=1 AND ic.column_id=c.column_id) THEN 1 ELSE 0 END,
            CASE WHEN EXISTS (SELECT 1 FROM sys.foreign_key_columns fk WHERE fk.parent_object_id=c.object_id AND fk.parent_column_id=c.column_id) THEN 1 ELSE 0 END,
            c.generated_always_type,
            CASE WHEN c.system_type_id=240 THEN 1 ELSE 0 END AS is_clr
            FROM sys.all_columns c JOIN sys.all_objects o ON o.object_id=c.object_id JOIN sys.schemas s ON s.schema_id=o.schema_id
            LEFT JOIN sys.types declared ON declared.user_type_id=c.user_type_id
            LEFT JOIN sys.types t ON t.user_type_id=c.system_type_id AND t.system_type_id=t.user_type_id
            WHERE o.type IN ('U','V') AND {filter} ORDER BY c.object_id,c.column_id")).await?;
        for r in rows {
            let Some(&(si, ri)) = relation_index.get(&object_id(&r, 0)?) else {
                continue;
            };
            let name = required(&r, 3)?;
            schemas[si].relations[ri].columns.push(ColumnInfo {
                attnum: number(&r, 1)?,
                name: required(&r, 2)?.into(),
                quoted: quote_if_needed(DatabaseKind::SqlServer, required(&r, 2)?),
                type_display: display(name, number(&r, 4)?, number(&r, 5)?, number(&r, 6)?),
                category: if number::<u8>(&r, 13)? != 0 { TypeCategory::Binary } else { category(name) },
                not_null: number::<u8>(&r, 7)? == 0,
                writable: number::<u8>(&r, 8)? == 0
                    && number::<u8>(&r, 9)? == 0
                    && number::<u8>(&r, 12)? == 0
                    && !matches!(name, "timestamp" | "rowversion"),
                pk: number::<u8>(&r, 10)? != 0,
                fk: number::<u8>(&r, 11)? != 0,
            });
        }
        let rows = self.rows(&format!("SELECT o.object_id,s.name,o.name,o.type,COALESCE(TYPE_NAME(p.user_type_id),TYPE_NAME(p.system_type_id),CASE WHEN p.parameter_id=0 THEN 'unknown' ELSE '' END) FROM sys.objects o JOIN sys.schemas s ON s.schema_id=o.schema_id LEFT JOIN sys.parameters p ON p.object_id=o.object_id AND p.parameter_id=0 WHERE o.type IN ('P','PC','FN','IF','TF','FS','FT') AND NOT {SYSTEM} ORDER BY o.name,o.object_id")).await?;
        let mut routine_index = HashMap::new();
        for r in rows {
            let si = *schema_index.get(required(&r, 1)?).ok_or_else(|| error("Routine schema changed during load"))?;
            let oid = object_id(&r, 0)?;
            routine_index.insert(oid, (si, schemas[si].functions.len(), Vec::<String>::new()));
            schemas[si].functions.push(FunctionInfo {
                oid,
                name: required(&r, 2)?.into(),
                quoted: quote_if_needed(DatabaseKind::SqlServer, required(&r, 2)?),
                args: String::new(),
                returns: required(&r, 4)?.into(),
                kind: if matches!(required(&r, 3)?.trim(), "P" | "PC") { FnKind::Procedure } else { FnKind::Function },
                insert_text: String::new(),
            });
        }
        let rows =
            self.rows("SELECT p.object_id,p.name,COALESCE(TYPE_NAME(p.user_type_id),TYPE_NAME(p.system_type_id),'unknown'),p.is_output,p.is_readonly FROM sys.parameters p WHERE p.parameter_id>0 ORDER BY p.object_id,p.parameter_id").await?;
        for r in rows {
            let oid = object_id(&r, 0)?;
            if let Some((si, fi, arguments)) = routine_index.get_mut(&oid) {
                let f = &mut schemas[*si].functions[*fi];
                if !f.args.is_empty() {
                    f.args.push_str(", ");
                }
                f.args.push_str(&format!(
                    "{} {}{}",
                    required(&r, 1)?,
                    required(&r, 2)?,
                    if number::<u8>(&r, 3)? != 0 { " OUTPUT" } else { "" }
                ));
                arguments.push(if number::<u8>(&r, 4)? != 0 {
                    format!("@table_argument_{}", arguments.len() + 1)
                } else {
                    "NULL".into()
                });
            }
        }
        for (_, (si, fi, arguments)) in routine_index {
            let schema = &mut schemas[si];
            let f = &mut schema.functions[fi];
            f.insert_text = routine_insert_text(&schema.name, &f.name, f.kind, &arguments);
        }
        let rows = self.rows(&format!("SELECT seq.object_id, s.name, seq.name, TYPE_NAME(seq.system_type_id), seq.precision, seq.scale FROM sys.sequences seq JOIN sys.schemas s ON s.schema_id = seq.schema_id WHERE NOT {SYSTEM} ORDER BY seq.name, seq.object_id")).await?;
        for r in rows {
            let Some(&si) = schema_index.get(required(&r, 1)?) else {
                continue;
            };
            let name = required(&r, 2)?;
            let type_name = required(&r, 3)?;
            schemas[si].sequences.push(SequenceInfo {
                oid: object_id(&r, 0)?,
                name: name.into(),
                quoted: quote_if_needed(DatabaseKind::SqlServer, name),
                type_name: display(type_name, 0, number(&r, 4)?, number(&r, 5)?),
            });
        }
        let rows = self.rows(&format!("SELECT syn.object_id, s.name, syn.name, syn.base_object_name FROM sys.synonyms syn JOIN sys.schemas s ON s.schema_id = syn.schema_id WHERE NOT {SYSTEM} ORDER BY syn.name, syn.object_id")).await?;
        for r in rows {
            let Some(&si) = schema_index.get(required(&r, 1)?) else {
                continue;
            };
            let name = required(&r, 2)?;
            schemas[si].synonyms.push(SynonymInfo {
                oid: object_id(&r, 0)?,
                name: name.into(),
                quoted: quote_if_needed(DatabaseKind::SqlServer, name),
                target: required(&r, 3)?.into(),
            });
        }
        let rows = self.rows(&format!("SELECT t.user_type_id, s.name, t.name, t.is_table_type, t.is_nullable, TYPE_NAME(t.system_type_id), t.max_length, t.precision, t.scale FROM sys.types t JOIN sys.schemas s ON s.schema_id = t.schema_id WHERE t.is_user_defined = 1 AND t.is_assembly_type = 0 AND {filter} ORDER BY t.name, t.user_type_id")).await?;
        for r in rows {
            let Some(&si) = schema_index.get(required(&r, 1)?) else {
                continue;
            };
            let name = required(&r, 2)?;
            let kind = if number::<u8>(&r, 3)? != 0 {
                TypeKind::Table
            } else {
                let base = required(&r, 5)?;
                TypeKind::Alias {
                    base: display(base, number(&r, 6)?, number(&r, 7)?, number(&r, 8)?),
                    nullable: number::<u8>(&r, 4)? != 0,
                }
            };
            schemas[si].types.push(TypeInfo {
                oid: object_id(&r, 0)?,
                name: name.into(),
                quoted: quote_if_needed(DatabaseKind::SqlServer, name),
                kind,
            });
        }
        Ok(Catalog { database_kind: DatabaseKind::SqlServer, db, version, default_schema, schemas, extensions: vec![] })
    }
}
