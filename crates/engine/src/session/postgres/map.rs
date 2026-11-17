use super::PgSession;
use crate::database_map::Neighborhood;
use crate::model::*;
use crate::schema_map::{DeclaredKey, KEY_READ_LIMIT, PARTITION_READ_LIMIT, Partition, SchemaKeys};

fn visible(oid: &str) -> String {
    format!(
        "(has_table_privilege({oid},'SELECT,INSERT,UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER') OR has_any_column_privilege({oid},'SELECT,INSERT,UPDATE,REFERENCES'))"
    )
}

impl PgSession {
    pub(in crate::session) async fn map_neighborhood(
        &self,
        oid: u32,
        after: u32,
    ) -> Result<Option<Neighborhood>, QueryError> {
        let root = self
            .client
            .query_opt(
                &format!(
                    "SELECT c.oid,n.nspname,c.relname FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
             WHERE c.oid=$1 AND c.relkind IN ('r','p','f','v','m')
               AND {}",
                    visible("c.oid")
                ),
                &[&oid],
            )
            .await
            .map_err(|e| super::query_failure(e, None))?;
        let Some(root) = root else { return Ok(None) };
        let mut page = Neighborhood::new(MapRelation {
            oid: root.get(0),
            schema: root.get(1),
            name: root.get(2),
            columns: vec![],
        });
        let (source, target) = (visible("k.conrelid"), visible("k.confrelid"));
        let rows = self
            .client
            .query(
                &format!(
                    "WITH incident AS (
                SELECT k.* FROM pg_constraint k
                WHERE k.contype='f' AND (k.conrelid=$1 OR k.confrelid=$1) AND k.oid>$2
                  AND NOT EXISTS (SELECT 1 FROM pg_constraint p WHERE p.oid=k.conparentid AND p.conrelid=k.conrelid)
                  AND {source}
                  AND {target}
                ORDER BY k.oid LIMIT 33
             )
             SELECT k.oid,k.conname,k.conrelid,sn.nspname,sc.relname,k.confrelid,tn.nspname,tc.relname,
                    sa.attname,ta.attname,k.convalidated
             FROM incident k
             JOIN pg_class sc ON sc.oid=k.conrelid JOIN pg_namespace sn ON sn.oid=sc.relnamespace
             JOIN pg_class tc ON tc.oid=k.confrelid JOIN pg_namespace tn ON tn.oid=tc.relnamespace
             CROSS JOIN LATERAL unnest(k.conkey,k.confkey) WITH ORDINALITY AS pair(s,t,i)
             JOIN pg_attribute sa ON sa.attrelid=k.conrelid AND sa.attnum=pair.s
             JOIN pg_attribute ta ON ta.attrelid=k.confrelid AND ta.attnum=pair.t
             ORDER BY k.oid,pair.i"
                ),
                &[&oid, &after],
            )
            .await
            .map_err(|e| super::query_failure(e, None))?;
        for r in rows {
            page.pair(
                MapForeignKey {
                    id: r.get(0),
                    name: r.get(1),
                    source: r.get(2),
                    target: r.get(5),
                    columns: vec![],
                    enabled: true,
                    validated: r.get(10),
                },
                MapRelation { oid: r.get(2), schema: r.get(3), name: r.get(4), columns: vec![] },
                MapRelation { oid: r.get(5), schema: r.get(6), name: r.get(7), columns: vec![] },
                MapColumnPair { source: r.get(8), target: r.get(9) },
            );
        }
        Ok(Some(page))
    }

    /// PostgreSQL clones a partitioned table's foreign keys onto each partition, on either end; those clones are left out.
    pub(in crate::session) async fn schema_keys(&self, schema: &str) -> Result<SchemaKeys, QueryError> {
        let (source, target) = (visible("k.conrelid"), visible("k.confrelid"));
        let keys = format!(
            "WITH k AS (
                SELECT k.oid,k.conname,k.conrelid,k.confrelid,k.conkey,k.confkey,k.convalidated FROM pg_constraint k
                JOIN pg_class sc ON sc.oid=k.conrelid JOIN pg_namespace sn ON sn.oid=sc.relnamespace
                JOIN pg_class tc ON tc.oid=k.confrelid JOIN pg_namespace tn ON tn.oid=tc.relnamespace
                WHERE k.contype='f' AND (sn.nspname=$1 OR tn.nspname=$1)
                  AND NOT sc.relispartition AND NOT tc.relispartition
                  AND {source}
                  AND {target}
                ORDER BY k.oid LIMIT {}
             )
             SELECT k.oid,k.conname,k.conrelid,k.confrelid,sa.attname,ta.attname,k.convalidated
             FROM k
             CROSS JOIN LATERAL unnest(k.conkey,k.confkey) WITH ORDINALITY AS pair(s,t,i)
             JOIN pg_attribute sa ON sa.attrelid=k.conrelid AND sa.attnum=pair.s
             JOIN pg_attribute ta ON ta.attrelid=k.confrelid AND ta.attnum=pair.t
             ORDER BY k.oid,pair.i",
            KEY_READ_LIMIT
        );
        let partitions = format!(
            "SELECT i.inhrelid,i.inhparent FROM pg_inherits i
             JOIN pg_class c ON c.oid=i.inhrelid JOIN pg_namespace n ON n.oid=c.relnamespace
             JOIN pg_class p ON p.oid=i.inhparent JOIN pg_namespace pn ON pn.oid=p.relnamespace
             WHERE c.relispartition AND (n.nspname=$1 OR pn.nspname=$1)
             ORDER BY i.inhrelid LIMIT {PARTITION_READ_LIMIT}"
        );
        let (keys, partitions) = futures_util::future::try_join(
            self.client.query(&keys, &[&schema]),
            self.client.query(&partitions, &[&schema]),
        )
        .await
        .map_err(|e| super::query_failure(e, None))?;
        let mut read = SchemaKeys {
            keys: vec![],
            partitions: partitions.iter().map(|r| Partition { partition: r.get(0), parent: r.get(1) }).collect(),
        };
        for r in keys {
            read.pair(
                DeclaredKey {
                    id: r.get(0),
                    name: r.get(1),
                    source: r.get(2),
                    target: r.get(3),
                    columns: vec![],
                    enabled: true,
                    validated: r.get(6),
                },
                MapColumnPair { source: r.get(4), target: r.get(5) },
            );
        }
        Ok(read)
    }
}
