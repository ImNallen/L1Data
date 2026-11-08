use std::collections::{HashMap, HashSet};
use std::ops::Range;

use crate::model::{DatabaseKind, DocRange, Param, ParamValue, RunError, RunTarget};
use crate::sql::lex::{self, LexKind, Strings};
use crate::sql::{self, StmtSpan};
use crate::text::{self, OffsetMap};

pub(crate) struct ExecutableScript {
    pub text: String,
    pub spans: Vec<StmtSpan>,
    pub ranges: Vec<DocRange>,
    pub changed: Vec<bool>,
}

fn unsupported(what: impl Into<String>) -> RunError {
    RunError::Unsupported { what: what.into() }
}

fn selected(text: &str, target: RunTarget, kind: DatabaseKind) -> Result<Vec<StmtSpan>, RunError> {
    text::pick(text, OffsetMap::new(text).target(target), kind).map_err(unsupported)
}

fn pg_occurrences(text: &str, strings: Strings) -> Result<Vec<Range<usize>>, RunError> {
    let mut tokens = lex::lexemes(text, strings).peekable();
    let mut out = Vec::new();
    let mut brackets = Vec::new();
    let mut previous = None;
    let mut prefix = [""; 4];
    let mut prefix_len = 0;
    while let Some(token) = tokens.next() {
        if matches!(
            token.kind,
            LexKind::Space | LexKind::LineComment | LexKind::BlockComment
        ) {
            continue;
        }
        if prefix_len < prefix.len() {
            prefix[prefix_len] = if token.kind == LexKind::Word {
                &text[token.span.clone()]
            } else {
                ""
            };
            prefix_len += 1;
        }
        match token.kind {
            LexKind::OpenBracket => {
                let constructor = previous.as_ref().is_some_and(|p: &lex::Lexeme| {
                    p.kind == LexKind::Word && text[p.span.clone()].eq_ignore_ascii_case("array")
                });
                brackets.push(constructor);
            }
            LexKind::CloseBracket => {
                brackets.pop();
            }
            LexKind::Param => {
                let declaration = if prefix[1].eq_ignore_ascii_case("OR")
                    && prefix[2].eq_ignore_ascii_case("REPLACE")
                {
                    prefix[3]
                } else {
                    prefix[1]
                };
                let server_owned = prefix[0].eq_ignore_ascii_case("PREPARE")
                    || (prefix[0].eq_ignore_ascii_case("CREATE")
                        && (declaration.eq_ignore_ascii_case("FUNCTION")
                            || declaration.eq_ignore_ascii_case("PROCEDURE")));
                if !server_owned {
                    let name = &text[token.span.clone()];
                    if name[1..].parse::<u32>().ok().filter(|n| *n > 0).is_none() {
                        return Err(unsupported(format!(
                            "Invalid parameter {name}. Use a positive positional index such as $1."
                        )));
                    }
                    out.push(token.span.clone());
                }
            }
            LexKind::Other
                if &text[token.span.clone()] == ":"
                    && brackets.iter().all(|constructor| *constructor) =>
            {
                if let Some(next) = tokens
                    .peek()
                    .filter(|next| next.kind == LexKind::Word && token.span.end == next.span.start)
                {
                    out.push(token.span.start..next.span.end);
                }
            }
            _ => {}
        }
        previous = Some(token);
    }
    Ok(out)
}

fn occurrences(text: &str, kind: DatabaseKind) -> Result<Vec<Range<usize>>, RunError> {
    match kind {
        DatabaseKind::SqlServer if !text.contains('@') => Ok(Vec::new()),
        DatabaseKind::SqlServer => sql::tsql::param_occurrences(text).map_err(unsupported),
        DatabaseKind::Postgres if !text.contains([':', '$']) => Ok(Vec::new()),
        DatabaseKind::Postgres => {
            let standard = pg_occurrences(text, Strings::Standard)?;
            if !standard.is_empty()
                && text.contains('\\')
                && standard != pg_occurrences(text, Strings::Escaped)?
            {
                return Err(unsupported(
                    "Parameter locations depend on standard_conforming_strings. Use E'...' strings with explicit backslash escaping.",
                ));
            }
            Ok(standard)
        }
    }
}

fn scan(
    text: &str,
    spans: &[StmtSpan],
    kind: DatabaseKind,
) -> Result<Vec<Vec<Range<usize>>>, RunError> {
    spans
        .iter()
        .map(|span| occurrences(span.text(text), kind))
        .collect()
}

pub(crate) fn discover(
    text: &str,
    target: RunTarget,
    kind: DatabaseKind,
) -> Result<Vec<Param>, RunError> {
    let spans = selected(text, target, kind)?;
    let scanned = scan(text, &spans, kind)?;
    let mut seen = HashSet::new();
    let mut params = Vec::new();
    for (span, occurrences) in spans.iter().zip(scanned) {
        let unit = span.text(text);
        for range in occurrences {
            let name = &unit[range];
            if seen.insert(name.to_owned()) {
                params.push(Param {
                    name: name.to_owned(),
                });
            }
        }
    }
    Ok(params)
}

fn literal(value: Option<&str>, kind: DatabaseKind) -> String {
    match value {
        None => "NULL".into(),
        Some(value) => match kind {
            DatabaseKind::Postgres => text::quote_escaped_literal(value),
            DatabaseKind::SqlServer => format!("N'{}'", value.replace('\'', "''")),
        },
    }
}

pub(crate) fn bind(
    text: String,
    target: RunTarget,
    kind: DatabaseKind,
    values: Vec<ParamValue>,
) -> Result<ExecutableScript, RunError> {
    let spans = selected(&text, target, kind)?;
    let scanned = scan(&text, &spans, kind)?;
    let map = OffsetMap::new(&text);
    let ranges = spans.iter().map(|span| map.range(span)).collect();
    let mut supplied = HashMap::new();
    for param in values {
        if param.value.as_ref().is_some_and(|v| v.contains('\0')) {
            return Err(unsupported(format!(
                "Parameter {} contains NUL.",
                param.name
            )));
        }
        if supplied.insert(param.name.clone(), param.value).is_some() {
            return Err(unsupported(format!("Duplicate parameter {}.", param.name)));
        }
    }
    let expected: HashSet<&str> = spans
        .iter()
        .zip(&scanned)
        .flat_map(|(span, found)| found.iter().map(|r| &span.text(&text)[r.clone()]))
        .collect();
    for name in &expected {
        if !supplied.contains_key(*name) {
            return Err(unsupported(format!("Missing value for parameter {name}.")));
        }
    }
    for name in supplied.keys() {
        if !expected.contains(name.as_str()) {
            return Err(unsupported(format!("Unexpected parameter {name}.")));
        }
    }
    if expected.is_empty() {
        let changed = vec![false; spans.len()];
        return Ok(ExecutableScript {
            text,
            spans,
            ranges,
            changed,
        });
    }
    let mut executable = String::with_capacity(text.len());
    let mut adjusted = Vec::with_capacity(spans.len());
    let mut changed = Vec::with_capacity(spans.len());
    for (span, found) in spans.iter().zip(scanned) {
        let unit = span.text(&text);
        let start = executable.len();
        let mut at = 0;
        changed.push(!found.is_empty());
        for occurrence in found {
            executable.push_str(&unit[at..occurrence.start]);
            let name = &unit[occurrence.clone()];
            executable.push_str(&literal(supplied[name].as_deref(), kind));
            at = occurrence.end;
        }
        executable.push_str(&unit[at..]);
        adjusted.push(StmtSpan {
            start,
            end: executable.len(),
        });
        executable.push('\n');
    }
    Ok(ExecutableScript {
        text: executable,
        spans: adjusted,
        ranges,
        changed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn names(text: &str, kind: DatabaseKind) -> Vec<String> {
        discover(text, RunTarget::All, kind)
            .unwrap()
            .into_iter()
            .map(|p| p.name)
            .collect()
    }
    #[test]
    fn postgres_discovery_preserves_sql_syntax() {
        assert_eq!(
            names(
                "select :x::int, $1, :x, ARRAY[:items], arr[lo:hi], arr[:hi], ':no', E':no', $$:no$$, \"$2\" -- :no\n",
                DatabaseKind::Postgres
            ),
            [":x", "$1", ":items"]
        );
        for sql in ["select $0", "select $1abc", "select $999999999999999999"] {
            assert!(discover(sql, RunTarget::All, DatabaseKind::Postgres).is_err());
        }
        assert!(
            discover(
                r"select '\' :x ' :y",
                RunTarget::All,
                DatabaseKind::Postgres
            )
            .is_err()
        );
    }

    #[test]
    fn parameter_free_strings_keep_their_original_sql() {
        for source in [
            r"SELECT '\' AS slash, '$0' AS txt",
            r"SELECT '\' AS slash, '$1' AS txt",
        ] {
            let found = discover(source, RunTarget::All, DatabaseKind::Postgres);
            assert!(
                found.as_ref().is_ok_and(Vec::is_empty),
                "parameter-free script was rejected: {found:?}"
            );
            let bound = bind(
                source.into(),
                RunTarget::All,
                DatabaseKind::Postgres,
                Vec::new(),
            )
            .unwrap();
            assert_eq!(bound.text, source);
            assert_eq!(bound.changed, [false]);
        }
    }

    #[test]
    fn postgres_declarations_own_their_positional_parameters() {
        for source in [
            "PREPARE q(int) AS SELECT $1; EXECUTE q(2)",
            "CREATE FUNCTION f(int) RETURNS int LANGUAGE SQL RETURN $1",
            "/* header */ create or replace function f(int) returns int language sql return $1",
            "CREATE PROCEDURE p(int) LANGUAGE SQL AS $$SELECT $1$$",
        ] {
            assert_eq!(names(source, DatabaseKind::Postgres), Vec::<String>::new());
            let bound = bind(
                source.into(),
                RunTarget::All,
                DatabaseKind::Postgres,
                Vec::new(),
            )
            .unwrap();
            assert_eq!(bound.text, source);
        }
        let source = "PREPARE q(int) AS SELECT $1; SELECT $1::int";
        assert_eq!(names(source, DatabaseKind::Postgres), ["$1"]);
        let bound = bind(
            source.into(),
            RunTarget::All,
            DatabaseKind::Postgres,
            vec![ParamValue {
                name: "$1".into(),
                value: Some("7".into()),
            }],
        )
        .unwrap();
        assert_eq!(
            bound.spans[0].text(&bound.text),
            "PREPARE q(int) AS SELECT $1"
        );
        assert_eq!(bound.spans[1].text(&bound.text), "SELECT E'7'::int");
        assert_eq!(
            names(
                "PREPARE q(int) AS SELECT $1 + :extra::int",
                DatabaseKind::Postgres
            ),
            [":extra"]
        );
    }

    #[test]
    fn leading_semicolons_preserve_routine_parameters() {
        for source in [
            ";CREATE PROCEDURE dbo.p @x int AS SELECT @x",
            ";;;CREATE OR ALTER FUNCTION dbo.f(@x int) RETURNS int AS BEGIN RETURN @x END",
        ] {
            assert_eq!(names(source, DatabaseKind::SqlServer), Vec::<String>::new());
        }
    }
    #[test]
    fn binding_keeps_original_ranges_and_quotes_values() {
        let source = "select '😀'; select :x, :x, $1";
        let result = bind(
            source.into(),
            RunTarget::All,
            DatabaseKind::Postgres,
            vec![
                ParamValue {
                    name: ":x".into(),
                    value: Some("a'\\b".into()),
                },
                ParamValue {
                    name: "$1".into(),
                    value: None,
                },
            ],
        )
        .unwrap();
        assert_eq!(
            result.spans[1].text(&result.text),
            "select E'a''\\\\b', E'a''\\\\b', NULL"
        );
        assert_eq!(result.ranges[1].from.0, 13);
        assert_eq!(result.changed, [false, true]);
    }
    #[test]
    fn tsql_declarations_labels_routines_and_batch_scopes() {
        assert_eq!(
            names(
                "DECLARE @a decimal(10,2) = coalesce(@input, 1), @b int = @other; SELECT @a, @b, @ID, @id, @@VERSION; EXEC dbo.p @label = @argument;",
                DatabaseKind::SqlServer
            ),
            ["@input", "@other", "@ID", "@id", "@argument"]
        );
        assert_eq!(
            names(
                "DECLARE @a TABLE (x int, y int)
DECLARE @c CURSOR FOR SELECT @input
SELECT @a, @c, @outside",
                DatabaseKind::SqlServer
            ),
            ["@input", "@outside"]
        );
        assert_eq!(
            names(
                "DECLARE @a int = 1
GO
SELECT @a",
                DatabaseKind::SqlServer
            ),
            ["@a"]
        );
        for sql in [
            "CREATE OR ALTER PROC dbo.p @id int AS SELECT @id",
            "ALTER FUNCTION dbo.f(@id int) RETURNS int AS BEGIN RETURN @id END",
            "CREATE RULE dbo.positive AS @value > 0",
            "CREATE TRIGGER dbo.t ON dbo.items AFTER INSERT AS SELECT @value",
        ] {
            assert!(names(sql, DatabaseKind::SqlServer).is_empty());
        }
        assert_eq!(
            names(
                "SELECT '@ignored', [@ignored], \"@ignored\" /* @ignored */ -- @ignored\n, @real",
                DatabaseKind::SqlServer
            ),
            ["@real"]
        );
    }

    #[test]
    fn exact_coverage_is_required() {
        for values in [
            vec![],
            vec![ParamValue {
                name: ":y".into(),
                value: None,
            }],
            vec![
                ParamValue {
                    name: ":x".into(),
                    value: None,
                },
                ParamValue {
                    name: ":x".into(),
                    value: None,
                },
            ],
            vec![ParamValue {
                name: ":x".into(),
                value: Some("\0".into()),
            }],
        ] {
            assert!(
                bind(
                    "select :x".into(),
                    RunTarget::All,
                    DatabaseKind::Postgres,
                    values
                )
                .is_err()
            );
        }
    }
    #[test]
    fn selection_discovers_only_selected_sql() {
        assert_eq!(
            discover(
                "select :a; select :b",
                RunTarget::Selection {
                    from: crate::model::DocOffset(11),
                    to: crate::model::DocOffset(20)
                },
                DatabaseKind::Postgres
            )
            .unwrap(),
            [Param { name: ":b".into() }]
        );
    }
}
