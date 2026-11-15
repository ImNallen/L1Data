use std::collections::HashMap;
use std::ops::Range;

use crate::model::{CellSource, DatabaseKind, DocRange, Param, ParamValue, RunError, RunTarget};
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
        if matches!(token.kind, LexKind::Space | LexKind::LineComment | LexKind::BlockComment) {
            continue;
        }
        if prefix_len < prefix.len() {
            prefix[prefix_len] = if token.kind == LexKind::Word { &text[token.span.clone()] } else { "" };
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
                let declaration = if prefix[1].eq_ignore_ascii_case("OR") && prefix[2].eq_ignore_ascii_case("REPLACE") {
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
            LexKind::Other if &text[token.span.clone()] == ":" && brackets.iter().all(|constructor| *constructor) => {
                if let Some(next) =
                    tokens.peek().filter(|next| next.kind == LexKind::Word && token.span.end == next.span.start)
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
            if !standard.is_empty() && text.contains('\\') && standard != pg_occurrences(text, Strings::Escaped)? {
                return Err(unsupported(
                    "Parameter locations depend on standard_conforming_strings. Use E'...' strings with explicit backslash escaping.",
                ));
            }
            Ok(standard)
        }
    }
}

/// The values a run may bind, by name. Built only where values enter the engine.
#[derive(Default)]
pub(crate) struct Values(HashMap<String, Option<String>>);

impl Values {
    pub(crate) fn new(list: Vec<ParamValue>) -> Result<Self, RunError> {
        let mut values = HashMap::with_capacity(list.len());
        for ParamValue { name, value } in list {
            if values.contains_key(&name) {
                return Err(unsupported(format!("Duplicate parameter {name}.")));
            }
            values.insert(name, value);
        }
        Ok(Self(values))
    }
}

/// Substitutes parameters wherever a run meets them, and notes each name once in first-use order. Only `prepare` makes
/// one, so `refs::expand` cannot run without binding.
pub(crate) struct Binder<'a> {
    values: &'a Values,
    kind: DatabaseKind,
    used: Vec<String>,
}

impl Binder<'_> {
    pub(crate) fn kind(&self) -> DatabaseKind {
        self.kind
    }

    /// Where `sql` uses a parameter, in `sql`'s own byte offsets, with the name each one uses.
    pub(crate) fn find<'s>(&self, sql: &'s str) -> Result<Vec<(Range<usize>, &'s str)>, RunError> {
        Ok(occurrences(sql, self.kind)?.into_iter().map(|r| (r.clone(), &sql[r])).collect())
    }

    /// The literal that stands for `name`. A name with no value stands as NULL, and `Prepared::executable` refuses it
    /// before anything is sent. A NUL value is refused only here, where a unit binds it.
    pub(crate) fn literal(&mut self, name: &str) -> Result<String, RunError> {
        if !self.used.iter().any(|n| n == name) {
            self.used.push(name.to_owned());
        }
        match self.values.0.get(name) {
            Some(Some(value)) if value.contains('\0') => Err(unsupported(format!("Parameter {name} contains NUL."))),
            Some(Some(value)) => Ok(match self.kind {
                DatabaseKind::Postgres => text::quote_escaped_literal(value),
                DatabaseKind::SqlServer => format!("N'{}'", value.replace('\'', "''")),
            }),
            Some(None) | None => Ok("NULL".into()),
        }
    }
}

/// A cell's text as the server would run it: units bound, references expanded with their bodies bound, and the names
/// met on the way.
pub(crate) struct Prepared {
    script: ExecutableScript,
    sources: Vec<CellSource>,
    used: Vec<String>,
    missing: Option<String>,
}

impl Prepared {
    /// Every name the selected units and the cells they reach use, each once: the units' own first, then each body's in
    /// the order expansion resolves it.
    pub(crate) fn params(self) -> Vec<Param> {
        self.used.into_iter().map(|name| Param { name }).collect()
    }

    /// The SQL to send and the cells it inlined, refused while a name it uses has no value.
    pub(crate) fn executable(self) -> Result<(ExecutableScript, Vec<CellSource>), RunError> {
        match self.missing {
            Some(name) => Err(unsupported(format!("Missing value for parameter {name}."))),
            None => Ok((self.script, self.sources)),
        }
    }
}

/// The only path from a cell's text to executable SQL. `console_run` and `script_params` both call it, so discovery
/// can never disagree with what binding needs.
pub(crate) fn prepare(
    text: String,
    target: RunTarget,
    kind: DatabaseKind,
    values: &Values,
    cells: &[CellSource],
) -> Result<Prepared, RunError> {
    let spans = selected(&text, target, kind)?;
    let map = OffsetMap::new(&text);
    let ranges = spans.iter().map(|span| map.range(span)).collect();
    let mut binder = Binder { values, kind, used: Vec::new() };
    let found = spans.iter().map(|span| binder.find(span.text(&text))).collect::<Result<Vec<_>, _>>()?;
    let script = if found.iter().all(Vec::is_empty) {
        let changed = vec![false; spans.len()];
        ExecutableScript { text, spans, ranges, changed }
    } else {
        let mut executable = String::with_capacity(text.len());
        let mut adjusted = Vec::with_capacity(spans.len());
        let mut changed = Vec::with_capacity(spans.len());
        for (span, found) in spans.iter().zip(found) {
            let unit = span.text(&text);
            let start = executable.len();
            let mut at = 0;
            changed.push(!found.is_empty());
            for (occurrence, name) in found {
                executable.push_str(&unit[at..occurrence.start]);
                executable.push_str(&binder.literal(name)?);
                at = occurrence.end;
            }
            executable.push_str(&unit[at..]);
            adjusted.push(StmtSpan { start, end: executable.len() });
            executable.push('\n');
        }
        ExecutableScript { text: executable, spans: adjusted, ranges, changed }
    };
    let (script, sources) = sql::refs::expand(script, cells, &mut binder)?;
    let missing = binder.used.iter().find(|name| !values.0.contains_key(*name)).cloned();
    Ok(Prepared { script, sources, used: binder.used, missing })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(pairs: &[(&str, Option<&str>)]) -> Values {
        Values::new(
            pairs
                .iter()
                .map(|(name, value)| ParamValue { name: (*name).into(), value: value.map(Into::into) })
                .collect(),
        )
        .unwrap()
    }

    fn discover(text: &str, target: RunTarget, kind: DatabaseKind) -> Result<Vec<Param>, RunError> {
        prepare(text.into(), target, kind, &Values::default(), &[]).map(Prepared::params)
    }

    fn bind(text: &str, kind: DatabaseKind, pairs: &[(&str, Option<&str>)]) -> Result<ExecutableScript, RunError> {
        prepare(text.into(), RunTarget::All, kind, &values(pairs), &[])?.executable().map(|(script, _)| script)
    }

    fn names(text: &str, kind: DatabaseKind) -> Vec<String> {
        discover(text, RunTarget::All, kind).unwrap().into_iter().map(|p| p.name).collect()
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
        assert!(discover(r"select '\' :x ' :y", RunTarget::All, DatabaseKind::Postgres).is_err());
    }

    #[test]
    fn parameter_free_strings_keep_their_original_sql() {
        for source in [r"SELECT '\' AS slash, '$0' AS txt", r"SELECT '\' AS slash, '$1' AS txt"] {
            let found = discover(source, RunTarget::All, DatabaseKind::Postgres);
            assert!(found.as_ref().is_ok_and(Vec::is_empty), "parameter-free script was rejected: {found:?}");
            let bound = bind(source, DatabaseKind::Postgres, &[]).unwrap();
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
            let bound = bind(source, DatabaseKind::Postgres, &[]).unwrap();
            assert_eq!(bound.text, source);
        }
        let source = "PREPARE q(int) AS SELECT $1; SELECT $1::int";
        assert_eq!(names(source, DatabaseKind::Postgres), ["$1"]);
        let bound = bind(source, DatabaseKind::Postgres, &[("$1", Some("7"))]).unwrap();
        assert_eq!(bound.spans[0].text(&bound.text), "PREPARE q(int) AS SELECT $1");
        assert_eq!(bound.spans[1].text(&bound.text), "SELECT E'7'::int");
        assert_eq!(names("PREPARE q(int) AS SELECT $1 + :extra::int", DatabaseKind::Postgres), [":extra"]);
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
        let result = bind(source, DatabaseKind::Postgres, &[(":x", Some("a'\\b")), ("$1", None)]).unwrap();
        assert_eq!(result.spans[1].text(&result.text), "select E'a''\\\\b', E'a''\\\\b', NULL");
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
    fn binding_takes_the_values_each_unit_uses_and_ignores_the_rest() {
        let pairs = [(":x", Some("1")), (":unused", Some("2")), ("$1", None)];
        assert_eq!(bind("select :x, $1", DatabaseKind::Postgres, &pairs).unwrap().text, "select E'1', NULL\n");
        assert_eq!(bind("select 1", DatabaseKind::Postgres, &pairs).unwrap().text, "select 1");
        let selected = prepare(
            "select :a; select :x".into(),
            RunTarget::Selection { from: crate::model::DocOffset(11), to: crate::model::DocOffset(20) },
            DatabaseKind::Postgres,
            &values(&pairs),
            &[],
        )
        .unwrap();
        assert!(selected.executable().is_ok(), "a name only an unselected unit uses needs no value");
    }

    #[test]
    fn a_name_with_no_value_is_noted_and_refused_only_when_executable() {
        let prepared = || {
            prepare(
                "select :x, :y, :x".into(),
                RunTarget::All,
                DatabaseKind::Postgres,
                &values(&[(":x", Some("1"))]),
                &[],
            )
        };
        assert_eq!(prepared().unwrap().params(), [Param { name: ":x".into() }, Param { name: ":y".into() }]);
        assert!(matches!(
            prepared().unwrap().executable(),
            Err(RunError::Unsupported { what }) if what == "Missing value for parameter :y."
        ));
    }

    #[test]
    fn duplicate_names_are_refused_where_values_enter() {
        let twice = vec![
            ParamValue { name: ":x".into(), value: None },
            ParamValue { name: ":x".into(), value: Some("1".into()) },
        ];
        assert!(matches!(Values::new(twice), Err(RunError::Unsupported { what }) if what == "Duplicate parameter :x."));
    }

    #[test]
    fn a_nul_value_is_refused_only_where_it_binds() {
        let pairs = [(":x", Some("a\0b")), (":y", Some("1"))];
        assert!(bind("select :y", DatabaseKind::Postgres, &pairs).is_ok());
        assert!(bind("select :x", DatabaseKind::Postgres, &pairs).is_err());
        assert!(bind("select @x", DatabaseKind::SqlServer, &[("@x", Some("\0"))]).is_err());
    }

    #[test]
    fn selection_discovers_only_selected_sql() {
        assert_eq!(
            discover(
                "select :a; select :b",
                RunTarget::Selection { from: crate::model::DocOffset(11), to: crate::model::DocOffset(20) },
                DatabaseKind::Postgres
            )
            .unwrap(),
            [Param { name: ":b".into() }]
        );
    }
}
