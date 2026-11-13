use super::{MsSession, error, native_object_id, number, required};
use crate::model::*;
use crate::sql::tsql::quote_ident;

#[cfg(all(test, feature = "mssql-tests"))]
#[path = "mssql_metadata_visibility.rs"]
mod mssql_metadata_visibility;

fn action(n: u8) -> Result<FkAction, QueryError> {
    match n {
        0 => Ok(FkAction::NoAction),
        1 => Ok(FkAction::Cascade),
        2 => Ok(FkAction::SetNull),
        3 => Ok(FkAction::SetDefault),
        _ => Err(error("Unknown referential action")),
    }
}

impl MsSession {
    pub(in crate::session) async fn detail(
        &mut self,
        db: DatabaseRef,
        oid: u32,
        version: u64,
    ) -> Result<Option<RelationDetail>, QueryError> {
        let object_id = oid;
        let oid = native_object_id(oid);
        if self.rows(&format!("SELECT 1 FROM sys.all_objects WHERE object_id={oid}")).await?.is_empty() {
            return Ok(None);
        }
        let mut detail = RelationDetail {
            db,
            oid: object_id,
            catalog_version: version,
            defaults: vec![],
            keys: vec![],
            indexes: vec![],
            triggers: vec![],
        };
        let rows = self.rows(&format!("SELECT c.name,c.is_identity,c.is_computed,dc.definition,cc.definition FROM sys.columns c LEFT JOIN sys.default_constraints dc ON dc.object_id=c.default_object_id LEFT JOIN sys.computed_columns cc ON cc.object_id=c.object_id AND cc.column_id=c.column_id WHERE c.object_id={oid} AND (c.is_identity=1 OR c.is_computed=1 OR c.default_object_id<>0) ORDER BY c.column_id")).await?;
        for r in rows {
            let column = required(&r, 0)?.into();
            let value = if number::<u8>(&r, 1)? != 0 {
                DefaultValue::Identity { always: true }
            } else if number::<u8>(&r, 2)? != 0 {
                let Some(sql) = &r[4] else { continue };
                DefaultValue::Generated { sql: sql.clone() }
            } else {
                let Some(sql) = &r[3] else { continue };
                DefaultValue::Expr { sql: sql.clone() }
            };
            detail.defaults.push(ColumnDefault { column, value });
        }
        let rows = self.rows(&format!("SELECT k.name,k.type,c.name FROM sys.key_constraints k JOIN sys.index_columns ic ON ic.object_id=k.parent_object_id AND ic.index_id=k.unique_index_id JOIN sys.columns c ON c.object_id=ic.object_id AND c.column_id=ic.column_id WHERE k.parent_object_id={oid} AND ic.key_ordinal>0 ORDER BY CASE k.type WHEN 'PK' THEN 0 ELSE 1 END,k.name,ic.key_ordinal")).await?;
        for r in rows {
            let name = required(&r, 0)?.to_owned();
            let col = required(&r, 2)?.to_owned();
            if let Some(k) = detail.keys.iter_mut().find(|k| k.name == name) {
                k.columns.push(col);
            } else {
                detail.keys.push(KeyInfo {
                    name,
                    columns: vec![col],
                    kind: if required(&r, 1)?.trim() == "PK" { KeyKind::Primary } else { KeyKind::Unique },
                });
            }
        }
        let rows = self.rows(&format!("SELECT fk.name,c.name,rs.name,ro.name,rc.name,fk.delete_referential_action,fk.update_referential_action FROM sys.foreign_keys fk JOIN sys.foreign_key_columns fc ON fc.constraint_object_id=fk.object_id JOIN sys.columns c ON c.object_id=fc.parent_object_id AND c.column_id=fc.parent_column_id JOIN sys.objects ro ON ro.object_id=fc.referenced_object_id JOIN sys.schemas rs ON rs.schema_id=ro.schema_id JOIN sys.columns rc ON rc.object_id=fc.referenced_object_id AND rc.column_id=fc.referenced_column_id WHERE fk.parent_object_id={oid} ORDER BY fk.name,fc.constraint_column_id")).await?;
        for r in rows {
            let name = required(&r, 0)?.to_owned();
            let col = required(&r, 1)?.to_owned();
            let ref_col = required(&r, 4)?.to_owned();
            if let Some(k) = detail.keys.iter_mut().find(|k| k.name == name) {
                k.columns.push(col);
                if let KeyKind::Foreign { ref_columns, .. } = &mut k.kind {
                    ref_columns.push(ref_col);
                }
            } else {
                detail.keys.push(KeyInfo {
                    name,
                    columns: vec![col],
                    kind: KeyKind::Foreign {
                        ref_schema: required(&r, 2)?.into(),
                        ref_table: required(&r, 3)?.into(),
                        ref_columns: vec![ref_col],
                        on_delete: action(number(&r, 5)?)?,
                        on_update: action(number(&r, 6)?)?,
                    },
                });
            }
        }
        let rows = self.rows(&format!("SELECT k.name,c.name,k.definition FROM sys.check_constraints k LEFT JOIN sys.columns c ON c.object_id=k.parent_object_id AND c.column_id=k.parent_column_id WHERE k.parent_object_id={oid} ORDER BY k.name")).await?;
        for r in rows {
            let name = required(&r, 0)?.into();
            let Some(definition) = &r[2] else { continue };
            detail.keys.push(KeyInfo {
                name,
                columns: r[1].clone().into_iter().collect(),
                kind: KeyKind::Check { definition: definition.clone() },
            });
        }
        detail.keys.sort_by_key(|k| {
            (
                match k.kind {
                    KeyKind::Primary => 0,
                    KeyKind::Foreign { .. } => 1,
                    KeyKind::Unique => 2,
                    KeyKind::Check { .. } => 3,
                },
                k.name.clone(),
            )
        });
        let rows = self.rows(&format!("SELECT i.name,i.type_desc,i.is_unique,i.is_primary_key,i.is_disabled,i.filter_definition,c.name,ic.is_descending_key,ic.is_included_column FROM sys.indexes i JOIN sys.index_columns ic ON ic.object_id=i.object_id AND ic.index_id=i.index_id JOIN sys.columns c ON c.object_id=ic.object_id AND c.column_id=ic.column_id WHERE i.object_id={oid} AND i.index_id>0 ORDER BY i.is_primary_key DESC,i.is_unique DESC,i.name,ic.is_included_column,ic.key_ordinal,ic.index_column_id")).await?;
        let mut index_columns: Vec<(String, Vec<String>, Vec<String>)> = vec![];
        for r in rows {
            let name = required(&r, 0)?.to_owned();
            if !detail.indexes.iter().any(|i| i.name == name) {
                detail.indexes.push(IndexInfo {
                    name: name.clone(),
                    method: required(&r, 1)?.into(),
                    unique: number::<u8>(&r, 2)? != 0,
                    primary: number::<u8>(&r, 3)? != 0,
                    valid: number::<u8>(&r, 4)? == 0,
                    predicate: r[5].clone(),
                    keys: String::new(),
                });
                index_columns.push((name.clone(), vec![], vec![]));
            }
            let (_, keys, include) = index_columns.iter_mut().find(|(n, _, _)| *n == name).unwrap();
            let col = quote_ident(required(&r, 6)?);
            if number::<u8>(&r, 8)? != 0 {
                include.push(col);
            } else {
                keys.push(format!("{col}{}", if number::<u8>(&r, 7)? != 0 { " DESC" } else { "" }));
            }
        }
        for (index, (_, keys, include)) in detail.indexes.iter_mut().zip(index_columns) {
            index.keys = format!(
                "({}){}",
                keys.join(", "),
                if include.is_empty() { String::new() } else { format!(" INCLUDE ({})", include.join(", ")) }
            );
        }
        let rows = self.rows(&format!("SELECT t.name,t.is_instead_of_trigger,t.is_disabled,m.definition,e.type_desc FROM sys.triggers t LEFT JOIN sys.sql_modules m ON m.object_id=t.object_id LEFT JOIN sys.trigger_events e ON e.object_id=t.object_id WHERE t.parent_id={oid} ORDER BY t.name,e.type")).await?;
        for r in rows {
            let name = required(&r, 0)?.to_owned();
            if !detail.triggers.iter().any(|t| t.name == name) {
                detail.triggers.push(TriggerInfo {
                    name: name.clone(),
                    timing: if number::<u8>(&r, 1)? != 0 { TriggerTiming::InsteadOf } else { TriggerTiming::After },
                    events: vec![],
                    per_row: false,
                    enabled: number::<u8>(&r, 2)? == 0,
                    implementation: TriggerImplementation::SqlModule { definition: r[3].clone() },
                });
            }
            let event = match r[4].as_deref() {
                Some("INSERT") => Some(TriggerEvent::Insert),
                Some("UPDATE") => Some(TriggerEvent::Update),
                Some("DELETE") => Some(TriggerEvent::Delete),
                _ => None,
            };
            if let Some(event) = event {
                detail.triggers.iter_mut().find(|t| t.name == name).unwrap().events.push(event);
            }
        }
        Ok(Some(detail))
    }
}
