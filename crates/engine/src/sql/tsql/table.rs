use super::{expression::expression_name, lex, quote_ident, quote_literal, word_at};
use crate::model::{ColumnFilter, ColumnInfo, FilterOp, TableQuery};
use crate::sql::mssql_value::{comparison_sql, display_sql};

pub(crate) fn parse_order_by(text: &str) -> Option<Vec<(String, bool)>> {
    let tokens = lex(text).ok()?;
    let mut out = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        if !expression_name(&tokens[i]) {
            return None;
        }
        let name = tokens[i].name()?;
        i += 1;
        let desc = word_at(&tokens, i, "DESC");
        if desc || word_at(&tokens, i, "ASC") {
            i += 1;
        }
        out.push((name, desc));
        if i == tokens.len() {
            break;
        }
        if tokens[i].text != "," || i + 1 == tokens.len() {
            return None;
        }
        i += 1;
    }
    Some(out)
}

pub(crate) fn format_order_by(keys: &[(String, bool)]) -> String {
    keys.iter()
        .map(|(name, desc)| format!("{} {}", quote_ident(name), if *desc { "DESC" } else { "ASC" }))
        .collect::<Vec<_>>()
        .join(", ")
}

fn like_pattern(value: &str, prefix_only: bool) -> Result<String, String> {
    let mut pattern = String::new();
    if !prefix_only {
        pattern.push('%');
    }
    for ch in value.chars() {
        if matches!(ch, '~' | '%' | '_' | '[') {
            pattern.push('~');
        }
        pattern.push(ch);
    }
    pattern.push('%');
    if pattern.encode_utf16().count() > 4000 {
        return Err("SQL Server text-search patterns cannot exceed 4000 UTF-16 code units after escaping. Use a shorter search.".into());
    }
    Ok(quote_literal(&pattern))
}

fn filter_sql(filter: &ColumnFilter, columns: &[ColumnInfo]) -> Result<String, String> {
    let info = columns
        .iter()
        .find(|c| c.name == filter.column)
        .ok_or_else(|| format!("Unknown filter column {}", filter.column))?;
    let column = quote_ident(&filter.column);
    let op = match filter.op {
        FilterOp::IsNull => return Ok(format!("{column} IS NULL")),
        FilterOp::NotNull => return Ok(format!("{column} IS NOT NULL")),
        FilterOp::Contains | FilterOp::StartsWith => {
            let display = display_sql(info, &column)?;
            return Ok(format!(
                "{display} LIKE {} ESCAPE N'~'",
                like_pattern(&filter.value, filter.op == FilterOp::StartsWith)?
            ));
        }
        FilterOp::Eq => "=",
        FilterOp::Ne => "<>",
        FilterOp::Lt => "<",
        FilterOp::Le => "<=",
        FilterOp::Gt => ">",
        FilterOp::Ge => ">=",
    };
    let (comparable, value) = comparison_sql(info, &column, &filter.value)?;
    Ok(format!("{comparable} {op} {value}"))
}

fn from_sql(schema: &str, table: &str, columns: &[ColumnInfo], query: &TableQuery) -> Result<String, String> {
    let mut sql = format!("FROM {}.{}", quote_ident(schema), quote_ident(table));
    let mut filters = Vec::new();
    if !query.r#where.trim().is_empty() {
        filters.push(format!("({}\n)", query.r#where.trim()));
    }
    filters.extend(query.filters.iter().map(|filter| filter_sql(filter, columns)).collect::<Result<Vec<_>, _>>()?);
    if !filters.is_empty() {
        sql.push_str(&format!(" WHERE {}", filters.join(" AND ")));
    }
    Ok(sql)
}

pub(crate) fn browse_sql(
    schema: &str,
    table: &str,
    projection: &str,
    columns: &[ColumnInfo],
    pk: &[String],
    query: &TableQuery,
    window: Option<(u32, u64)>,
) -> Result<String, String> {
    let mut sql = format!("SELECT {projection} {}", from_sql(schema, table, columns, query)?);
    let mut order = Vec::new();
    let user_order = !query.order_by.trim().is_empty();
    if user_order {
        order.push(format!("{}\n", query.order_by.trim()));
    }
    for column in pk {
        let column = quote_ident(column);
        order.push(if user_order { format!("(SELECT {column})") } else { column });
    }
    match window {
        Some((limit, offset)) => {
            if order.is_empty() {
                order.push("(SELECT NULL)".into());
            }
            sql.push_str(&format!(" ORDER BY {} OFFSET {offset} ROWS FETCH NEXT {limit} ROWS ONLY", order.join(", ")));
        }
        None if !order.is_empty() => sql.push_str(&format!(" ORDER BY {}", order.join(", "))),
        None => {}
    }
    Ok(sql)
}

pub(crate) fn count_sql(
    schema: &str,
    table: &str,
    columns: &[ColumnInfo],
    query: &TableQuery,
) -> Result<String, String> {
    Ok(format!("SELECT COUNT_BIG(*) {}", from_sql(schema, table, columns, query)?))
}

#[cfg(test)]
mod tests {
    use super::super::validate_expression;
    use super::*;
    use crate::model::TypeCategory;

    #[test]
    fn simple_order_inspection_and_generation_preserve_identifier_spelling() {
        let keys = vec![("Name".into(), true), ("旅]人".into(), false), ("id".into(), false)];
        assert_eq!(parse_order_by("Name DESC, [旅]]人] ASC, \"id\""), Some(keys.clone()));
        assert_eq!(format_order_by(&keys), "[Name] DESC, [旅]]人] ASC, [id] ASC");
        assert_eq!(parse_order_by(&format_order_by(&keys)), Some(keys));
        assert_eq!(parse_order_by(""), Some(vec![]));
        for advanced in [
            "LOWER(Name) DESC",
            "dbo.id",
            "id COLLATE x",
            "id -- comment",
            "id,",
            "id DESC SELECT 1",
            "[open",
            "1 DESC",
        ] {
            assert_eq!(parse_order_by(advanced), None, "{advanced}");
        }
    }

    #[test]
    fn browse_paginates_with_pk_tiebreakers_and_preserves_trusted_projection() {
        let query = TableQuery::default();
        assert_eq!(
            browse_sql("s]", "旅", "*", &[], &["Id".into(), "Tenant".into()], &query, Some((50, 200))).unwrap(),
            "SELECT * FROM [s]]].[旅] ORDER BY [Id], [Tenant] OFFSET 200 ROWS FETCH NEXT 50 ROWS ONLY"
        );
        assert_eq!(
            browse_sql("s", "t", "CONVERT(decimal(19,4), [money]) AS [money]", &[], &[], &query, Some((1, u64::MAX)))
                .unwrap(),
            "SELECT CONVERT(decimal(19,4), [money]) AS [money] FROM [s].[t] ORDER BY (SELECT NULL) OFFSET 18446744073709551615 ROWS FETCH NEXT 1 ROWS ONLY"
        );
        let query = TableQuery { order_by: "[Id] DESC, LOWER([Name])".into(), ..Default::default() };
        assert_eq!(
            browse_sql("s", "t", "*", &[], &["Id".into(), "Tenant".into()], &query, Some((10, 0))).unwrap(),
            "SELECT * FROM [s].[t] ORDER BY [Id] DESC, LOWER([Name])\n, (SELECT [Id]), (SELECT [Tenant]) OFFSET 0 ROWS FETCH NEXT 10 ROWS ONLY"
        );
        let query = TableQuery { order_by: "[t].[Id] DESC".into(), ..Default::default() };
        assert!(
            browse_sql("s", "t", "*", &[], &["Id".into()], &query, Some((10, 0))).unwrap().contains(", (SELECT [Id])")
        );
        assert!(validate_expression("(SELECT [Id])", true).is_err());
        assert_eq!(
            browse_sql("s", "t", "*", &[], &["Id".into()], &query, None).unwrap(),
            "SELECT * FROM [s].[t] ORDER BY [t].[Id] DESC\n, (SELECT [Id])"
        );
        assert_eq!(browse_sql("s", "t", "*", &[], &[], &TableQuery::default(), None).unwrap(), "SELECT * FROM [s].[t]");
    }

    #[test]
    fn count_big_and_column_filters_escape_every_like_metacharacter() {
        let columns = ["na]me", "prefix", "空", "x"].map(|name| ColumnInfo {
            name: name.into(),
            quoted: name.into(),
            type_display: "nvarchar(max)".into(),
            category: TypeCategory::Text,
            attnum: 1,
            writable: true,
            not_null: false,
            pk: false,
            fk: false,
        });
        let query = TableQuery {
            r#where: "[active] = 1 OR [旅] = N'人'".into(),
            filters: vec![
                ColumnFilter { column: "na]me".into(), op: FilterOp::Contains, value: "~%_[x]'旅".into() },
                ColumnFilter { column: "prefix".into(), op: FilterOp::StartsWith, value: "a_b".into() },
                ColumnFilter { column: "空".into(), op: FilterOp::IsNull, value: "ignored".into() },
            ],
            order_by: "ignored".into(),
        };
        assert_eq!(
            count_sql("Sales", "Orders", &columns, &query).unwrap(),
            "SELECT COUNT_BIG(*) FROM [Sales].[Orders] WHERE ([active] = 1 OR [旅] = N'人'\n) AND CONVERT(nvarchar(max), [na]]me]) LIKE N'%~~~%~_~[x]''旅%' ESCAPE N'~' AND CONVERT(nvarchar(max), [prefix]) LIKE N'a~_b%' ESCAPE N'~' AND [空] IS NULL"
        );
        for (op, expected) in [
            (FilterOp::Eq, "="),
            (FilterOp::Ne, "<>"),
            (FilterOp::Lt, "<"),
            (FilterOp::Le, "<="),
            (FilterOp::Gt, ">"),
            (FilterOp::Ge, ">="),
        ] {
            assert_eq!(
                filter_sql(&ColumnFilter { column: "x".into(), op, value: "O'Brien".into() }, &columns).unwrap(),
                format!("[x] {expected} N'O''Brien'")
            );
        }
        assert_eq!(count_sql("s", "t", &[], &TableQuery::default()).unwrap(), "SELECT COUNT_BIG(*) FROM [s].[t]");
    }
}
