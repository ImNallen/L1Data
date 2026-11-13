use super::history::{Token, history_is_write, tokens};
use super::keywords::{NEVER_A_FUNCTION, OTHER_KEYWORDS};
use super::split::split_statements;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameRef {
    pub schema: Option<String>,
    pub name: String,
}

#[derive(Clone, Copy, PartialEq)]
enum Expect {
    No,
    FromItem,
    Target,
}

#[derive(Clone, Copy)]
struct Level {
    expect: Expect,
    in_from: bool,
}

const CLAUSES: [&str; 17] = [
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
    "ON",
];

const RUN_QUERY_VERBS: [&str; 6] = ["SELECT", "WITH", "VALUES", "TABLE", "SHOW", "EXPLAIN"];

/// Whether one statement may run with nobody watching. `READ ONLY` stops writes but not a call
/// that acts outside the transaction, so the answer is closed by default: only a read whose
/// every call is a vetted built-in is `Clear`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Vet {
    Clear,
    NotARead,
    Unvetted(Vec<String>),
}

pub(crate) const VETTED: &[&str] = &[
    "abbrev",
    "abs",
    "acos",
    "acosh",
    "age",
    "any_value",
    "array_agg",
    "array_append",
    "array_cat",
    "array_dims",
    "array_fill",
    "array_length",
    "array_lower",
    "array_ndims",
    "array_position",
    "array_positions",
    "array_prepend",
    "array_remove",
    "array_replace",
    "array_to_json",
    "array_to_string",
    "array_upper",
    "ascii",
    "asin",
    "asinh",
    "atan",
    "atan2",
    "atanh",
    "avg",
    "bit_and",
    "bit_length",
    "bit_or",
    "bit_xor",
    "bool",
    "bool_and",
    "bool_or",
    "bpchar",
    "broadcast",
    "btrim",
    "cardinality",
    "cbrt",
    "ceil",
    "ceiling",
    "char_length",
    "character_length",
    "chr",
    "col_description",
    "concat",
    "concat_ws",
    "convert_from",
    "convert_to",
    "corr",
    "cos",
    "cosh",
    "count",
    "covar_pop",
    "covar_samp",
    "cume_dist",
    "current_database",
    "current_schema",
    "current_schemas",
    "current_setting",
    "date",
    "date_bin",
    "date_part",
    "date_trunc",
    "daterange",
    "decode",
    "degrees",
    "dense_rank",
    "div",
    "encode",
    "every",
    "exp",
    "factorial",
    "family",
    "first_value",
    "float4",
    "float8",
    "floor",
    "format",
    "format_type",
    "gcd",
    "generate_series",
    "generate_subscripts",
    "get_bit",
    "get_byte",
    "has_any_column_privilege",
    "has_column_privilege",
    "has_database_privilege",
    "has_function_privilege",
    "has_schema_privilege",
    "has_sequence_privilege",
    "has_table_privilege",
    "host",
    "hostmask",
    "inet_client_addr",
    "inet_merge",
    "inet_same_family",
    "initcap",
    "int2",
    "int4",
    "int4range",
    "int8",
    "int8range",
    "interval",
    "isempty",
    "isfinite",
    "json_agg",
    "json_array_elements",
    "json_array_elements_text",
    "json_array_length",
    "json_build_array",
    "json_build_object",
    "json_each",
    "json_each_text",
    "json_extract_path",
    "json_extract_path_text",
    "json_object",
    "json_object_agg",
    "json_object_keys",
    "json_populate_record",
    "json_populate_recordset",
    "json_strip_nulls",
    "json_to_record",
    "json_to_recordset",
    "json_typeof",
    "jsonb_agg",
    "jsonb_array_elements",
    "jsonb_array_elements_text",
    "jsonb_array_length",
    "jsonb_build_array",
    "jsonb_build_object",
    "jsonb_each",
    "jsonb_each_text",
    "jsonb_extract_path",
    "jsonb_extract_path_text",
    "jsonb_insert",
    "jsonb_object",
    "jsonb_object_agg",
    "jsonb_object_keys",
    "jsonb_path_exists",
    "jsonb_path_exists_tz",
    "jsonb_path_match",
    "jsonb_path_match_tz",
    "jsonb_path_query",
    "jsonb_path_query_array",
    "jsonb_path_query_array_tz",
    "jsonb_path_query_first",
    "jsonb_path_query_first_tz",
    "jsonb_path_query_tz",
    "jsonb_populate_record",
    "jsonb_populate_recordset",
    "jsonb_pretty",
    "jsonb_set",
    "jsonb_set_lax",
    "jsonb_strip_nulls",
    "jsonb_to_record",
    "jsonb_to_recordset",
    "jsonb_typeof",
    "justify_days",
    "justify_hours",
    "justify_interval",
    "lag",
    "last_value",
    "lcm",
    "lead",
    "left",
    "length",
    "like",
    "ln",
    "log",
    "log10",
    "lower",
    "lower_inc",
    "lower_inf",
    "lpad",
    "ltrim",
    "make_date",
    "make_interval",
    "make_time",
    "make_timestamp",
    "make_timestamptz",
    "masklen",
    "max",
    "md5",
    "min",
    "min_scale",
    "mod",
    "mode",
    "name",
    "netmask",
    "network",
    "now",
    "nth_value",
    "ntile",
    "num_nonnulls",
    "num_nulls",
    "numeric",
    "numrange",
    "obj_description",
    "octet_length",
    "oid",
    "parse_ident",
    "percent_rank",
    "percentile_cont",
    "percentile_disc",
    "pg_backend_pid",
    "pg_char_to_encoding",
    "pg_client_encoding",
    "pg_column_size",
    "pg_conf_load_time",
    "pg_describe_object",
    "pg_encoding_to_char",
    "pg_function_is_visible",
    "pg_get_constraintdef",
    "pg_get_expr",
    "pg_get_function_arguments",
    "pg_get_function_result",
    "pg_get_functiondef",
    "pg_get_indexdef",
    "pg_get_serial_sequence",
    "pg_get_triggerdef",
    "pg_get_userbyid",
    "pg_get_viewdef",
    "pg_has_role",
    "pg_identify_object",
    "pg_postmaster_start_time",
    "pg_size_bytes",
    "pg_size_pretty",
    "pg_table_is_visible",
    "pg_type_is_visible",
    "pg_typeof",
    "phraseto_tsquery",
    "pi",
    "plainto_tsquery",
    "power",
    "quote_ident",
    "quote_literal",
    "quote_nullable",
    "radians",
    "range_agg",
    "range_intersect_agg",
    "range_merge",
    "rank",
    "regclass",
    "regexp_count",
    "regexp_instr",
    "regexp_like",
    "regexp_match",
    "regexp_matches",
    "regexp_replace",
    "regexp_split_to_array",
    "regexp_split_to_table",
    "regexp_substr",
    "regr_avgx",
    "regr_avgy",
    "regr_count",
    "regr_intercept",
    "regr_r2",
    "regr_slope",
    "regr_sxx",
    "regr_sxy",
    "regr_syy",
    "repeat",
    "replace",
    "reverse",
    "right",
    "round",
    "row_number",
    "row_to_json",
    "rpad",
    "rtrim",
    "scale",
    "set_masklen",
    "sha224",
    "sha256",
    "sha384",
    "sha512",
    "shobj_description",
    "sign",
    "sin",
    "sinh",
    "split_part",
    "sqrt",
    "starts_with",
    "statement_timestamp",
    "stddev",
    "stddev_pop",
    "stddev_samp",
    "string_agg",
    "string_to_array",
    "string_to_table",
    "strpos",
    "substr",
    "sum",
    "tan",
    "tanh",
    "text",
    "time",
    "timestamp",
    "timestamptz",
    "timetz",
    "timezone",
    "to_ascii",
    "to_char",
    "to_date",
    "to_hex",
    "to_json",
    "to_jsonb",
    "to_number",
    "to_regclass",
    "to_regnamespace",
    "to_regproc",
    "to_regrole",
    "to_regtype",
    "to_timestamp",
    "to_tsquery",
    "to_tsvector",
    "transaction_timestamp",
    "translate",
    "trim_array",
    "trim_scale",
    "trunc",
    "ts_headline",
    "ts_rank",
    "ts_rank_cd",
    "tsrange",
    "tstzrange",
    "unistr",
    "unnest",
    "upper",
    "upper_inc",
    "upper_inf",
    "var_pop",
    "var_samp",
    "varchar",
    "variance",
    "version",
    "websearch_to_tsquery",
    "width_bucket",
];

/// Size helpers, vetted although volatile: they read file sizes and change nothing.
pub(crate) const VETTED_SIZES: &[&str] = &[
    "pg_database_size",
    "pg_indexes_size",
    "pg_relation_size",
    "pg_table_size",
    "pg_tablespace_size",
    "pg_total_relation_size",
];

/// The built-in operator symbols an analytics query uses. An operator is a function, so one off
/// this list asks like an unlisted call.
pub(crate) const VETTED_OPERATORS: &[&str] = &[
    "!~", "!~*", "!~~", "!~~*", "#>", "#>>", "%", "&&", "*", "+", "-", "->", "->>", "/", "<", "<=", "<>", "<@", "=",
    ">", ">=", "?", "?&", "?|", "@>", "@@", "^", "||", "~", "~*", "~~", "~~*",
];

/// Unreserved words that are syntax before `(` only after these words, nearest first. Anywhere
/// else they may be a function the user defined, so they are judged like any other name.
const SYNTAX_AFTER: &[(&str, &[&str])] = &[
    ("by", &["group"]),
    ("by", &["order"]),
    ("by", &["partition"]),
    ("sets", &["grouping"]),
    ("join", &["cross"]),
    ("join", &["inner"]),
    ("join", &["outer"]),
    ("join", &["left"]),
    ("join", &["right"]),
    ("join", &["full"]),
    ("join", &["natural"]),
    ("varying", &["character"]),
    ("zone", &["time"]),
    ("system", &["tablesample"]),
    ("bernoulli", &["tablesample"]),
];

const GROUP_BY_ENDS: [&str; 10] =
    ["having", "window", "order", "limit", "offset", "fetch", "for", "union", "intersect", "except"];

#[derive(Clone, Copy, Default)]
struct Clause {
    cte_list: bool,
    group_by: bool,
}

fn part(t: Option<&Token<'_>>) -> Option<String> {
    match t? {
        Token::Word(w) => Some(w.to_ascii_lowercase()),
        Token::Quoted(q) => Some((*q).replace("\"\"", "\"")),
        _ => None,
    }
}

fn is_word(t: Option<&Token<'_>>, expected: &str) -> bool {
    matches!(t, Some(Token::Word(w)) if w.eq_ignore_ascii_case(expected))
}

fn is_keyword(word: &str) -> bool {
    NEVER_A_FUNCTION.binary_search(&word).is_ok() || OTHER_KEYWORDS.binary_search(&word).is_ok()
}

fn close_of(tokens: &[Token<'_>], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, t) in tokens.iter().enumerate().skip(open) {
        match t {
            Token::Open => depth += 1,
            Token::Close => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

pub fn relation_names(sql: &str) -> Vec<NameRef> {
    let toks = tokens(sql);
    let first = match toks.first() {
        Some(Token::Word(w)) => w.to_ascii_uppercase(),
        _ => String::new(),
    };
    let mut levels = vec![Level { expect: Expect::No, in_from: false }];
    let mut ctes: Vec<(String, usize)> = Vec::new();
    let mut found: Vec<(NameRef, usize, Expect)> = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        let level = levels.last_mut().expect("level 0 is never popped");
        match &toks[i] {
            Token::Open => {
                level.expect = Expect::No;
                levels.push(Level { expect: Expect::No, in_from: false });
            }
            Token::Close => {
                if levels.len() > 1 {
                    levels.pop();
                }
            }
            Token::Comma => {
                if level.in_from && level.expect == Expect::No {
                    level.expect = Expect::FromItem;
                }
            }
            Token::Word(w) => {
                let up = w.to_ascii_uppercase();
                match up.as_str() {
                    "FROM" if is_word(i.checked_sub(1).and_then(|p| toks.get(p)), "DISTINCT") => {}
                    "FROM" | "JOIN" => {
                        level.expect = Expect::FromItem;
                        level.in_from = true;
                    }
                    "INTO" | "UPDATE" | "TABLE" => level.expect = Expect::Target,
                    "USING" if first == "DELETE" || first == "MERGE" => {
                        level.expect = Expect::FromItem;
                        level.in_from = true;
                    }
                    "AS" => {
                        let mut j = i + 1;
                        while is_word(toks.get(j), "NOT") || is_word(toks.get(j), "MATERIALIZED") {
                            j += 1;
                        }
                        if toks.get(j) == Some(&Token::Open)
                            && let Some(close) = close_of(&toks, j)
                        {
                            let before = i.saturating_sub(1);
                            let name_at = if toks[before] == Token::Close {
                                toks[..before].iter().rposition(|t| *t == Token::Open).and_then(|o| o.checked_sub(1))
                            } else {
                                Some(before)
                            };
                            if let Some(name) = name_at.and_then(|n| part(toks.get(n))) {
                                ctes.push((name, close));
                            }
                        }
                    }
                    _ if CLAUSES.contains(&up.as_str()) => {
                        level.in_from = false;
                        level.expect = Expect::No;
                    }
                    "ONLY" | "LATERAL" if level.expect != Expect::No => {}
                    _ if level.expect != Expect::No => {
                        let expect = level.expect;
                        level.expect = Expect::No;
                        i = take_name(&toks, i, expect, &mut found);
                    }
                    _ => {}
                }
            }
            Token::Quoted(_) if level.expect != Expect::No => {
                let expect = level.expect;
                level.expect = Expect::No;
                i = take_name(&toks, i, expect, &mut found);
            }
            _ => {}
        }
        i += 1;
    }
    let mut out: Vec<NameRef> = Vec::new();
    for (name, at, expect) in found {
        let mut after = at + 1;
        while toks.get(after) == Some(&Token::Dot) {
            after += 2;
        }
        let function_call = expect == Expect::FromItem && toks.get(after) == Some(&Token::Open);
        let shadowed = name.schema.is_none() && ctes.iter().any(|(cte, close)| *cte == name.name && *close < at);
        if !function_call && !shadowed && !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

fn take_name(toks: &[Token<'_>], mut i: usize, expect: Expect, found: &mut Vec<(NameRef, usize, Expect)>) -> usize {
    let at = i;
    let mut parts = vec![part(toks.get(i)).expect("a word or a quoted identifier")];
    while toks.get(i + 1) == Some(&Token::Dot)
        && let Some(next) = part(toks.get(i + 2))
    {
        parts.push(next);
        i += 2;
    }
    let name = parts.pop().expect("one part");
    found.push((NameRef { schema: parts.pop(), name }, at, expect));
    i
}

/// The one gate for running without a user. `Clear` needs a read statement, no `U&` escape (its
/// `\` sequences can spell any name), no `SELECT … INTO`, every called function on `VETTED`, and
/// every operator on `VETTED_OPERATORS`.
pub fn vet(sql: &str) -> Vet {
    if !is_read_statement(sql) {
        return Vet::NotARead;
    }
    let toks = tokens(sql);
    let mut items: Vec<String> = Vec::new();
    let mut note = |item: String| {
        if !items.contains(&item) {
            items.push(item);
        }
    };
    let mut levels = vec![Clause::default()];
    for (i, t) in toks.iter().enumerate() {
        let word = match t {
            Token::Open => {
                levels.push(Clause::default());
                continue;
            }
            Token::Close => {
                if levels.len() > 1 {
                    levels.pop();
                }
                continue;
            }
            Token::UnicodeEscape => {
                note("U& escape".into());
                continue;
            }
            Token::Operator(op) => {
                if let Some(call) = operator_at(&toks, i, op)
                    && !call.vetted
                {
                    note(format!("operator {}", call.name));
                }
                continue;
            }
            Token::Word(w) => Some(w.to_ascii_lowercase()),
            _ => None,
        };
        let level = levels.last_mut().expect("level 0 is never popped");
        match word.as_deref() {
            Some("into") => note("SELECT INTO".into()),
            Some("with") if i == 0 || toks[i - 1] == Token::Open => level.cte_list = true,
            Some("select" | "values" | "table") => level.cte_list = false,
            Some("group") if is_word(toks.get(i + 1), "by") => level.group_by = true,
            Some(w) if GROUP_BY_ENDS.contains(&w) => level.group_by = false,
            _ => {}
        }
        if toks.get(i + 1) == Some(&Token::Open)
            && let Some(call) = call_at(&toks, i, *level)
            && !call.vetted
        {
            note(call.name);
        }
    }
    if items.is_empty() { Vet::Clear } else { Vet::Unvetted(items) }
}

struct Call {
    name: String,
    vetted: bool,
}

fn call_at(toks: &[Token<'_>], i: usize, clause: Clause) -> Option<Call> {
    let name = part(toks.get(i))?;
    let mut parts = vec![name.clone()];
    let mut first = i;
    while first >= 2
        && toks[first - 1] == Token::Dot
        && let Some(schema) = part(toks.get(first - 2))
    {
        parts.insert(0, schema);
        first -= 2;
    }
    let written = parts.join(".");
    if parts.len() > 2 || (parts.len() == 2 && parts[0] != "pg_catalog") {
        return Some(Call { name: written, vetted: false });
    }
    let unquoted = matches!(toks[i], Token::Word(_));
    let grouping = clause.group_by && matches!(name.as_str(), "rollup" | "cube");
    let operator_syntax = name == "operator" && operator_in(toks, i + 1);
    if parts.len() == 1
        && (clause.cte_list
            || (unquoted && (grouping || operator_syntax || NEVER_A_FUNCTION.binary_search(&name.as_str()).is_ok()))
            || syntax_before(toks, i, &name))
    {
        return None;
    }
    let vetted = VETTED.binary_search(&name.as_str()).is_ok() || VETTED_SIZES.contains(&name.as_str());
    Some(Call { name: written, vetted })
}

/// Whether the `(` at `open` starts `OPERATOR(op)` or `OPERATOR(schema.op)`.
fn operator_in(toks: &[Token<'_>], open: usize) -> bool {
    match toks.get(open + 1) {
        Some(Token::Operator(_)) => true,
        Some(Token::Word(_) | Token::Quoted(_)) => {
            toks.get(open + 2) == Some(&Token::Dot) && matches!(toks.get(open + 3), Some(Token::Operator(_)))
        }
        _ => false,
    }
}

/// The operator at `i`, judged like a call: a bare symbol by `VETTED_OPERATORS`, and
/// `OPERATOR(schema.op)` only for `pg_catalog`. The `*` after a dot is a column list, not an operator.
fn operator_at(toks: &[Token<'_>], i: usize, op: &str) -> Option<Call> {
    let listed = VETTED_OPERATORS.binary_search(&op).is_ok();
    if i == 0 || toks[i - 1] != Token::Dot {
        return Some(Call { name: op.to_owned(), vetted: listed });
    }
    let qualified = i >= 4 && toks[i - 3] == Token::Open && is_word(toks.get(i - 4), "operator");
    if !qualified && op == "*" {
        return None;
    }
    let schema = i.checked_sub(2).and_then(|s| part(toks.get(s))).unwrap_or_default();
    Some(Call { name: format!("{schema}.{op}"), vetted: listed && schema == "pg_catalog" })
}

fn syntax_before(toks: &[Token<'_>], i: usize, name: &str) -> bool {
    let Some(before) = i.checked_sub(1) else { return true };
    match &toks[before] {
        Token::Close | Token::Quoted(_) => true,
        Token::Word(w) if w.eq_ignore_ascii_case("as") => true,
        Token::Word(w) => {
            let listed = SYNTAX_AFTER.iter().any(|(n, words)| {
                *n == name
                    && words
                        .iter()
                        .enumerate()
                        .all(|(k, word)| is_word(i.checked_sub(k + 1).and_then(|j| toks.get(j)), word))
            });
            listed || !is_keyword(&w.to_ascii_lowercase())
        }
        _ => false,
    }
}

pub fn is_read_statement(sql: &str) -> bool {
    let spans = split_statements(sql);
    let [span] = spans.as_slice() else { return false };
    let stmt = span.text(sql);
    let toks = tokens(stmt);
    let leads_with_read = match toks.first() {
        Some(Token::Word(w)) => RUN_QUERY_VERBS.iter().any(|v| w.eq_ignore_ascii_case(v)),
        Some(Token::Open) => true,
        _ => false,
    };
    leads_with_read && !history_is_write(stmt)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(sql: &str) -> Vec<String> {
        relation_names(sql)
            .into_iter()
            .map(|r| match r.schema {
                Some(s) => format!("{s}.{}", r.name),
                None => r.name,
            })
            .collect()
    }

    #[test]
    fn the_mockup_query_uses_orders_and_customers_not_its_aliases() {
        let sql = "select c.region, count(*) as orders, sum(o.total) as revenue\nfrom orders o join customers c on c.id = o.customer_id\nwhere o.status = 'paid' and o.created_at > now() - interval '90 days'\ngroup by c.region order by revenue desc";
        assert_eq!(names(sql), ["orders", "customers"]);
    }

    #[test]
    fn schema_qualified_quoted_and_comma_lists() {
        assert_eq!(
            names(r#"select * from public.orders o, "Order Items" i, shop."Odd""Name" n"#),
            ["public.orders", "Order Items", "shop.Odd\"Name"]
        );
        assert_eq!(names("select 1 from ONLY orders"), ["orders"]);
        assert_eq!(names("table lab.type_zoo"), ["lab.type_zoo"]);
    }

    #[test]
    fn functions_subselects_and_cte_names_are_not_relations() {
        assert_eq!(names("select * from generate_series(1, 3) g, orders"), ["orders"]);
        assert_eq!(names("select * from (select * from orders) q join customers using (id)"), ["orders", "customers"]);
        assert_eq!(
            names("with orders as (select * from orders where total > 1) select * from orders join customers on true"),
            ["orders", "customers"],
            "the inner FROM is the table, the outer one is the CTE"
        );
        assert_eq!(names("with recent as (select * from orders) select * from recent"), ["orders"]);
        assert_eq!(
            names(
                "with recent(id) as materialized (select id from orders) select * from recent r join customers c on c.id = r.id"
            ),
            ["orders", "customers"]
        );
    }

    #[test]
    fn writes_and_distinct_from() {
        assert_eq!(names("insert into orders (id) values (1)"), ["orders"]);
        assert_eq!(
            names("update orders set total = 1 from customers c where c.id = orders.customer_id"),
            ["orders", "customers"]
        );
        assert_eq!(names("delete from orders using customers c where true"), ["orders", "customers"]);
        assert_eq!(names("select a is distinct from b from orders"), ["orders"]);
        assert_eq!(names("select * from a left outer join lateral (select 1) x on true cross join b"), ["a", "b"]);
    }

    #[test]
    fn strings_and_comments_hide_names() {
        assert_eq!(names("select 'from fake' from real -- from commented\n/* join nope */"), ["real"]);
        assert_eq!(names("select 1"), Vec::<String>::new());
    }

    fn unvetted(items: &[&str]) -> Vet {
        Vet::Unvetted(items.iter().map(|s| (*s).to_owned()).collect())
    }

    #[test]
    fn escapes_side_effects_and_select_into_each_ask_and_name_what_they_use() {
        assert_eq!(vet(r#"select U&"pg\005fcancel_backend"(0)"#), unvetted(&["U& escape", r"pg\005fcancel_backend"]));
        assert_eq!(vet(r#"select U&"\0064ate_trunc"('day', now())"#), unvetted(&["U& escape", r"\0064ate_trunc"]));
        assert_eq!(vet(r"select u&'\0041'"), unvetted(&["U& escape"]));
        assert_eq!(vet("select query_to_xml('select 1', true, false, '')"), unvetted(&["query_to_xml"]));
        assert_eq!(vet("select pg_stat_reset()"), unvetted(&["pg_stat_reset"]));
        assert_eq!(vet("select pg_file_write('/tmp/x', 'y', false)"), unvetted(&["pg_file_write"]));
        assert_eq!(vet("select pg_terminate_backend(1)"), unvetted(&["pg_terminate_backend"]));
        assert_eq!(vet("select * into backup from orders"), unvetted(&["SELECT INTO"]));
        assert_eq!(
            vet(
                "select set_config('a', 'b', false), pg_sleep(1), nextval('s'), dblink_exec('x', 'y'), pg_advisory_lock(1)"
            ),
            unvetted(&["set_config", "pg_sleep", "nextval", "dblink_exec", "pg_advisory_lock"])
        );
        assert_eq!(
            vet("select count(*), pg_sleep(1), pg_sleep(2) from orders"),
            unvetted(&["pg_sleep"]),
            "each name once"
        );
    }

    #[test]
    fn user_schemas_quoted_names_and_unknown_names_are_unvetted_but_pg_catalog_is_judged_by_name() {
        assert_eq!(
            vet("select public.total(1), lab.\"Score\"(2), a.b.c(3)"),
            unvetted(&["public.total", "lab.Score", "a.b.c"])
        );
        assert_eq!(
            vet(r#"select "COUNT"(*), "my func"(1) from orders"#),
            unvetted(&["COUNT", "my func"]),
            "a quoted name is matched verbatim"
        );
        assert_eq!(
            vet(
                "select pg_catalog.count(*), PG_CATALOG.date_trunc('day', now()), \"pg_catalog\".\"sum\"(total) from orders"
            ),
            Vet::Clear
        );
        assert_eq!(vet("select pg_catalog.pg_terminate_backend(1)"), unvetted(&["pg_catalog.pg_terminate_backend"]));
        assert_eq!(vet("select my_func(1) from t"), unvetted(&["my_func"]));
        assert_eq!(
            vet("select * from t cross join pg_sleep(1)"),
            unvetted(&["pg_sleep"]),
            "a function after JOIN is a call"
        );
        assert_eq!(vet("select * from t where x like pg_sleep(1)::text"), unvetted(&["pg_sleep"]));
        assert_eq!(vet("select * from t order by pg_sleep(1)"), unvetted(&["pg_sleep"]));
        assert_eq!(
            vet("select rollup(1) from t group by 1 order by cube(2)"),
            unvetted(&["rollup", "cube"]),
            "outside GROUP BY, rollup and cube may be the user's functions"
        );
        assert_eq!(
            vet("select 1 from t group by a, rollup (b), (select cube(1))"),
            unvetted(&["cube"]),
            "a subquery inside GROUP BY is its own level"
        );
        assert_eq!(vet("select data.join(1) from t data"), unvetted(&["data.join"]));
        assert_eq!(
            vet("select 1 from t where name ilike ('%y%')"),
            unvetted(&["ilike"]),
            "a keyword used as a column hides the operator before it, so this asks"
        );
    }

    #[test]
    fn reads_calling_only_vetted_builtins_are_clear() {
        let mockup = "select c.region, count(*) as orders, sum(o.total) as revenue\nfrom orders o join customers c on c.id = o.customer_id\nwhere o.status = 'paid' and o.created_at > now() - interval '90 days'\ngroup by c.region order by revenue desc";
        assert_eq!(vet(mockup), Vet::Clear);
        let card = "SELECT c.region,\n  sum(o.total) FILTER (\n    WHERE o.created_at >= now() - interval '90 days'\n  ) - sum(o.total) FILTER (\n    WHERE o.created_at <  now() - interval '90 days'\n  ) AS growth\nFROM orders o\nJOIN customers c ON c.id = o.customer_id\nWHERE o.status = 'paid'\nGROUP BY c.region\nORDER BY growth DESC;";
        assert_eq!(vet(card), Vet::Clear);
        for sql in [
            "select date_trunc('month', created_at) m, count(*), sum(total)::numeric(12, 2), avg(total)::float(8), string_agg(number, ',' order by number), coalesce(max(total), 0), nullif(min(total), 0), greatest(1, 2), least(1, 2) from orders group by 1",
            "select row_number() over (partition by customer_id order by created_at desc), rank() over w, lag(total) over (order by id rows between 1 preceding and current row) from orders window w as (order by id)",
            "select extract(epoch from created_at), cast(total as varchar(10)), created_at::timestamp(0), total::character varying(20), substring(number from 1 for 3), position('a' in number), trim(both 'x' from number), overlay(number placing 'z' from 1), created_at at time zone ('UTC') from orders",
            "with recursive t(n) as (select 1 union all select n + 1 from t where n < 3), u(m) as not materialized (select 2) select n, m from t cross join (select * from u) q",
            "select * from generate_series(1, 3) as g(n) join (select 1 as n) s on s.n = g.n, unnest(array[1, 2]) with ordinality as a(v, i) tablesample system (10) repeatable (1)",
            "select region, count(*) from customers group by rollup (region), cube (region), grouping sets ((region), ())",
            "select * from orders o(a, b, c) where (a, b) overlaps (c, a) and exists (select 1) and a in (1, 2) and a = any (array[1]) and a between (1) and (2) and email like ('%x%') and email ilike ('%y%')",
            "select pg_size_pretty(pg_total_relation_size('orders')), pg_relation_size('orders'), current_database(), current_schema(), version(), pg_typeof(1), current_setting('search_path'), current_timestamp(3), localtime(0)",
            "select to_json(o), jsonb_build_object('a', 1) -> 'a', array_length(array[1], 1), lower(name), left(name, 2), right(name, 2), to_char(created_at, 'YYYY'), (select max(id) from orders) from orders o",
            "explain (analyze false, format json) select count(*) from orders",
            "values (1), (2)",
            "table orders",
            "show search_path",
        ] {
            assert_eq!(vet(sql), Vet::Clear, "{sql}");
        }
    }

    #[test]
    fn operators_are_judged_like_calls() {
        assert_eq!(
            vet("select 1 ## 2"),
            unvetted(&["operator ##"]),
            "an operator is a function the user may have defined"
        );
        assert_eq!(vet("select a operator(public.##) b from t"), unvetted(&["operator public.##"]));
        assert_eq!(
            vet("select a operator(pg_catalog.##) b, c operator(pg_catalog.+) d, e operator(+) f from t"),
            unvetted(&["operator pg_catalog.##"]),
            "OPERATOR() is judged by its symbol like a pg_catalog call"
        );
        assert_eq!(vet("select x |> y, 1 +-- a comment ends the operator\n 2 from t"), unvetted(&["operator |>"]));
        assert_eq!(vet("select a <-> b, a <-> c from t"), unvetted(&["operator <->"]), "each symbol once");
        for sql in [
            "select c.region, count(*) as orders, sum(o.total) as revenue\nfrom orders o join customers c on c.id = o.customer_id\nwhere o.status = 'paid' and o.created_at > now() - interval '90 days'\ngroup by c.region order by revenue desc",
            "select 1 + 2 * 3 - 4 / 5 % 6 ^ 7, -1, +1, 2+-1, 2*-1, a>-1 from t",
            "select * from t where a >= b and a <> c and a != d and a <= e and a = f and a < g and a > h",
            "select x || y, s ~~ 'a%', s ~~* 'A%', s !~~ 'b', s !~~* 'B', s ~ '^x', s ~* '^X', s !~ 'y', s !~* 'Y' from t",
            "select j->>'k', j -> 'k', j #> '{a}', j #>> '{a}', j @> '{}', j <@ '{}', j ? 'k', j ?| array['a'], j ?& array['a'], v @@ to_tsquery('x') from t",
            "select arr @> array[1], array[1] <@ arr, arr && array[2], r @> 1, 1 <@ r from t",
            "select t.*, count(t.*), make_interval(days => 1), total::numeric(12, 2) from t",
        ] {
            assert_eq!(vet(sql), Vet::Clear, "{sql}");
        }
    }

    #[test]
    fn writes_are_not_reads_before_any_vetting() {
        assert_eq!(vet("delete from orders"), Vet::NotARead);
        assert_eq!(vet("create table backup as select * from orders"), Vet::NotARead);
        assert_eq!(vet("copy orders to '/tmp/x'"), Vet::NotARead);
        assert_eq!(vet("with x as (delete from t returning *) select * from x"), Vet::NotARead);
        assert_eq!(vet("select 1; select pg_sleep(1)"), Vet::NotARead);
        assert_eq!(vet(""), Vet::NotARead);
    }

    #[test]
    fn the_lists_are_sorted_for_the_binary_search() {
        for (name, list) in [
            ("VETTED", VETTED),
            ("VETTED_SIZES", VETTED_SIZES),
            ("VETTED_OPERATORS", VETTED_OPERATORS),
            ("NEVER_A_FUNCTION", NEVER_A_FUNCTION),
            ("OTHER_KEYWORDS", OTHER_KEYWORDS),
            ("RESERVED", super::super::keywords::RESERVED),
            ("TYPE_OR_FUNCTION_NAME", super::super::keywords::TYPE_OR_FUNCTION_NAME),
        ] {
            assert!(list.windows(2).all(|w| w[0] < w[1]), "{name} is not sorted and unique");
        }
        assert!(
            SYNTAX_AFTER.iter().all(|(name, _)| !NEVER_A_FUNCTION.contains(name)),
            "SYNTAX_AFTER is for words that may also be function names"
        );
    }

    #[test]
    fn read_statements_are_single_select_like_statements_without_a_write() {
        for sql in [
            "select 1",
            "  -- note\n select * from orders;",
            "WITH x AS (select 1) select * from x",
            "(select 1) union (select 2)",
            "table orders",
            "values (1), (2)",
            "show search_path",
            "explain select 1",
            "explain (analyze false) delete from t",
            "explain analyze select pg_sleep(1)",
        ] {
            assert!(is_read_statement(sql), "{sql}");
        }
        for sql in [
            "delete from t",
            "insert into t values (1)",
            "update t set a = 1",
            "with x as (delete from t returning *) select * from x",
            "explain analyze delete from t",
            "select 1; select 2",
            "select 1\n\nselect 2",
            "commit",
            "set transaction_read_only = off",
            "copy orders to stdout",
            "fetch 1 from c",
            "",
            "-- only a comment",
            "create table t (id int)",
            "truncate t",
            "do $$ begin end $$",
        ] {
            assert!(!is_read_statement(sql), "{sql}");
        }
    }
}
