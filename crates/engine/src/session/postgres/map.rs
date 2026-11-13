use super::PgSession;
use crate::database_map::Neighborhood;
use crate::model::*;

impl PgSession {
    pub(in crate::session) async fn map_neighborhood(
        &self,
        oid: u32,
        after: u32,
    ) -> Result<Option<Neighborhood>, QueryError> {
        let root = self.client.query_opt(
            "SELECT c.oid,n.nspname,c.relname FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
             WHERE c.oid=$1 AND c.relkind IN ('r','p','f','v','m')
               AND (has_table_privilege(c.oid,'SELECT,INSERT,UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER') OR has_any_column_privilege(c.oid,'SELECT,INSERT,UPDATE,REFERENCES'))", &[&oid]
        ).await.map_err(|e| super::query_failure(e, None))?;
        let Some(root) = root else { return Ok(None) };
        let mut page = Neighborhood::new(MapRelation {
            oid: root.get(0),
            schema: root.get(1),
            name: root.get(2),
            columns: vec![],
        });
        let rows = self.client.query(
            "WITH incident AS (
                SELECT k.* FROM pg_constraint k
                WHERE k.contype='f' AND (k.conrelid=$1 OR k.confrelid=$1) AND k.oid>$2
                  AND NOT EXISTS (SELECT 1 FROM pg_constraint p WHERE p.oid=k.conparentid AND p.conrelid=k.conrelid)
                  AND (has_table_privilege(k.conrelid,'SELECT,INSERT,UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER') OR has_any_column_privilege(k.conrelid,'SELECT,INSERT,UPDATE,REFERENCES'))
                  AND (has_table_privilege(k.confrelid,'SELECT,INSERT,UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER') OR has_any_column_privilege(k.confrelid,'SELECT,INSERT,UPDATE,REFERENCES'))
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
             ORDER BY k.oid,pair.i", &[&oid,&after]
        ).await.map_err(|e| super::query_failure(e, None))?;
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
}
