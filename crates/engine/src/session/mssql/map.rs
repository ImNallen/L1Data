use super::{MsSession, number, object_id, required};
use crate::model::*;
use crate::schema_map::{DeclaredKey, KEY_READ_LIMIT, SchemaKeys};
use crate::sql::tsql::quote_literal;

impl MsSession {
    pub(in crate::session) async fn schema_keys(&mut self, schema: &str) -> Result<SchemaKeys, QueryError> {
        let rows = self.rows(&format!(
            "WITH k AS (
                SELECT TOP ({limit}) fk.object_id,fk.name,fk.parent_object_id,fk.referenced_object_id,fk.is_disabled,fk.is_not_trusted
                FROM sys.foreign_keys fk
                JOIN sys.objects so ON so.object_id=fk.parent_object_id JOIN sys.schemas ss ON ss.schema_id=so.schema_id
                JOIN sys.objects t ON t.object_id=fk.referenced_object_id JOIN sys.schemas ts ON ts.schema_id=t.schema_id
                WHERE ss.name={schema} OR ts.name={schema}
                ORDER BY fk.object_id
             )
             SELECT k.object_id,k.name,k.parent_object_id,k.referenced_object_id,sc.name,tc.name,k.is_disabled,k.is_not_trusted
             FROM k
             JOIN sys.foreign_key_columns fc ON fc.constraint_object_id=k.object_id
             JOIN sys.columns sc ON sc.object_id=fc.parent_object_id AND sc.column_id=fc.parent_column_id
             JOIN sys.columns tc ON tc.object_id=fc.referenced_object_id AND tc.column_id=fc.referenced_column_id
             ORDER BY k.object_id,fc.constraint_column_id",
            limit = KEY_READ_LIMIT,
            schema = quote_literal(schema),
        )).await?;
        let mut read = SchemaKeys { keys: vec![], partitions: vec![] };
        for r in rows {
            read.pair(
                DeclaredKey {
                    id: object_id(&r, 0)?,
                    name: required(&r, 1)?.into(),
                    source: object_id(&r, 2)?,
                    target: object_id(&r, 3)?,
                    columns: vec![],
                    enabled: number::<u8>(&r, 6)? == 0,
                    validated: number::<u8>(&r, 7)? == 0,
                },
                MapColumnPair { source: required(&r, 4)?.into(), target: required(&r, 5)?.into() },
            );
        }
        Ok(read)
    }
}
