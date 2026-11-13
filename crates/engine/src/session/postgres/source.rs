use std::collections::HashMap;

use tokio_postgres::types::ToSql;
use tokio_postgres::{Client, Row, Transaction};

use crate::model::{DetailError, ObjectScript, ScriptTarget};
use crate::script::{self, Change, Privilege, postgres as ddl};
use crate::text::quote_ident_if_needed as ident;

/// A relation and, for a partitioned table, every partition beneath it.
const RELATIONS: &str = "WITH RECURSIVE tree(oid) AS (
        SELECT $1::oid
        UNION ALL
        SELECT h.inhrelid FROM tree JOIN pg_inherits h ON h.inhparent = tree.oid JOIN pg_class hc ON hc.oid = h.inhrelid WHERE hc.relispartition
     )
     SELECT c.relkind::text, n.nspname, c.relname, c.relpersistence = 'u',
        coalesce(c.reloptions, '{}')::text[] || ARRAY(SELECT 'toast.' || o FROM pg_class t, unnest(t.reloptions) o WHERE t.oid = c.reltoastrelid),
        obj_description(c.oid, 'pg_class'), pg_get_partkeydef(c.oid), pg_get_expr(c.relpartbound, c.oid), pn.nspname, p.relname, c.relispopulated,
        c.relrowsecurity, c.relforcerowsecurity, c.relreplident::text, ts.spcname, CASE WHEN am.amname <> 'heap' THEN am.amname END,
        EXISTS (SELECT 1 FROM pg_seclabel l WHERE l.classoid = 'pg_class'::regclass AND l.objoid = c.oid),
        fs.srvname, coalesce(ft.ftoptions, '{}'),
        ARRAY(SELECT hn.nspname::text FROM pg_inherits h JOIN pg_class hc ON hc.oid = h.inhparent JOIN pg_namespace hn ON hn.oid = hc.relnamespace
              WHERE h.inhrelid = c.oid AND NOT c.relispartition ORDER BY h.inhseqno),
        ARRAY(SELECT hc.relname::text FROM pg_inherits h JOIN pg_class hc ON hc.oid = h.inhparent
              WHERE h.inhrelid = c.oid AND NOT c.relispartition ORDER BY h.inhseqno),
        CASE WHEN c.reloftype <> 0 THEN format_type(c.reloftype, NULL) END,
        ARRAY(SELECT p.pubname::text FROM pg_publication_rel pr JOIN pg_publication p ON p.oid = pr.prpubid WHERE pr.prrelid = c.oid ORDER BY 1),
        pg_get_userbyid(c.relowner)::text, i.inhparent,
        ARRAY(SELECT a.attname::text FROM pg_attribute a WHERE a.attrelid = c.oid AND a.attnum > 0
              AND EXISTS (SELECT 1 FROM pg_seclabel l WHERE l.classoid = 'pg_class'::regclass AND l.objoid = c.oid AND l.objsubid = a.attnum) ORDER BY a.attnum),
        ARRAY(SELECT sn.nspname::text FROM pg_depend dep JOIN pg_class sc ON sc.oid = dep.objid AND sc.relkind = 'S' JOIN pg_namespace sn ON sn.oid = sc.relnamespace
              WHERE dep.classid = 'pg_class'::regclass AND dep.refclassid = 'pg_class'::regclass AND dep.refobjid = c.oid AND dep.deptype IN ('a', 'i')
                AND EXISTS (SELECT 1 FROM pg_seclabel l WHERE l.classoid = 'pg_class'::regclass AND l.objoid = sc.oid) ORDER BY sc.relname),
        ARRAY(SELECT sc.relname::text FROM pg_depend dep JOIN pg_class sc ON sc.oid = dep.objid AND sc.relkind = 'S'
              WHERE dep.classid = 'pg_class'::regclass AND dep.refclassid = 'pg_class'::regclass AND dep.refobjid = c.oid AND dep.deptype IN ('a', 'i')
                AND EXISTS (SELECT 1 FROM pg_seclabel l WHERE l.classoid = 'pg_class'::regclass AND l.objoid = sc.oid) ORDER BY sc.relname),
        {schema_publications},
        c.oid
     FROM tree JOIN pg_class c ON c.oid = tree.oid JOIN pg_namespace n ON n.oid = c.relnamespace
     LEFT JOIN pg_inherits i ON i.inhrelid = c.oid AND c.relispartition
     LEFT JOIN pg_class p ON p.oid = i.inhparent LEFT JOIN pg_namespace pn ON pn.oid = p.relnamespace
     LEFT JOIN pg_tablespace ts ON ts.oid = c.reltablespace
     LEFT JOIN pg_am am ON am.oid = c.relam
     LEFT JOIN pg_foreign_table ft ON ft.ftrelid = c.oid LEFT JOIN pg_foreign_server fs ON fs.oid = ft.ftserver";

struct Queries {
    relations: String,
    columns: String,
    statistics: String,
    sequence_states: String,
}

impl Queries {
    fn for_version(version: i32) -> Queries {
        let compression =
            |alias: &str| if version >= 140000 { format!("{alias}.attcompression::text") } else { "''".into() };
        let target = if version >= 130000 { "NULLIF(s.stxstattarget, -1)::int4" } else { "NULL::int4" };
        // PostgreSQL 15 added unlogged sequences, and a table's identity and serial sequences take its persistence.
        let unlogged = if version >= 150000 {
            "CASE WHEN sc.relpersistence = 'u' OR t.relpersistence = 'u' THEN sc.relpersistence = 'u' END"
        } else {
            "NULL::bool"
        };
        // PostgreSQL 15 added publications of every table in a schema.
        let schema_publications = if version >= 150000 {
            "ARRAY(SELECT p.pubname::text FROM pg_publication_namespace pn JOIN pg_publication p ON p.oid = pn.pnpubid WHERE pn.pnnspid = c.relnamespace AND c.relkind IN ('r', 'p') ORDER BY 1)"
        } else {
            "'{}'::text[]"
        };
        // Recent minor releases stamp every column with pg_attver, which the server sets and a script cannot.
        let options = "ARRAY(SELECT o FROM unnest(a.attoptions) o WHERE split_part(o, '=', 1) <> 'pg_attver')";
        Queries {
            relations: RELATIONS.replace("{schema_publications}", schema_publications),
            columns: format!(
                "SELECT a.attname, coalesce(serial.name, format_type(a.atttypid, a.atttypmod)), a.attnotnull, a.attidentity::text, a.attgenerated::text,
        CASE WHEN serial.name IS NULL THEN pg_get_expr(d.adbin, d.adrelid) END,
        CASE WHEN a.attcollation <> t.typcollation THEN cn.nspname END, CASE WHEN a.attcollation <> t.typcollation THEN co.collname END,
        col_description(a.attrelid, a.attnum),
        ident.nspname, ident.relname, format_type(ident.seqtypid, NULL), ident.seqstart, ident.seqincrement, ident.seqmin, ident.seqmax, ident.seqcache, ident.seqcycle,
        a.attislocal, NULLIF(a.attstattarget, -1)::int4, a.attstorage::text, t.typstorage::text, {compression}, {options}, coalesce(a.attfdwoptions, '{{}}'),
        pa.position, pa.def, pa.attnotnull, pa.attstorage::text, pa.attcompression, serial.seq, a.attrelid
     FROM pg_attribute a
     JOIN pg_class c ON c.oid = a.attrelid
     JOIN pg_type t ON t.oid = a.atttypid
     LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum
     LEFT JOIN pg_collation co ON co.oid = a.attcollation
     LEFT JOIN pg_namespace cn ON cn.oid = co.collnamespace
     LEFT JOIN LATERAL (
        SELECT sn.nspname, sc.relname, s.* FROM pg_depend dep JOIN pg_class sc ON sc.oid = dep.objid AND sc.relkind = 'S'
        JOIN pg_namespace sn ON sn.oid = sc.relnamespace JOIN pg_sequence s ON s.seqrelid = sc.oid
        WHERE a.attidentity <> '' AND dep.classid = 'pg_class'::regclass AND dep.refclassid = 'pg_class'::regclass
          AND dep.refobjid = a.attrelid AND dep.refobjsubid = a.attnum AND dep.deptype = 'i'
     ) ident ON true
     LEFT JOIN LATERAL (
        SELECT CASE a.atttypid WHEN 'int2'::regtype THEN 'smallserial' WHEN 'int4'::regtype THEN 'serial' WHEN 'int8'::regtype THEN 'bigserial' END AS name, sc.relname AS seq
        FROM pg_depend dep JOIN pg_class sc ON sc.oid = dep.objid AND sc.relkind = 'S' JOIN pg_sequence s ON s.seqrelid = sc.oid
        WHERE a.attidentity = '' AND a.attnotnull AND a.atttypid IN ('int2'::regtype, 'int4'::regtype, 'int8'::regtype)
          AND dep.classid = 'pg_class'::regclass AND dep.refclassid = 'pg_class'::regclass
          AND dep.refobjid = a.attrelid AND dep.refobjsubid = a.attnum AND dep.deptype = 'a'
          AND pg_get_expr(d.adbin, d.adrelid) = 'nextval(' || quote_literal(sc.oid::regclass::text) || '::regclass)'
          AND sc.relnamespace = c.relnamespace AND sc.relname = c.relname || '_' || a.attname || '_seq'
          AND s.seqtypid = a.atttypid AND s.seqstart = 1 AND s.seqincrement = 1 AND s.seqmin = 1 AND s.seqcache = 1 AND NOT s.seqcycle
          AND s.seqmax = CASE a.atttypid WHEN 'int2'::regtype THEN 32767 WHEN 'int4'::regtype THEN 2147483647 ELSE 9223372036854775807 END
          AND NOT EXISTS (SELECT 1 FROM pg_constraint nn WHERE nn.conrelid = a.attrelid AND nn.contype = 'n' AND nn.conkey[1] = a.attnum AND (nn.connoinherit OR NOT nn.convalidated))
     ) serial ON true
     LEFT JOIN LATERAL (
        SELECT (h.inhseqno::int8 << 16) + pa.attnum AS position, pg_get_expr(pd.adbin, pd.adrelid) AS def,
            EXISTS (
                SELECT 1 FROM pg_inherits nh JOIN pg_attribute nn ON nn.attrelid = nh.inhparent AND nn.attname = a.attname AND nn.attnotnull AND NOT nn.attisdropped
                WHERE nh.inhrelid = a.attrelid AND NOT EXISTS (SELECT 1 FROM pg_constraint nk WHERE nk.conrelid = nn.attrelid AND nk.contype = 'n' AND nk.conkey[1] = nn.attnum AND nk.connoinherit)
            ) AS attnotnull,
            pa.attstorage, {parent_compression} AS attcompression
        FROM pg_inherits h JOIN pg_attribute pa ON pa.attrelid = h.inhparent AND pa.attname = a.attname AND NOT pa.attisdropped
        LEFT JOIN pg_attrdef pd ON pd.adrelid = pa.attrelid AND pd.adnum = pa.attnum
        WHERE h.inhrelid = a.attrelid AND a.attinhcount > 0 ORDER BY h.inhseqno LIMIT 1
     ) pa ON true
     WHERE a.attrelid = ANY ($1) AND a.attnum > 0 AND NOT a.attisdropped
     ORDER BY a.attnum",
                compression = compression("a"),
                parent_compression = compression("pa"),
            ),
            statistics: format!(
                "SELECT n.nspname, s.stxname, pg_get_statisticsobjdef(s.oid), {target}, obj_description(s.oid, 'pg_statistic_ext'), s.stxrelid
                 FROM pg_statistic_ext s JOIN pg_namespace n ON n.oid = s.stxnamespace WHERE s.stxrelid = ANY ($1) ORDER BY s.stxname"
            ),
            sequence_states: format!(
                "SELECT * FROM (
                    SELECT sn.nspname, sc.relname, {unlogged} AS unlogged, obj_description(sc.oid, 'pg_class') AS comment, dep.refobjid
                    FROM pg_depend dep JOIN pg_class sc ON sc.oid = dep.objid AND sc.relkind = 'S' JOIN pg_namespace sn ON sn.oid = sc.relnamespace
                    JOIN pg_class t ON t.oid = dep.refobjid
                    WHERE dep.classid = 'pg_class'::regclass AND dep.refclassid = 'pg_class'::regclass AND dep.refobjid = ANY ($1) AND dep.deptype IN ('a', 'i')
                 ) s WHERE s.unlogged IS NOT NULL OR s.comment IS NOT NULL ORDER BY s.relname"
            ),
        }
    }
}

const OWNED_SEQUENCES: &str = "SELECT sn.nspname, sc.relname, a.attname, format_type(s.seqtypid, NULL), s.seqstart, s.seqincrement, s.seqmin, s.seqmax, s.seqcache, s.seqcycle, dep.refobjid
     FROM pg_depend dep JOIN pg_class sc ON sc.oid = dep.objid AND sc.relkind = 'S'
     JOIN pg_namespace sn ON sn.oid = sc.relnamespace JOIN pg_sequence s ON s.seqrelid = sc.oid
     JOIN pg_attribute a ON a.attrelid = dep.refobjid AND a.attnum = dep.refobjsubid
     WHERE dep.classid = 'pg_class'::regclass AND dep.refclassid = 'pg_class'::regclass AND dep.refobjid = ANY ($1) AND dep.deptype = 'a'
     ORDER BY sc.relname";

/// A partition's copies of its parent's constraints, an inheritance child's inherited checks, and a
/// foreign key's clones for the partitions it references come with the parent, so only their
/// comments are read. `pg_get_constraintdef` prints an exclusion constraint's storage parameters
/// but no constraint's tablespace.
///
/// PostgreSQL 18 stores `NOT NULL` as constraints. The column's own `NOT NULL` replays a local one
/// with the default name and nothing else to say, so that one is not read. A replay gives an
/// inherited one the parent's name and validates it, so `lost` marks one it cannot reproduce.
const CONSTRAINTS: &str = "SELECT k.conname, pg_get_constraintdef(k.oid), obj_description(k.oid, 'pg_constraint'),
        k.contype = 'f' AND k.confrelid = k.conrelid
          AND NOT EXISTS (SELECT 1 FROM pg_constraint u WHERE u.conindid = k.conindid AND u.conrelid = k.conrelid AND u.contype IN ('p', 'u')),
        CASE WHEN k.contype IN ('p', 'u') THEN coalesce(ic.reloptions, '{}') ELSE '{}' END, ts.spcname,
        NOT (k.conislocal AND k.conparentid = 0),
        k.contype = 'f' AND k.confrelid <> k.conrelid AND k.confrelid = ANY ($1),
        nn.attname, not_null.lost,
        k.conrelid
     FROM pg_constraint k
     JOIN pg_class rel ON rel.oid = k.conrelid
     LEFT JOIN pg_class ic ON ic.oid = k.conindid AND k.contype IN ('p', 'u', 'x') LEFT JOIN pg_tablespace ts ON ts.oid = ic.reltablespace
     LEFT JOIN pg_attribute nn ON k.contype = 'n' AND nn.attrelid = k.conrelid AND nn.attnum = k.conkey[1]
     CROSS JOIN LATERAL (
        SELECT k.contype = 'n' AND k.coninhcount > 0 AND (NOT k.convalidated OR NOT k.conislocal AND NOT EXISTS (
            SELECT 1 FROM pg_inherits h JOIN pg_constraint p ON p.conrelid = h.inhparent AND p.contype = 'n' AND p.conname = k.conname WHERE h.inhrelid = k.conrelid
        )) AS lost
     ) not_null
     WHERE k.conrelid = ANY ($1) AND k.contype IN ('p', 'u', 'f', 'c', 'x', 'n')
       AND (k.contype <> 'n' AND k.conislocal AND k.conparentid = 0
            OR obj_description(k.oid, 'pg_constraint') IS NOT NULL
            OR not_null.lost
            OR k.contype = 'n' AND k.conislocal AND (k.coninhcount > 0 OR NOT k.convalidated OR k.connoinherit OR k.conname <> rel.relname || '_' || nn.attname || '_not_null'))
     ORDER BY position(k.contype IN 'pufcxn'), k.conname";

/// A partition's copy of its parent's index comes with the parent.
const INDEXES: &str = "SELECT ic.relname,
        CASE WHEN NOT EXISTS (SELECT 1 FROM pg_constraint k WHERE k.conindid = i.indexrelid AND k.conrelid = i.indrelid AND k.contype IN ('p', 'u', 'x'))
             THEN pg_get_indexdef(i.indexrelid) END,
        obj_description(i.indexrelid, 'pg_class'), i.indisclustered, i.indisreplident, ts.spcname,
        ARRAY(SELECT a.attnum::int4 FROM pg_attribute a WHERE a.attrelid = i.indexrelid AND NULLIF(a.attstattarget, -1) IS NOT NULL ORDER BY a.attnum),
        ARRAY(SELECT NULLIF(a.attstattarget, -1)::int4 FROM pg_attribute a WHERE a.attrelid = i.indexrelid AND NULLIF(a.attstattarget, -1) IS NOT NULL ORDER BY a.attnum),
        NOT i.indisvalid, ic.relkind = 'I', i.indrelid
     FROM pg_index i JOIN pg_class ic ON ic.oid = i.indexrelid LEFT JOIN pg_tablespace ts ON ts.oid = ic.reltablespace
     WHERE i.indrelid = ANY ($1) AND NOT EXISTS (SELECT 1 FROM pg_inherits h WHERE h.inhrelid = i.indexrelid)
     ORDER BY ic.relname";

/// A partition's clone of its parent's trigger comes with the parent, so only a clone whose
/// state or comment the replay would not reproduce is read, to set them again.
const TRIGGERS: &str = "SELECT t.tgname, pg_get_triggerdef(t.oid), t.tgenabled::text, obj_description(t.oid, 'pg_trigger'), p.oid IS NOT NULL, t.tgrelid
     FROM pg_trigger t
     LEFT JOIN pg_depend d ON d.classid = 'pg_trigger'::regclass AND d.objid = t.oid AND d.refclassid = 'pg_trigger'::regclass AND d.deptype = 'P'
     LEFT JOIN pg_trigger p ON p.oid = d.refobjid
     WHERE t.tgrelid = ANY ($1)
       AND (p.oid IS NULL AND NOT t.tgisinternal
            OR p.oid IS NOT NULL AND (t.tgenabled <> 'O' OR t.tgenabled <> p.tgenabled OR obj_description(t.oid, 'pg_trigger') IS NOT NULL))
     ORDER BY t.tgname";

/// A partition's copy of its parent's index comes with the parent under a name PostgreSQL chooses,
/// which depends on the names the schema already holds, so every copy's name is restored. The copy
/// takes its parent index's storage parameters and tablespace, an unset tablespace being the
/// database's.
const ATTACHED_INDEXES: &str = "SELECT ic.relname, h.inhparent::regclass::text, i.indisclustered, i.indisreplident, obj_description(i.indexrelid, 'pg_class'),
        coalesce(ic.reloptions, '{}'), coalesce(pc.reloptions, '{}'),
        CASE WHEN ic.reltablespace <> pc.reltablespace
             THEN coalesce(ts.spcname, (SELECT dt.spcname FROM pg_database d JOIN pg_tablespace dt ON dt.oid = d.dattablespace WHERE d.datname = current_database())) END,
        i.indrelid
     FROM pg_index i JOIN pg_class ic ON ic.oid = i.indexrelid JOIN pg_inherits h ON h.inhrelid = i.indexrelid JOIN pg_class pc ON pc.oid = h.inhparent
     LEFT JOIN pg_tablespace ts ON ts.oid = ic.reltablespace
     WHERE i.indrelid = ANY ($1)
     ORDER BY ic.relname";

const RULES: &str = "SELECT r.rulename, pg_get_ruledef(r.oid), r.ev_enabled::text, obj_description(r.oid, 'pg_rewrite'), r.ev_class FROM pg_rewrite r
     WHERE r.ev_class = ANY ($1) AND r.rulename <> '_RETURN' ORDER BY r.rulename";

const POLICIES: &str = "SELECT p.polname, p.polpermissive, p.polcmd::text,
        ARRAY(SELECT r.rolname::text FROM pg_roles r WHERE r.oid = ANY (p.polroles) ORDER BY r.rolname),
        pg_get_expr(p.polqual, p.polrelid), pg_get_expr(p.polwithcheck, p.polrelid), obj_description(p.oid, 'pg_policy'), p.polrelid
     FROM pg_policy p WHERE p.polrelid = ANY ($1) ORDER BY p.polname";

const ROUTINE: &str = "SELECT p.prokind::text, p.proname, CASE WHEN p.prokind <> 'a' THEN pg_get_functiondef(p.oid) END,
        obj_description(p.oid, 'pg_proc'), n.nspname, pg_get_function_identity_arguments(p.oid), pg_get_userbyid(p.proowner)::text, p.prosecdef,
        ARRAY(SELECT e.extname::text FROM pg_depend d JOIN pg_extension e ON e.oid = d.refobjid
              WHERE d.classid = 'pg_proc'::regclass AND d.objid = p.oid AND d.refclassid = 'pg_extension'::regclass AND d.deptype = 'x' ORDER BY 1),
        EXISTS (SELECT 1 FROM pg_seclabel l WHERE l.classoid = 'pg_proc'::regclass AND l.objoid = p.oid)
     FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace WHERE p.oid = $1";

fn acl_changes(from: &str, acl: &str, default: &str, filter: &str, keys: &str) -> String {
    let side = |exploded: &str, granted: bool| {
        format!(
            "SELECT {keys}, a.grantee, a.privilege_type, a.is_grantable, {granted} FROM {from} CROSS JOIN LATERAL aclexplode({exploded}) a WHERE {filter} AND {acl} IS NOT NULL"
        )
    };
    format!(
        "({} EXCEPT {}) UNION ALL ({} EXCEPT {})",
        side(acl, true),
        side(default, true),
        side(default, false),
        side(acl, false)
    )
}

fn grants_query(changes: &[String]) -> String {
    format!(
        "SELECT k.s, k.n, k.col, k.privilege_type, CASE WHEN k.grantee = 0 THEN NULL ELSE pg_get_userbyid(k.grantee)::text END, k.is_grantable, k.granted, k.rel
         FROM ({}) k(rel, s, n, col, grantee, privilege_type, is_grantable, granted)
         ORDER BY 1 NULLS FIRST, 2 NULLS FIRST, 3 NULLS FIRST, 5 NULLS FIRST, 4, 7",
        changes.join(" UNION ALL ")
    )
}

fn relation_grants_query() -> String {
    grants_query(&[
        acl_changes(
            "pg_class c",
            "c.relacl",
            "acldefault('r', c.relowner)",
            "c.oid = ANY ($1)",
            "c.oid, NULL::text, NULL::text, NULL::text",
        ),
        acl_changes(
            "pg_attribute at JOIN pg_class ac ON ac.oid = at.attrelid",
            "at.attacl",
            "acldefault('c', ac.relowner)",
            "at.attrelid = ANY ($1) AND at.attnum > 0 AND NOT at.attisdropped",
            "at.attrelid, NULL::text, NULL::text, at.attname::text",
        ),
        acl_changes(
            "pg_depend dep JOIN pg_class sc ON sc.oid = dep.objid AND sc.relkind = 'S' JOIN pg_namespace sn ON sn.oid = sc.relnamespace",
            "sc.relacl",
            "acldefault('s', sc.relowner)",
            "dep.classid = 'pg_class'::regclass AND dep.refclassid = 'pg_class'::regclass AND dep.refobjid = ANY ($1) AND dep.deptype IN ('a', 'i')",
            "dep.refobjid, sn.nspname::text, sc.relname::text, NULL::text",
        ),
    ])
}

fn routine_grants_query() -> String {
    grants_query(&[acl_changes(
        "pg_proc p",
        "p.proacl",
        "acldefault('f', p.proowner)",
        "p.oid = $1",
        "p.oid, NULL::text, NULL::text, NULL::text",
    )])
}

fn grant_row(r: &Row) -> Privilege {
    let change = match (r.get::<_, bool>(6), r.get::<_, bool>(5)) {
        (false, _) => Change::Revoked,
        (true, true) => Change::GrantedWithGrantOption,
        (true, false) => Change::Granted,
    };
    Privilege {
        name: r.get(3),
        column: r.get::<_, Option<&str>>(2).map(ident),
        grantee: r.get::<_, Option<&str>>(4).map_or("PUBLIC".into(), ident),
        change,
    }
}

fn owner(name: String, role: &str) -> ddl::Owner {
    ddl::Owner { connecting_role: name == role, name }
}

/// The relation itself, its columns, and the sequences it owns that carry a security label.
fn labeled(r: &Row) -> Vec<String> {
    let relation = ddl::qualified(r.get(1), r.get(2));
    let (columns, schemas, sequences): (Vec<String>, Vec<String>, Vec<String>) = (r.get(25), r.get(26), r.get(27));
    let itself = r.get::<_, bool>(16).then(|| relation.clone());
    let columns = columns.iter().map(|c| format!("{relation}.{}", ident(c)));
    itself.into_iter().chain(columns).chain(schemas.iter().zip(&sequences).map(|(s, n)| ddl::qualified(s, n))).collect()
}

fn unscripted(r: &Row, grants: &[Row], role: &str) -> ddl::Unscripted {
    let mut u = ddl::Unscripted {
        security_labels: labeled(r),
        publications: r.get(22),
        schema_publications: r.get(28),
        owner: owner(r.get(23), role),
        ..Default::default()
    };
    for g in grants {
        match (g.get::<_, Option<String>>(0), g.get::<_, Option<String>>(1)) {
            (Some(schema), Some(name)) => match u.sequence_privileges.last_mut() {
                Some(s) if s.schema == schema && s.name == name => s.privileges.push(grant_row(g)),
                _ => {
                    u.sequence_privileges.push(ddl::SequencePrivileges { schema, name, privileges: vec![grant_row(g)] })
                }
            },
            _ => u.privileges.push(grant_row(g)),
        }
    }
    u
}

/// With `search_path` only `pg_catalog`, the server qualifies every name it prints in an
/// expression or a view, index, constraint, or trigger definition. Routine bodies stay as written.
pub async fn load(client: &mut Client, target: &ScriptTarget) -> Result<ObjectScript, DetailError> {
    let tx = client.build_transaction().read_only(true).start().await.map_err(failed)?;
    let session = tx.query_one("SELECT current_setting('server_version_num')::int4, current_user::text, set_config('search_path', 'pg_catalog', true)", &[]).await.map_err(failed)?;
    let (queries, role) = (Queries::for_version(session.get(0)), session.get::<_, String>(1));
    let script = match target {
        ScriptTarget::Relation { oid } => relation(&tx, &queries, &role, *oid).await,
        ScriptTarget::Routine { oid } => routine(&tx, &role, *oid).await,
        ScriptTarget::Index { relation_oid, name } => index(&tx, *relation_oid, name).await,
        ScriptTarget::Trigger { relation_oid, name } => trigger(&tx, *relation_oid, name).await,
        ScriptTarget::Sequence { oid } => sequence_script(&tx, &role, *oid).await,
        ScriptTarget::Type { oid } => type_script(&tx, &role, *oid).await,
        ScriptTarget::Extension { oid } => extension_script(&tx, &role, *oid).await,
        ScriptTarget::Synonym { .. } => Err(refused("PostgreSQL has no synonyms.")),
    };
    tx.rollback().await.map_err(failed)?;
    script
}

fn failed(e: tokio_postgres::Error) -> DetailError {
    DetailError::Failed { message: super::query_failure(e, None).message }
}

fn refused(message: &str) -> DetailError {
    DetailError::Failed { message: message.into() }
}

const SEQUENCE_ROW: &str = "SELECT n.nspname, c.relname, c.relpersistence = 'u', format_type(s.seqtypid, NULL),
        s.seqstart, s.seqincrement, s.seqmin, s.seqmax, s.seqcache, s.seqcycle,
        obj_description(c.oid, 'pg_class'), owned.nspname, owned.relname, owned.attname,
        pg_get_userbyid(c.relowner)::text,
        EXISTS (SELECT 1 FROM pg_seclabel l WHERE l.classoid = 'pg_class'::regclass AND l.objoid = c.oid)
     FROM pg_class c
     JOIN pg_namespace n ON n.oid = c.relnamespace
     JOIN pg_sequence s ON s.seqrelid = c.oid
     LEFT JOIN LATERAL (
         SELECT onsp.nspname, oc.relname, a.attname
         FROM pg_depend d
         JOIN pg_class oc ON oc.oid = d.refobjid
         JOIN pg_namespace onsp ON onsp.oid = oc.relnamespace
         JOIN pg_attribute a ON a.attrelid = d.refobjid AND a.attnum = d.refobjsubid AND NOT a.attisdropped
         WHERE d.classid = 'pg_class'::regclass AND d.objid = c.oid
           AND d.refclassid = 'pg_class'::regclass AND d.deptype IN ('a', 'i')
         LIMIT 1
     ) owned ON true
     WHERE c.oid = $1 AND c.relkind = 'S'";

const TYPE_HEAD: &str = "SELECT t.typtype::text, n.nspname, t.typname, pg_get_userbyid(t.typowner)::text,
        obj_description(t.oid, 'pg_type'),
        EXISTS (SELECT 1 FROM pg_seclabel l WHERE l.classoid = 'pg_type'::regclass AND l.objoid = t.oid)
     FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace WHERE t.oid = $1";

fn sequence_grants() -> String {
    grants_query(&[acl_changes(
        "pg_class c",
        "c.relacl",
        "acldefault('s', c.relowner)",
        "c.oid = $1",
        "c.oid, NULL::text, NULL::text, NULL::text",
    )])
}

fn type_grants() -> String {
    grants_query(&[acl_changes(
        "pg_type t",
        "t.typacl",
        "acldefault('T', t.typowner)",
        "t.oid = $1",
        "t.oid, NULL::text, NULL::text, NULL::text",
    )])
}

fn privilege_rows(rows: &[Row]) -> Vec<Privilege> {
    rows.iter().map(grant_row).collect()
}

async fn sequence_script(tx: &Transaction<'_>, role: &str, oid: u32) -> Result<ObjectScript, DetailError> {
    let params: &[&(dyn ToSql + Sync)] = &[&oid];
    let grants_sql = sequence_grants();
    let (row, grants) =
        tokio::try_join!(tx.query_opt(SEQUENCE_ROW, params), tx.query(&grants_sql, params)).map_err(failed)?;
    let row = row.ok_or(DetailError::Gone)?;
    let name: String = row.get(1);
    let type_name = row.get::<_, Option<String>>(3).ok_or_else(|| refused("The sequence type could not be read."))?;
    let (owned_schema, owned_table, owned_column) =
        match (row.get::<_, Option<String>>(11), row.get::<_, Option<String>>(12), row.get::<_, Option<String>>(13)) {
            (Some(schema), Some(table), Some(column)) => (schema, table, column),
            _ => (String::new(), String::new(), String::new()),
        };
    let ddl = ddl::standalone_sequence(&ddl::StandaloneSequence {
        sequence: ddl::Sequence {
            schema: row.get(0),
            name: name.clone(),
            column: String::new(),
            type_name,
            start: row.get(4),
            increment: row.get(5),
            min: row.get(6),
            max: row.get(7),
            cache: row.get(8),
            cycle: row.get(9),
        },
        unlogged: row.get(2),
        owned_schema,
        owned_table,
        owned_column,
        comment: row.get(10),
        privileges: privilege_rows(&grants),
        owner: owner(row.get(14), role),
        security_label: row.get(15),
    });
    Ok(script::object_script(&name, ddl))
}

async fn type_script(tx: &Transaction<'_>, role: &str, oid: u32) -> Result<ObjectScript, DetailError> {
    let params: &[&(dyn ToSql + Sync)] = &[&oid];
    let grants_sql = type_grants();
    let (head, grants) =
        tokio::try_join!(tx.query_opt(TYPE_HEAD, params), tx.query(&grants_sql, params)).map_err(failed)?;
    let head = head.ok_or(DetailError::Gone)?;
    let kind: String = head.get(0);
    if matches!(kind.as_str(), "b" | "p") {
        return Err(refused("Built-in types cannot be scripted."));
    }
    let object = ddl::ScriptedObject {
        schema: head.get(1),
        name: head.get(2),
        comment: head.get(4),
        privileges: privilege_rows(&grants),
        owner: owner(head.get(3), role),
        security_label: head.get(5),
    };
    let name = object.name.clone();
    let ddl = match kind.as_str() {
        "e" => {
            let labels = tx
                .query("SELECT e.enumlabel FROM pg_enum e WHERE e.enumtypid = $1 ORDER BY e.enumsortorder", params)
                .await
                .map_err(failed)?;
            ddl::enum_type(&object, &labels.iter().map(|r| r.get(0)).collect::<Vec<String>>())
        }
        "d" => {
            let row = tx
                .query_opt(
                    "SELECT format_type(t.typbasetype, t.typtypmod),
                            CASE WHEN t.typcollation <> b.typcollation THEN c.collname END,
                            pg_get_expr(t.typdefaultbin, 0), t.typnotnull
                     FROM pg_type t JOIN pg_type b ON b.oid = t.typbasetype
                     LEFT JOIN pg_collation c ON c.oid = t.typcollation WHERE t.oid = $1",
                    params,
                )
                .await
                .map_err(failed)?
                .ok_or(DetailError::Gone)?;
            let checks = tx.query("SELECT con.conname, pg_get_constraintdef(con.oid) FROM pg_constraint con WHERE con.contypid = $1 ORDER BY con.conname", params).await.map_err(failed)?;
            let checks =
                checks
                    .iter()
                    .filter_map(|r| Some((r.get(0), r.get::<_, Option<String>>(1)?)))
                    .collect::<Vec<(String, String)>>();
            ddl::domain_type(
                &object,
                &row.get::<_, String>(0),
                row.get::<_, Option<String>>(1).as_deref(),
                row.get::<_, Option<String>>(2).as_deref(),
                row.get(3),
                &checks,
            )
        }
        "c" => {
            let fields = tx
                .query(
                    "SELECT a.attname, format_type(a.atttypid, a.atttypmod),
                            CASE WHEN a.attcollation <> t.typcollation AND a.attcollation <> 0 THEN c.collname END
                     FROM pg_attribute a JOIN pg_type t ON t.oid = a.atttypid
                     LEFT JOIN pg_collation c ON c.oid = a.attcollation
                     WHERE a.attrelid = (SELECT typrelid FROM pg_type WHERE oid = $1) AND a.attnum > 0 AND NOT a.attisdropped
                     ORDER BY a.attnum",
                    params,
                )
                .await
                .map_err(failed)?;
            let fields = fields
                .iter()
                .map(|r| ddl::CompositeField { name: r.get(0), type_name: r.get(1), collation: r.get(2) })
                .collect::<Vec<_>>();
            ddl::composite_type(&object, &fields)
        }
        "r" => {
            let row = tx
                .query_opt(
                    "SELECT format_type(r.rngsubtype, NULL),
                            CASE WHEN opc.opcdefault IS NOT FALSE THEN NULL ELSE opn.nspname END,
                            CASE WHEN opc.opcdefault IS NOT FALSE THEN NULL ELSE opc.opcname END,
                            CASE WHEN r.rngcollation <> 0 THEN coll.collname END,
                            CASE WHEN r.rngcanonical <> 0 THEN r.rngcanonical::regproc::text END,
                            CASE WHEN r.rngsubdiff <> 0 THEN r.rngsubdiff::regproc::text END
                     FROM pg_range r
                     LEFT JOIN pg_opclass opc ON opc.oid = r.rngsubopc
                     LEFT JOIN pg_namespace opn ON opn.oid = opc.opcnamespace
                     LEFT JOIN pg_collation coll ON coll.oid = r.rngcollation
                     WHERE r.rngtypid = $1",
                    params,
                )
                .await
                .map_err(failed)?
                .ok_or(DetailError::Gone)?;
            let opclass = match (row.get::<_, Option<String>>(1), row.get::<_, Option<String>>(2)) {
                (Some(schema), Some(name)) => Some(ddl::qualified(&schema, &name)),
                _ => None,
            };
            ddl::range_type(
                &object,
                &ddl::RangeScript {
                    subtype: row.get::<_, Option<String>>(0).unwrap_or_default(),
                    opclass,
                    collation: row.get(3),
                    canonical: row.get(4),
                    subtype_diff: row.get(5),
                },
            )
        }
        _ => {
            return Err(refused("Only enums, domains, composites, and ranges can be scripted."));
        }
    };
    Ok(script::object_script(&name, ddl))
}

async fn extension_script(tx: &Transaction<'_>, role: &str, oid: u32) -> Result<ObjectScript, DetailError> {
    let row = tx
        .query_opt(
            "SELECT e.extname, n.nspname, e.extversion, obj_description(e.oid, 'pg_extension'), pg_get_userbyid(e.extowner)::text,
                    EXISTS (SELECT 1 FROM pg_seclabel l WHERE l.classoid = 'pg_extension'::regclass AND l.objoid = e.oid)
             FROM pg_extension e JOIN pg_namespace n ON n.oid = e.extnamespace WHERE e.oid = $1",
            &[&oid],
        )
        .await
        .map_err(failed)?
        .ok_or(DetailError::Gone)?;
    let name: String = row.get(0);
    let ddl = ddl::extension_script(
        &name,
        &row.get::<_, String>(1),
        &row.get::<_, Option<String>>(2).unwrap_or_default(),
        row.get::<_, Option<String>>(3).as_deref(),
        &owner(row.get(4), role),
        row.get(5),
    );
    Ok(script::object_script(&name, ddl))
}

async fn relation(tx: &Transaction<'_>, queries: &Queries, role: &str, oid: u32) -> Result<ObjectScript, DetailError> {
    let relations = tx.query(&queries.relations, &[&oid]).await.map_err(failed)?;
    let r = relations.iter().find(|r| relid(r) == oid).ok_or(DetailError::Gone)?;
    let name: String = r.get(2);
    let ddl = match r.get::<_, &str>(0) {
        "r" | "p" | "f" => ddl::table(&table(oid, &Tree::load(tx, queries, role, &relations).await?)?),
        kind @ ("v" | "m") => {
            let oids = vec![oid];
            let params: &[&(dyn ToSql + Sync)] = &[&oids];
            let grants = relation_grants_query();
            let (query, columns, indexes, triggers, rules, grants) = tokio::try_join!(
                tx.query_one("SELECT pg_get_viewdef(($1::oid[])[1], true)", params),
                tx.query(&queries.columns, params),
                tx.query(INDEXES, params),
                tx.query(TRIGGERS, params),
                tx.query(RULES, params),
                tx.query(&grants, params)
            )
            .map_err(failed)?;
            ddl::view(&ddl::View {
                schema: r.get(1),
                name: name.clone(),
                materialized: kind == "m",
                populated: r.get(10),
                access_method: r.get(15),
                options: r.get(4),
                tablespace: r.get(14),
                query: query.get::<_, Option<String>>(0).ok_or(DetailError::Gone)?,
                comment: r.get(5),
                columns: columns.iter().map(column).collect(),
                indexes: indexes.iter().map(index_row).collect(),
                triggers: triggers.iter().filter_map(cloned_trigger_row).collect(),
                rules: rules.iter().map(rule_row).collect(),
                unscripted: unscripted(r, &grants, role),
            })
        }
        _ => return Err(refused("Only tables and views can be scripted.")),
    };
    Ok(script::object_script(&name, ddl))
}

/// Every query's last column is the relation the row belongs to.
fn relid(r: &Row) -> u32 {
    r.get(r.len() - 1)
}

/// A table's rows and its partitions', each read once for the whole tree.
struct Tree<'a> {
    role: &'a str,
    relations: &'a [Row],
    sequence_states: Rows,
    columns: Rows,
    sequences: Rows,
    constraints: Rows,
    indexes: Rows,
    attached: Rows,
    triggers: Rows,
    rules: Rows,
    policies: Rows,
    statistics: Rows,
    grants: Rows,
}

#[derive(Default)]
struct Rows(HashMap<u32, Vec<Row>>);

impl Rows {
    fn new(rows: Vec<Row>) -> Rows {
        let mut by = Rows::default();
        for r in rows {
            by.0.entry(relid(&r)).or_default().push(r);
        }
        by
    }

    fn of(&self, oid: u32) -> &[Row] {
        self.0.get(&oid).map_or(&[], Vec::as_slice)
    }
}

impl<'a> Tree<'a> {
    async fn load(
        tx: &Transaction<'_>,
        queries: &Queries,
        role: &'a str,
        relations: &'a [Row],
    ) -> Result<Tree<'a>, DetailError> {
        let oids: Vec<u32> = relations.iter().map(relid).collect();
        let params: &[&(dyn ToSql + Sync)] = &[&oids];
        let grants = relation_grants_query();
        let (
            columns,
            sequences,
            sequence_states,
            constraints,
            indexes,
            attached,
            triggers,
            rules,
            policies,
            statistics,
            grants,
        ) = tokio::try_join!(
            tx.query(&queries.columns, params),
            tx.query(OWNED_SEQUENCES, params),
            tx.query(&queries.sequence_states, params),
            tx.query(CONSTRAINTS, params),
            tx.query(INDEXES, params),
            tx.query(ATTACHED_INDEXES, params),
            tx.query(TRIGGERS, params),
            tx.query(RULES, params),
            tx.query(POLICIES, params),
            tx.query(&queries.statistics, params),
            tx.query(&grants, params),
        )
        .map_err(failed)?;
        Ok(Tree {
            role,
            relations,
            sequence_states: Rows::new(sequence_states),
            columns: Rows::new(columns),
            sequences: Rows::new(sequences),
            constraints: Rows::new(constraints),
            indexes: Rows::new(indexes),
            attached: Rows::new(attached),
            triggers: Rows::new(triggers),
            rules: Rows::new(rules),
            policies: Rows::new(policies),
            statistics: Rows::new(statistics),
            grants: Rows::new(grants),
        })
    }

    /// The partitions directly under `oid`, the default partition last.
    fn partitions(&self, oid: u32) -> Vec<u32> {
        let mut children: Vec<&Row> =
            self.relations.iter().filter(|r| r.get::<_, Option<u32>>(24) == Some(oid)).collect();
        children.sort_by_key(|r| (r.get::<_, Option<&str>>(7) == Some("DEFAULT"), r.get::<_, &str>(2)));
        children.into_iter().map(relid).collect()
    }
}

fn table(oid: u32, tree: &Tree) -> Result<ddl::Table, DetailError> {
    let r = tree.relations.iter().find(|r| relid(r) == oid).ok_or(DetailError::Gone)?;
    let (columns, sequences, constraints, indexes, attached) = (
        tree.columns.of(oid),
        tree.sequences.of(oid),
        tree.constraints.of(oid),
        tree.indexes.of(oid),
        tree.attached.of(oid),
    );
    let (triggers, rules, policies, statistics, grants) = (
        tree.triggers.of(oid),
        tree.rules.of(oid),
        tree.policies.of(oid),
        tree.statistics.of(oid),
        tree.grants.of(oid),
    );
    let schema: String = r.get(1);
    let serials: Vec<String> = columns.iter().filter_map(|c| c.get::<_, Option<String>>(30)).collect();
    let parent = match r.get::<_, Option<String>>(7) {
        Some(bound) => ddl::Parent::Partition { schema: r.get(8), name: r.get(9), bound },
        None => {
            let (schemas, names): (Vec<String>, Vec<String>) = (r.get(19), r.get(20));
            if schemas.is_empty() {
                ddl::Parent::None
            } else {
                ddl::Parent::Inherits(schemas.into_iter().zip(names).collect())
            }
        }
    };
    let children = tree.partitions(oid).into_iter().map(|child| table(child, tree)).collect::<Result<Vec<_>, _>>()?;
    let (lost, constraints): (Vec<&Row>, Vec<&Row>) = constraints.iter().partition(|k| k.get(9));
    Ok(ddl::Table {
        name: r.get(2),
        unlogged: r.get(3),
        of_type: r.get(21),
        foreign: r.get::<_, Option<String>>(17).map(|server| ddl::Foreign { server, options: r.get(18) }),
        access_method: r.get(15),
        options: r.get(4),
        tablespace: r.get(14),
        comment: r.get(5),
        columns: columns.iter().map(column).collect(),
        owned_sequences: sequences
            .iter()
            .filter(|s| !(s.get::<_, &str>(0) == schema && serials.iter().any(|n| n == s.get::<_, &str>(1))))
            .map(owned_sequence)
            .collect(),
        sequence_states: tree
            .sequence_states
            .of(oid)
            .iter()
            .map(|s| ddl::SequenceState { schema: s.get(0), name: s.get(1), unlogged: s.get(2), comment: s.get(3) })
            .collect(),
        constraints: constraints.into_iter().filter_map(constraint_row).collect(),
        parent,
        partition_key: r.get(6),
        partitions: children,
        indexes: indexes.iter().map(index_row).collect(),
        attached_indexes: attached
            .iter()
            .map(|a| ddl::AttachedIndex {
                name: a.get(0),
                parent: a.get(1),
                clustered: a.get(2),
                replica_identity: a.get(3),
                comment: a.get(4),
                options: a.get(5),
                parent_options: a.get(6),
                tablespace: a.get(7),
            })
            .collect(),
        replica_identity: match r.get::<_, &str>(13) {
            "n" => ddl::ReplicaIdentity::Nothing,
            "f" => ddl::ReplicaIdentity::Full,
            "i" => ddl::ReplicaIdentity::Index,
            _ => ddl::ReplicaIdentity::Default,
        },
        triggers: triggers.iter().filter_map(cloned_trigger_row).collect(),
        rules: rules.iter().map(rule_row).collect(),
        row_security: r.get(11),
        force_row_security: r.get(12),
        policies: policies
            .iter()
            .map(|p| ddl::Policy {
                name: p.get(0),
                permissive: p.get(1),
                command: first(p.get(2)),
                roles: p.get(3),
                using: p.get(4),
                check: p.get(5),
                comment: p.get(6),
            })
            .collect(),
        statistics: statistics
            .iter()
            .filter_map(|s| {
                Some(ddl::Statistics {
                    schema: s.get(0),
                    name: s.get(1),
                    definition: s.get::<_, Option<String>>(2)?,
                    target: s.get(3),
                    comment: s.get(4),
                })
            })
            .collect(),
        unscripted: ddl::Unscripted {
            inherited_not_nulls: lost.iter().map(|k| k.get(0)).collect(),
            ..unscripted(r, grants, tree.role)
        },
        schema,
    })
}

fn first(s: &str) -> char {
    s.chars().next().unwrap_or(' ')
}

fn owned_sequence(r: &Row) -> ddl::Sequence {
    ddl::Sequence {
        schema: r.get(0),
        name: r.get(1),
        column: r.get(2),
        type_name: r.get(3),
        start: r.get(4),
        increment: r.get(5),
        min: r.get(6),
        max: r.get(7),
        cache: r.get(8),
        cycle: r.get(9),
    }
}

fn column(r: &Row) -> ddl::Column {
    // From PostgreSQL 17 a partition shares its identity with its topmost ancestor, which owns
    // the sequence, and `PARTITION OF` gives it the identity, so it has none of its own to script.
    let identity = match (r.get::<_, &str>(3), r.get::<_, Option<&str>>(10)) {
        ("", _) | (_, None) => None,
        (kind, Some(_)) => Some(ddl::Identity {
            always: kind == "a",
            sequence: ddl::Sequence {
                schema: r.get(9),
                name: r.get(10),
                column: r.get(0),
                type_name: r.get(11),
                start: r.get(12),
                increment: r.get(13),
                min: r.get(14),
                max: r.get(15),
                cache: r.get(16),
                cycle: r.get(17),
            },
        }),
    };
    let generated = match r.get::<_, &str>(4) {
        "s" => Some(ddl::Generated::Stored),
        "v" => Some(ddl::Generated::Virtual),
        _ => None,
    };
    let inherited = r.get::<_, Option<i64>>(25).map(|position| ddl::Inherited {
        replay_order: position,
        default: r.get(26),
        not_null: r.get(27),
        storage: r.get(28),
        compression: r.get(29),
    });
    ddl::Column {
        name: r.get(0),
        type_name: r.get(1),
        collation: r.get::<_, Option<String>>(6).zip(r.get::<_, Option<String>>(7)),
        not_null: r.get(2),
        default: r.get(5),
        generated,
        identity,
        comment: r.get(8),
        declared: r.get(18),
        inherited,
        statistics: r.get(19),
        storage: r.get(20),
        type_storage: r.get(21),
        compression: r.get(22),
        options: r.get(23),
        fdw_options: r.get(24),
    }
}

/// `None` for a row whose `pg_get_*def` is NULL: the object was dropped after the query took its snapshot.
fn constraint_row(r: &Row) -> Option<ddl::Constraint> {
    Some(ddl::Constraint {
        name: r.get(0),
        definition: r.get::<_, Option<String>>(1)?,
        comment: r.get(2),
        after_indexes: r.get(3),
        index_options: r.get(4),
        index_tablespace: r.get(5),
        inherited: r.get(6),
        after_tree: r.get(7),
        not_null_column: r.get(8),
    })
}

fn index_row(r: &Row) -> ddl::Index {
    let (columns, targets): (Vec<i32>, Vec<i32>) = (r.get(6), r.get(7));
    ddl::Index {
        name: r.get(0),
        definition: r.get(1),
        comment: r.get(2),
        clustered: r.get(3),
        replica_identity: r.get(4),
        tablespace: r.get(5),
        statistics: columns
            .into_iter()
            .zip(targets)
            .map(|(column, target)| ddl::ColumnStatistics { column, target })
            .collect(),
        invalid: r.get(8),
        partitioned: r.get(9),
    }
}

fn trigger_row(r: &Row) -> Option<ddl::Trigger> {
    Some(ddl::Trigger {
        name: r.get(0),
        definition: r.get::<_, Option<String>>(1)?,
        enabled: first(r.get(2)),
        comment: r.get(3),
        cloned: false,
    })
}

fn cloned_trigger_row(r: &Row) -> Option<ddl::Trigger> {
    Some(ddl::Trigger { cloned: r.get(4), ..trigger_row(r)? })
}

fn rule_row(r: &Row) -> ddl::Rule {
    ddl::Rule { name: r.get(0), definition: r.get(1), enabled: first(r.get(2)), comment: r.get(3) }
}

fn keyword(relkind: &str) -> &'static str {
    match relkind {
        "f" => "FOREIGN TABLE",
        "m" => "MATERIALIZED VIEW",
        _ => "TABLE",
    }
}

async fn routine(tx: &Transaction<'_>, role: &str, oid: u32) -> Result<ObjectScript, DetailError> {
    let grants = routine_grants_query();
    let params: &[&(dyn ToSql + Sync)] = &[&oid];
    let (r, grants) = tokio::try_join!(tx.query_opt(ROUTINE, params), tx.query(&grants, params)).map_err(failed)?;
    let r = r.ok_or(DetailError::Gone)?;
    let kind: &str = r.get(0);
    let name: String = r.get(1);
    let arguments = r.get::<_, Option<&str>>(5).ok_or(DetailError::Gone)?;
    let Some(definition) = r.get::<_, Option<String>>(2) else {
        return Err(refused("Aggregates cannot be scripted yet."));
    };
    let signature = format!("{}({arguments})", ddl::qualified(r.get(4), &name));
    let routine = ddl::Routine {
        definition,
        procedure: kind == "p",
        signature,
        comment: r.get(3),
        privileges: grants.iter().map(grant_row).collect(),
        owner: owner(r.get(6), role),
        security_definer: r.get(7),
        extensions: r.get(8),
        security_labels: r.get(9),
    };
    Ok(script::object_script(&name, ddl::routine(&routine)))
}

async fn index(tx: &Transaction<'_>, relation: u32, name: &str) -> Result<ObjectScript, DetailError> {
    let sql = "SELECT ic.relname,
                      CASE WHEN k.oid IS NULL THEN pg_get_indexdef(i.indexrelid) END,
                      obj_description(i.indexrelid, 'pg_class'), i.indisclustered, i.indisreplident, ts.spcname,
                      ARRAY(SELECT a.attnum::int4 FROM pg_attribute a WHERE a.attrelid = i.indexrelid AND NULLIF(a.attstattarget, -1) IS NOT NULL ORDER BY a.attnum),
                      ARRAY(SELECT NULLIF(a.attstattarget, -1)::int4 FROM pg_attribute a WHERE a.attrelid = i.indexrelid AND NULLIF(a.attstattarget, -1) IS NOT NULL ORDER BY a.attnum),
                      NOT i.indisvalid, ic.relkind = 'I',
                      k.conname, pg_get_constraintdef(k.oid), n.nspname, c.relname, c.relkind::text, obj_description(k.oid, 'pg_constraint'),
                      CASE WHEN k.contype IN ('p', 'u') THEN coalesce(ic.reloptions, '{}') ELSE '{}' END,
                      (SELECT h.inhparent::regclass::text FROM pg_inherits h WHERE h.inhrelid = i.indexrelid)
               FROM pg_index i JOIN pg_class ic ON ic.oid = i.indexrelid JOIN pg_class c ON c.oid = i.indrelid JOIN pg_namespace n ON n.oid = c.relnamespace
               LEFT JOIN pg_tablespace ts ON ts.oid = ic.reltablespace
               LEFT JOIN pg_constraint k ON k.conindid = i.indexrelid AND k.conrelid = i.indrelid AND k.contype IN ('p', 'u', 'x')
               WHERE i.indrelid = $1 AND ic.relname = $2";
    let r = tx.query_opt(sql, &[&relation, &name]).await.map_err(failed)?.ok_or(DetailError::Gone)?;
    let constraint = match r.get::<_, Option<String>>(10) {
        Some(name) => Some(ddl::Constraint {
            name,
            definition: r.get::<_, Option<String>>(11).ok_or(DetailError::Gone)?,
            comment: r.get(15),
            index_options: r.get(16),
            index_tablespace: r.get(5),
            ..Default::default()
        }),
        None => None,
    };
    let (schema, table): (&str, &str) = (r.get(12), r.get(13));
    let ddl = ddl::index_script(
        &index_row(&r),
        constraint.as_ref(),
        r.get(17),
        keyword(r.get(14)),
        schema,
        &ddl::qualified(schema, table),
    );
    Ok(script::object_script(name, ddl))
}

async fn trigger(tx: &Transaction<'_>, relation: u32, name: &str) -> Result<ObjectScript, DetailError> {
    let sql = "SELECT t.tgname, pg_get_triggerdef(t.oid), t.tgenabled::text, obj_description(t.oid, 'pg_trigger'), n.nspname, c.relname, c.relkind::text
               FROM pg_trigger t JOIN pg_class c ON c.oid = t.tgrelid JOIN pg_namespace n ON n.oid = c.relnamespace
               WHERE t.tgrelid = $1 AND t.tgname = $2";
    let r = tx.query_opt(sql, &[&relation, &name]).await.map_err(failed)?.ok_or(DetailError::Gone)?;
    let table = ddl::qualified(r.get(4), r.get(5));
    Ok(script::object_script(
        name,
        script::Ddl::complete(ddl::script(ddl::trigger(
            &trigger_row(&r).ok_or(DetailError::Gone)?,
            keyword(r.get(6)),
            &table,
        ))),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queries_read_only_the_columns_the_server_has() {
        let v12 = Queries::for_version(120000);
        assert!(
            !v12.columns.contains("attcompression::text") && !v12.statistics.contains("stxstattarget"),
            "{}\n{}",
            v12.columns,
            v12.statistics
        );
        let v13 = Queries::for_version(130000);
        assert!(!v13.columns.contains("attcompression::text") && v13.statistics.contains("stxstattarget"));
        let v14 = Queries::for_version(140000);
        assert!(v14.columns.contains("a.attcompression::text") && v14.columns.contains("pa.attcompression::text"));
        assert!(
            v14.columns.contains("unnest(a.attoptions) o WHERE split_part(o, '=', 1) <> 'pg_attver'"),
            "{}",
            v14.columns
        );
        assert!(
            !v14.relations.contains("pg_publication_namespace")
                && Queries::for_version(150000).relations.contains("pg_publication_namespace")
        );
    }
}
