use super::{MsSession, native_object_id, number, required};
use crate::database_map::Neighborhood;
use crate::model::*;

impl MsSession {
    pub(in crate::session) async fn map_neighborhood(
        &mut self,
        oid: u32,
        after: u32,
    ) -> Result<Option<Neighborhood>, QueryError> {
        let root = self.rows(&format!("SELECT o.object_id,s.name,o.name FROM sys.objects o JOIN sys.schemas s ON s.schema_id=o.schema_id WHERE o.object_id={} AND o.type IN ('U','V')", native_object_id(oid))).await?;
        let Some(r) = root.first() else {
            return Ok(None);
        };
        let mut page = Neighborhood::new(MapRelation {
            oid: number::<i32>(r, 0)? as u32,
            schema: required(r, 1)?.into(),
            name: required(r, 2)?.into(),
            columns: vec![],
        });
        let rows = self
            .rows(&format!(
                "WITH incident AS (
                SELECT TOP (33) fk.* FROM sys.foreign_keys fk
                WHERE (fk.parent_object_id={oid} OR fk.referenced_object_id={oid}) AND fk.object_id>{after}
                ORDER BY fk.object_id
             )
             SELECT fk.object_id,fk.name,fk.parent_object_id,ss.name,so.name,fk.referenced_object_id,ts.name,t.name,
                    sc.name,tc.name,fk.is_disabled,fk.is_not_trusted
             FROM incident fk
             JOIN sys.objects so ON so.object_id=fk.parent_object_id JOIN sys.schemas ss ON ss.schema_id=so.schema_id
             JOIN sys.objects t ON t.object_id=fk.referenced_object_id JOIN sys.schemas ts ON ts.schema_id=t.schema_id
             JOIN sys.foreign_key_columns fc ON fc.constraint_object_id=fk.object_id
             JOIN sys.columns sc ON sc.object_id=fc.parent_object_id AND sc.column_id=fc.parent_column_id
             JOIN sys.columns tc ON tc.object_id=fc.referenced_object_id AND tc.column_id=fc.referenced_column_id
             ORDER BY fk.object_id,fc.constraint_column_id",
                oid = native_object_id(oid),
                after = native_object_id(after)
            ))
            .await?;
        for r in rows {
            let source = number::<i32>(&r, 2)? as u32;
            let target = number::<i32>(&r, 5)? as u32;
            page.pair(
                MapForeignKey {
                    id: number::<i32>(&r, 0)? as u32,
                    name: required(&r, 1)?.into(),
                    source,
                    target,
                    columns: vec![],
                    enabled: number::<u8>(&r, 10)? == 0,
                    validated: number::<u8>(&r, 11)? == 0,
                },
                MapRelation {
                    oid: source,
                    schema: required(&r, 3)?.into(),
                    name: required(&r, 4)?.into(),
                    columns: vec![],
                },
                MapRelation {
                    oid: target,
                    schema: required(&r, 6)?.into(),
                    name: required(&r, 7)?.into(),
                    columns: vec![],
                },
                MapColumnPair { source: required(&r, 8)?.into(), target: required(&r, 9)?.into() },
            );
        }
        Ok(Some(page))
    }
}
