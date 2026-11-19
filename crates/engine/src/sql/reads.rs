use super::scope::{Class, map_lexemes, unquote};
use crate::model::DatabaseKind;

const RESETS: [&str; 18] = [
    "WHERE",
    "GROUP",
    "ORDER",
    "HAVING",
    "LIMIT",
    "OFFSET",
    "UNION",
    "INTERSECT",
    "EXCEPT",
    "WINDOW",
    "FETCH",
    "FOR",
    "RETURNING",
    "SET",
    "VALUES",
    "SELECT",
    "OUTPUT",
    "OPTION",
];

#[derive(Clone, Copy, PartialEq)]
enum Expect {
    No,
    FromItem,
    Target,
}

#[derive(Default)]
struct Level {
    /// In a `FROM` list, where a comma starts another item. Only a clause word ends it, since an `ON`
    /// condition holds no comma outside parentheses.
    from: bool,
    /// Saw `SELECT`, `DELETE`, or `UPDATE`, so a `FROM` here starts a `FROM` list and is not
    /// `extract(year FROM x)`.
    query: bool,
}

/// A relation or CTE name the statement reads more than once, as first written. Both servers describe
/// a CTE's column as its base table's, and a self-join's columns as one table's, so a result whose
/// columns share one table can still mix rows. The count is by bare name, case folded, so it may name
/// two tables that only share a name, and it counts a recursive CTE's own reference.
pub(crate) fn repeated_read(text: &str, kind: DatabaseKind) -> Option<String> {
    let lexemes: Vec<_> = map_lexemes(text, kind)
        .into_iter()
        .filter(|l| !matches!(l.class, Class::LineComment | Class::BlockComment))
        .collect();
    let word =
        |i: usize| lexemes.get(i).filter(|l| l.class == Class::Word).map(|l| text[l.span.clone()].to_ascii_uppercase());
    let name = |i: usize| {
        let l = lexemes.get(i)?;
        let raw = &text[l.span.clone()];
        match l.class {
            Class::Word | Class::Param | Class::Reference => Some(raw.to_owned()),
            Class::Name => Some(unquote(raw).unwrap_or_else(|| raw.to_owned())),
            _ => None,
        }
    };
    let mut first = word(0);
    let mut levels = vec![Level::default()];
    let mut expect = Expect::No;
    let mut seen: Vec<(String, String)> = Vec::new();
    let mut i = 0;
    while i < lexemes.len() {
        match lexemes[i].class {
            Class::Open => {
                let item = expect == Expect::FromItem;
                levels.push(Level { from: item, query: false });
                expect = if item { Expect::FromItem } else { Expect::No };
            }
            Class::Close => {
                if levels.len() > 1 {
                    levels.pop();
                }
                expect = Expect::No;
            }
            Class::Comma if levels.last().is_some_and(|l| l.from) => expect = Expect::FromItem,
            Class::Semi => {
                levels.truncate(1);
                levels[0] = Level::default();
                expect = Expect::No;
                first = word(i + 1);
            }
            _ => {
                let level = levels.last_mut().expect("level 0 is never popped");
                let up = word(i);
                match up.as_deref() {
                    Some("FROM") if word(i.wrapping_sub(1)).as_deref() == Some("DISTINCT") => {}
                    Some("FROM") if level.query => {
                        level.from = true;
                        expect = Expect::FromItem;
                    }
                    Some("JOIN" | "APPLY") => {
                        level.from = true;
                        expect = Expect::FromItem;
                    }
                    Some("USING") if matches!(first.as_deref(), Some("DELETE" | "MERGE")) => {
                        level.from = true;
                        expect = Expect::FromItem;
                    }
                    Some("DELETE") => level.query = true,
                    Some("UPDATE") => {
                        level.query = true;
                        expect = Expect::Target;
                    }
                    Some("INTO" | "TABLE" | "MERGE") => expect = Expect::Target,
                    Some("ON") => expect = Expect::No,
                    Some(w) if RESETS.contains(&w) => {
                        level.from = false;
                        level.query |= w == "SELECT";
                        expect = Expect::No;
                    }
                    Some("ONLY" | "LATERAL") if expect != Expect::No => {}
                    _ if expect != Expect::No => {
                        let context = expect;
                        expect = Expect::No;
                        if let Some(mut path) = name(i) {
                            while lexemes.get(i + 1).is_some_and(|l| l.class == Class::Dot)
                                && let Some(part) = name(i + 2)
                            {
                                path = format!("{path}.{part}");
                                i += 2;
                            }
                            let call = context == Expect::FromItem
                                && lexemes.get(i + 1).is_some_and(|l| l.class == Class::Open);
                            if !call {
                                let key = path.rsplit('.').next().unwrap_or(&path).to_lowercase();
                                if let Some((_, shown)) = seen.iter().find(|(k, _)| *k == key) {
                                    return Some(shown.clone());
                                }
                                seen.push((key, path));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use DatabaseKind::{Postgres, SqlServer};

    fn pg(sql: &str) -> Option<String> {
        repeated_read(sql, Postgres)
    }

    fn ms(sql: &str) -> Option<String> {
        repeated_read(sql, SqlServer)
    }

    #[test]
    fn a_self_join_reads_its_table_twice() {
        assert_eq!(
            pg("SELECT a.id, b.status FROM public.orders a JOIN public.orders b ON b.id = a.id + 1").as_deref(),
            Some("public.orders")
        );
        assert_eq!(pg("select a.id, b.status from orders a, orders b where b.id = a.id").as_deref(), Some("orders"));
        assert_eq!(
            pg("SELECT * FROM orders a JOIN customers c ON c.id = a.customer_id, orders b").as_deref(),
            Some("orders")
        );
        assert_eq!(pg("SELECT * FROM (orders a JOIN orders b ON b.id = a.id)").as_deref(), Some("orders"));
        assert_eq!(
            pg("SELECT * FROM (orders a JOIN customers c ON true) JOIN (orders b JOIN customers d ON true) ON true")
                .as_deref(),
            Some("orders")
        );
    }

    #[test]
    fn a_cte_read_twice_counts_by_its_name_and_its_body_counts_too() {
        assert_eq!(
            pg("WITH x AS (SELECT * FROM orders) SELECT a.id, b.status FROM x a JOIN x b ON b.id = a.id").as_deref(),
            Some("x")
        );
        assert_eq!(
            pg("WITH x AS (SELECT * FROM orders) SELECT x.id, o.status FROM x JOIN orders o ON o.id = x.id + 1")
                .as_deref(),
            Some("orders")
        );
        assert_eq!(pg("WITH x AS MATERIALIZED (SELECT * FROM orders) SELECT * FROM x"), None);
    }

    #[test]
    fn a_union_or_a_subquery_of_the_same_table_reads_it_twice() {
        assert_eq!(pg("SELECT id, status FROM orders UNION SELECT id, status FROM orders").as_deref(), Some("orders"));
        assert_eq!(
            pg("SELECT * FROM orders WHERE id IN (SELECT id FROM orders WHERE status = 'new')").as_deref(),
            Some("orders")
        );
        assert_eq!(pg("SELECT * FROM orders WHERE EXISTS (SELECT 1 FROM ONLY orders)").as_deref(), Some("orders"));
    }

    #[test]
    fn one_read_of_each_table_is_not_repeated() {
        assert_eq!(pg("SELECT * FROM public.orders WHERE status = 'new' ORDER BY id"), None);
        assert_eq!(pg("SELECT o.* FROM orders o JOIN customers c ON c.id = o.customer_id"), None);
        assert_eq!(pg("SELECT * FROM orders a JOIN customers b USING (id) JOIN regions c USING (id)"), None);
        assert_eq!(pg("SELECT * FROM orders, generate_series(1, 2) g, generate_series(1, 3) h"), None);
        assert_eq!(
            pg(
                "SELECT extract(year FROM created_at), extract(month FROM created_at) FROM orders WHERE a IS DISTINCT FROM b AND c IS DISTINCT FROM b"
            ),
            None
        );
        assert_eq!(pg("SELECT * FROM orders -- FROM orders\nWHERE note = 'FROM orders'"), None);
    }

    #[test]
    fn names_compare_unquoted_and_case_folded_by_their_last_part() {
        assert_eq!(pg(r#"SELECT * FROM "Orders" a JOIN orders b ON true"#).as_deref(), Some("Orders"));
        assert_eq!(pg(r#"SELECT * FROM public."orders" a JOIN orders b ON true"#).as_deref(), Some("public.orders"));
        assert_eq!(pg("SELECT * FROM public.orders a JOIN audit.orders b ON true").as_deref(), Some("public.orders"));
    }

    #[test]
    fn t_sql_brackets_apply_and_three_part_names_count() {
        assert_eq!(
            ms("SELECT a.id, b.status FROM [dbo].[orders] a JOIN dbo.Orders b ON b.id = a.id + 1").as_deref(),
            Some("dbo.orders")
        );
        assert_eq!(
            ms("SELECT * FROM shop.dbo.orders a CROSS APPLY (SELECT TOP 1 * FROM [orders] b WHERE b.id > a.id) n")
                .as_deref(),
            Some("shop.dbo.orders")
        );
        assert_eq!(
            ms(";WITH x AS (SELECT * FROM dbo.orders) SELECT a.id, b.status FROM x a JOIN x b ON b.id = a.id")
                .as_deref(),
            Some("x")
        );
        assert_eq!(ms("SELECT * FROM dbo.orders WITH (NOLOCK), dbo.orders b").as_deref(), Some("dbo.orders"));
        assert_eq!(ms("SELECT id FROM dbo.orders UNION ALL SELECT id FROM dbo.orders").as_deref(), Some("dbo.orders"));
        assert_eq!(ms("SELECT o.* FROM dbo.orders o JOIN dbo.customers c ON c.id = o.customer_id"), None);
        assert_eq!(ms("SELECT * FROM [dbo].[order lines] o OUTER APPLY dbo.f(o.id) x OUTER APPLY dbo.f(o.id) y"), None);
    }
}
