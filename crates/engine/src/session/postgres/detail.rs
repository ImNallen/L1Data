use tokio_postgres::Client;

use crate::model::{
    ColumnDefault, DatabaseRef, DefaultValue, FkAction, IndexInfo, KeyInfo, KeyKind, RelationDetail, TriggerEvent,
    TriggerImplementation, TriggerInfo, TriggerTiming,
};
use crate::script::postgres::{group_end, top_level};

/// Reads one relation's defaults, keys, indexes, and triggers. Every query is an index lookup on
/// the relation's OID, so the cost does not grow with the size of the database. `None` when the
/// OID names no relation.
pub async fn load(
    client: &Client,
    db: DatabaseRef,
    oid: u32,
    catalog_version: u64,
) -> Result<Option<RelationDetail>, tokio_postgres::Error> {
    let params: &[&(dyn tokio_postgres::types::ToSql + Sync)] = &[&oid];
    let (exists, defaults, keys, indexes, triggers) = tokio::try_join!(
        client.query_opt("SELECT 1 FROM pg_class WHERE oid = $1", params),
        client.query(
            "SELECT a.attname, a.attidentity::text, a.attgenerated::text, pg_get_expr(d.adbin, d.adrelid)
             FROM pg_attribute a LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum
             WHERE a.attrelid = $1 AND a.attnum > 0 AND NOT a.attisdropped AND (a.attidentity <> '' OR d.adbin IS NOT NULL)
             ORDER BY a.attnum",
            params,
        ),
        client.query(
            "SELECT k.conname, k.contype::text,
                    ARRAY(SELECT a.attname::text FROM unnest(k.conkey) WITH ORDINALITY u(n, i)
                          JOIN pg_attribute a ON a.attrelid = k.conrelid AND a.attnum = u.n ORDER BY u.i),
                    fn.nspname, fc.relname,
                    ARRAY(SELECT a.attname::text FROM unnest(k.confkey) WITH ORDINALITY u(n, i)
                          JOIN pg_attribute a ON a.attrelid = k.confrelid AND a.attnum = u.n ORDER BY u.i),
                    k.confdeltype::text, k.confupdtype::text, pg_get_constraintdef(k.oid)
             FROM pg_constraint k
             LEFT JOIN pg_class fc ON fc.oid = k.confrelid LEFT JOIN pg_namespace fn ON fn.oid = fc.relnamespace
             WHERE k.conrelid = $1 AND k.contype IN ('p', 'f', 'u', 'c')
               -- A foreign key into a partitioned table also gets one clone per referenced partition.
               AND NOT EXISTS (SELECT 1 FROM pg_constraint p WHERE p.oid = k.conparentid AND p.conrelid = k.conrelid)
             ORDER BY position(k.contype IN 'pfuc'), k.conname",
            params,
        ),
        client.query(
            "SELECT ic.relname, am.amname, i.indisunique, i.indisprimary, i.indisvalid,
                    pg_get_indexdef(i.indexrelid), pg_get_expr(i.indpred, i.indrelid, true)
             FROM pg_index i JOIN pg_class ic ON ic.oid = i.indexrelid JOIN pg_am am ON am.oid = ic.relam
             WHERE i.indrelid = $1
             ORDER BY i.indisprimary DESC, i.indisunique DESC, ic.relname",
            params,
        ),
        client.query(
            "SELECT t.tgname, t.tgtype, t.tgenabled::text, t.tgfoid::regproc::text
             FROM pg_trigger t
             WHERE t.tgrelid = $1
               -- PG 12-14 mark a partition's clone of its parent's trigger internal; psql shows it the same way.
               AND (NOT t.tgisinternal OR EXISTS (SELECT 1 FROM pg_depend d
                    WHERE d.classid = 'pg_trigger'::regclass AND d.objid = t.oid
                      AND d.refclassid = 'pg_trigger'::regclass AND d.deptype = 'P'))
             ORDER BY t.tgname",
            params,
        ),
    )?;
    if exists.is_none() {
        return Ok(None);
    }

    let defaults = defaults
        .iter()
        .map(|r| ColumnDefault { column: r.get(0), value: default_value(r.get(1), r.get(2), r.get(3)) })
        .collect();

    // `pg_get_constraintdef` and `pg_get_indexdef` return NULL for an object dropped after the query took its snapshot.
    let keys = keys
        .iter()
        .filter_map(|r| {
            let kind = match r.get::<_, String>(1).as_str() {
                "p" => KeyKind::Primary,
                "u" => KeyKind::Unique,
                "f" => KeyKind::Foreign {
                    ref_schema: r.get(3),
                    ref_table: r.get(4),
                    ref_columns: r.get(5),
                    on_delete: fk_action(&r.get::<_, String>(6)),
                    on_update: fk_action(&r.get::<_, String>(7)),
                },
                _ => KeyKind::Check { definition: r.get::<_, Option<String>>(8)? },
            };
            Some(KeyInfo { name: r.get(0), columns: r.get(2), kind })
        })
        .collect();

    let indexes = indexes
        .iter()
        .filter_map(|r| {
            Some(IndexInfo {
                name: r.get(0),
                method: r.get(1),
                unique: r.get(2),
                primary: r.get(3),
                valid: r.get(4),
                keys: index_keys(r.get::<_, Option<&str>>(5)?).to_owned(),
                predicate: r.get(6),
            })
        })
        .collect();

    let triggers = triggers
        .iter()
        .map(|r| {
            let (timing, events, per_row) = decode_tgtype(r.get(1));
            TriggerInfo {
                name: r.get(0),
                timing,
                events,
                per_row,
                enabled: matches!(r.get::<_, &str>(2), "O" | "A"),
                implementation: TriggerImplementation::Function { name: r.get(3) },
            }
        })
        .collect();

    Ok(Some(RelationDetail { db, oid, catalog_version, defaults, keys, indexes, triggers }))
}

/// From `pg_attribute.attidentity`, `pg_attribute.attgenerated`, and the `pg_attrdef` expression.
fn default_value(identity: &str, generated: &str, expr: Option<String>) -> DefaultValue {
    match (identity, generated, expr) {
        ("a", _, _) => DefaultValue::Identity { always: true },
        ("d", _, _) => DefaultValue::Identity { always: false },
        (_, "s" | "v", Some(sql)) => DefaultValue::Generated { sql },
        (_, _, sql) => DefaultValue::Expr { sql: strip_literal_cast(&sql.unwrap_or_default()).to_owned() },
    }
}

fn fk_action(code: &str) -> FkAction {
    match code {
        "r" => FkAction::Restrict,
        "c" => FkAction::Cascade,
        "n" => FkAction::SetNull,
        "d" => FkAction::SetDefault,
        _ => FkAction::NoAction,
    }
}

/// `pg_trigger.tgtype` bits, from `catalog/pg_trigger.h`.
fn decode_tgtype(tgtype: i16) -> (TriggerTiming, Vec<TriggerEvent>, bool) {
    let has = |bit: u8| tgtype & (1 << bit) != 0;
    let timing = if has(6) {
        TriggerTiming::InsteadOf
    } else if has(1) {
        TriggerTiming::Before
    } else {
        TriggerTiming::After
    };
    let events =
        [(2, TriggerEvent::Insert), (4, TriggerEvent::Update), (3, TriggerEvent::Delete), (5, TriggerEvent::Truncate)]
            .into_iter()
            .filter_map(|(bit, event)| has(bit).then_some(event))
            .collect();
    (timing, events, has(0))
}

/// The key list of a `pg_get_indexdef`, with its `INCLUDE` list:
/// `CREATE INDEX i ON public.t USING btree (a DESC) INCLUDE (b) WHERE (c)` gives `(a DESC) INCLUDE (b)`.
pub fn index_keys(indexdef: &str) -> &str {
    let Some(open) = top_level(indexdef, " USING ").and_then(|at| indexdef[at..].find('(').map(|i| at + i)) else {
        return indexdef;
    };
    let Some(mut end) = group_end(indexdef, open) else { return indexdef };
    if indexdef[end..].starts_with(" INCLUDE (") {
        end = group_end(indexdef, end + " INCLUDE ".len()).unwrap_or(end);
    }
    &indexdef[open..end]
}

/// `'pending'::text` gives `'pending'`; anything but a lone cast literal comes back unchanged.
fn strip_literal_cast(expr: &str) -> &str {
    let Some(body) = expr.strip_prefix('\'') else { return expr };
    let mut chars = body.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if c != '\'' {
            continue;
        }
        if chars.peek().is_some_and(|&(_, next)| next == '\'') {
            chars.next();
            continue;
        }
        let end = i + 2;
        let is_type = |t: &str| !t.is_empty() && t.chars().all(|c| c.is_alphanumeric() || " _.\"()[],".contains(c));
        return match expr[end..].strip_prefix("::") {
            Some(cast) if is_type(cast) => &expr[..end],
            _ => expr,
        };
    }
    expr
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_keys_are_the_key_list_and_include_list() {
        let cut = |def| index_keys(def);
        assert_eq!(cut("CREATE UNIQUE INDEX orders_pkey ON public.orders USING btree (id)"), "(id)");
        assert_eq!(
            cut("CREATE INDEX orders_created_at_idx ON public.orders USING btree (created_at DESC)"),
            "(created_at DESC)"
        );
        assert_eq!(
            cut("CREATE INDEX orders_open_idx ON public.orders USING btree (status) WHERE (status <> 'paid'::text)"),
            "(status)"
        );
        assert_eq!(
            cut("CREATE INDEX i ON public.t USING btree (a, lower((b)::text)) INCLUDE (c, d) WHERE (a > 0)"),
            "(a, lower((b)::text)) INCLUDE (c, d)"
        );
        assert_eq!(
            cut(r#"CREATE INDEX "x USING y" ON public."t (" USING gin (doc jsonb_path_ops)"#),
            "(doc jsonb_path_ops)"
        );
        assert_eq!(cut("CREATE INDEX i ON public.t USING btree (((a || ')'::text)))"), "(((a || ')'::text)))");
    }

    #[test]
    fn a_lone_literal_default_drops_its_cast() {
        assert_eq!(strip_literal_cast("'pending'::text"), "'pending'");
        assert_eq!(strip_literal_cast("'{}'::jsonb"), "'{}'");
        assert_eq!(strip_literal_cast("'it''s'::character varying"), "'it''s'");
        assert_eq!(strip_literal_cast("now()"), "now()");
        assert_eq!(strip_literal_cast("('a'::text || 'b'::text)"), "('a'::text || 'b'::text)");
        assert_eq!(strip_literal_cast("'a'::text || 'b'::text"), "'a'::text || 'b'::text");
        assert_eq!(strip_literal_cast("'2026-01-01'::date + 1"), "'2026-01-01'::date + 1");
    }

    #[test]
    fn stored_and_virtual_generated_columns_are_generated() {
        let generated = |kind| default_value("", kind, Some("(a * 2)".into()));
        assert_eq!(generated("s"), DefaultValue::Generated { sql: "(a * 2)".into() });
        assert_eq!(generated("v"), DefaultValue::Generated { sql: "(a * 2)".into() });
        assert_eq!(default_value("", "", Some("'x'::text".into())), DefaultValue::Expr { sql: "'x'".into() });
        assert_eq!(default_value("d", "", None), DefaultValue::Identity { always: false });
    }

    #[test]
    fn tgtype_decodes_timing_events_and_level() {
        assert_eq!(decode_tgtype(0b1_0011), (TriggerTiming::Before, vec![TriggerEvent::Update], true));
        assert_eq!(
            decode_tgtype(0b1_1101),
            (TriggerTiming::After, vec![TriggerEvent::Insert, TriggerEvent::Update, TriggerEvent::Delete], true)
        );
        assert_eq!(decode_tgtype(0b10_0000), (TriggerTiming::After, vec![TriggerEvent::Truncate], false));
        assert_eq!(decode_tgtype(0b100_0101), (TriggerTiming::InsteadOf, vec![TriggerEvent::Insert], true));
    }
}
