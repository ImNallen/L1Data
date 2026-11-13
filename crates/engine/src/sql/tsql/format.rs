use super::{Kind, STATEMENT_WORDS, Token, lex, split_batches};

#[derive(Clone, Copy)]
enum InsertSource {
    Values,
    Select,
}

#[derive(Clone, Copy)]
enum Statement {
    Select,
    InsertTarget,
    InsertSource(InsertSource),
    Update { set: bool },
    Delete,
}

fn format_batch(text: &str) -> Result<String, String> {
    let tokens = lex(text)?;
    let mut statement = None;
    let mut significant: Option<Token<'_>> = None;
    let mut depth = 0usize;
    let mut out = String::new();
    let mut previous: Option<Token<'_>> = None;
    for token in tokens {
        let mut dml_clause = false;
        if !token.comment() {
            let root = statement.is_none() && token.text != ";";
            let mut allowed = false;
            if root {
                statement = Some(if token.word("SELECT") {
                    Statement::Select
                } else if token.word("INSERT") {
                    Statement::InsertTarget
                } else if token.word("UPDATE") {
                    Statement::Update { set: false }
                } else if token.word("DELETE") {
                    Statement::Delete
                } else {
                    return Err("Unsupported T-SQL formatting statement".into());
                });
                allowed = true;
            } else if depth == 0 {
                match statement {
                    Some(Statement::InsertTarget)
                        if token.word("INTO") && significant.is_some_and(|p| p.word("INSERT")) =>
                    {
                        allowed = true
                    }
                    Some(Statement::InsertTarget) if token.word("VALUES") || token.word("SELECT") => {
                        statement = Some(Statement::InsertSource(if token.word("VALUES") {
                            InsertSource::Values
                        } else {
                            InsertSource::Select
                        }));
                        allowed = true;
                        dml_clause = token.word("VALUES");
                    }
                    Some(Statement::Update { set: false }) if token.word("SET") => {
                        statement = Some(Statement::Update { set: true });
                        allowed = true;
                        dml_clause = true;
                    }
                    _ => {}
                }
            }
            if !allowed
                && token.kind == Kind::Word
                && (STATEMENT_WORDS.iter().any(|w| {
                    token.word(w) && (*w != "SELECT" || depth == 0 && !matches!(statement, Some(Statement::Select)))
                }) || ["APPLY", "CASE", "EXCEPT", "FOR", "INTERSECT", "INTO", "OVER", "PIVOT", "UNION", "UNPIVOT"]
                    .iter()
                    .any(|w| token.word(w))
                    || token.word("OUTPUT") && !matches!(statement, Some(Statement::Select))
                    || token.word("VALUES") && depth == 0 && !matches!(statement, Some(Statement::Select)))
            {
                return Err(format!("Unsupported T-SQL formatting syntax {}", token.text));
            }
            if token.kind == Kind::Symbol
                && ![
                    "(", ")", ".", ",", ";", "+", "-", "*", "/", "%", "&", "|", "^", "~", "=", "<", ">", "<=", ">=",
                    "<>", "!=", "!<", "!>",
                ]
                .contains(&token.text)
            {
                return Err(format!("Unsupported T-SQL formatting symbol {}", token.text));
            }
        }
        let gap = previous.map_or("", |p| &text[p.end..token.start]);
        let clause = dml_clause
            || depth == 0
                && token.word("SELECT")
                && matches!(statement, Some(Statement::InsertSource(InsertSource::Select)))
            || depth == 0
                && ["FROM", "WHERE", "GROUP", "HAVING", "ORDER", "OFFSET", "FETCH"].iter().any(|w| token.word(w))
                && !(token.word("FROM")
                    && matches!(statement, Some(Statement::Delete))
                    && significant.is_some_and(|p| p.word("DELETE")));
        let newline = clause
            || previous.is_some_and(|p| p.kind == Kind::LineComment || p.text == ";")
            || token.comment() && gap.contains(['\r', '\n']);
        if !out.is_empty() {
            if newline {
                if !out.ends_with('\n') {
                    out.push('\n');
                }
            } else if !matches!(token.text, "." | "," | ")" | ";")
                && !previous.is_some_and(|p| matches!(p.text, "." | "("))
                && !(token.text == "(" && gap.is_empty())
            {
                out.push(' ');
            }
        }
        out.push_str(token.text);
        match token.text {
            "(" if !token.comment() => depth += 1,
            ")" if !token.comment() => {
                depth = depth.checked_sub(1).ok_or("Unsupported unbalanced T-SQL parentheses")?
            }
            ";" if !token.comment() && depth == 0 => {
                if matches!(statement, Some(Statement::InsertTarget | Statement::Update { set: false })) {
                    return Err("Unsupported incomplete T-SQL formatting statement".into());
                }
                statement = None;
            }
            _ => {}
        }
        if !token.comment() {
            significant = Some(token);
        }
        previous = Some(token);
    }
    if depth != 0 {
        return Err("Unsupported unbalanced T-SQL parentheses".into());
    }
    if matches!(statement, Some(Statement::InsertTarget | Statement::Update { set: false })) {
        return Err("Unsupported incomplete T-SQL formatting statement".into());
    }
    Ok(out)
}

pub(crate) fn format(text: &str) -> Result<String, String> {
    if text.len() > 65_536 {
        return Err("Unsupported T-SQL formatting above 64 KiB".into());
    }
    let batches = split_batches(text)?;
    if batches.is_empty() {
        return Err("No T-SQL batch to format".into());
    }
    let mut out = String::new();
    let mut at = 0;
    for span in batches {
        out.push_str(&text[at..span.start]);
        out.push_str(&format_batch(span.text(text))?);
        at = span.end;
    }
    out.push_str(&text[at..]);
    let signature = |text: &str| -> Result<Vec<Vec<(Kind, String)>>, String> {
        split_batches(text)?
            .into_iter()
            .map(|s| Ok(lex(s.text(text))?.into_iter().map(|t| (t.kind, t.text.to_owned())).collect()))
            .collect()
    };
    if signature(text)? != signature(&out)? {
        return Err("Unsupported T-SQL formatting would change tokens or batch scope".into());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn batches(text: &str) -> Vec<&str> {
        split_batches(text).unwrap().iter().map(|s| s.text(text)).collect()
    }

    #[test]
    fn formatter_preserves_tokens_comments_identifiers_and_batch_scope() {
        let text = "-- header\nselect  [旅]]人] ,N'go\nGO\nit''s', @value from [Sales].[T] /* keep\n GO */ where id=1 order by [旅]]人] desc; -- end\nGO -- separator\nselect  \"MixedCase\", lower(N'A') from #Temp;\n";
        let formatted = format(text).unwrap();
        assert!(formatted.contains("\nfrom [Sales].[T]"), "{formatted}");
        assert!(formatted.contains("GO -- separator\n"));
        assert_eq!(batches(&formatted).len(), 2);
        let tokens = |s: &str| lex(s).unwrap().into_iter().map(|t| (t.kind, t.text.to_owned())).collect::<Vec<_>>();
        assert_eq!(tokens(text), tokens(&formatted));
        assert_eq!(format(&formatted).unwrap(), formatted);
        assert_eq!(format("select 1 select 2").unwrap(), "select 1 select 2");
        assert_eq!(
            format("select  a,b from t where a=1; select 2").unwrap(),
            "select a, b\nfrom t\nwhere a = 1;\nselect 2"
        );
    }

    #[test]
    fn formatter_lays_out_ordinary_dml_without_changing_tokens() {
        for (text, expected) in [
            (
                "insert into dbo.t (id,n) values (1,N'hello'),(2,@n);",
                "insert into dbo.t (id, n)\nvalues (1, N'hello'),(2, @n);",
            ),
            ("INSERT #t DEFAULT VALUES;", "INSERT #t DEFAULT\nVALUES;"),
            (
                "Insert dbo.t(id,n) Select id,n From dbo.s Where id>@id Order by id;",
                "Insert dbo.t(id, n)\nSelect id, n\nFrom dbo.s\nWhere id > @id\nOrder by id;",
            ),
            (
                "update t set n=@n,id=id+1 from dbo.t t where id=2;",
                "update t\nset n = @n, id = id + 1\nfrom dbo.t t\nwhere id = 2;",
            ),
            ("DELETE FROM dbo.t WHERE id=@id;", "DELETE FROM dbo.t\nWHERE id = @id;"),
            (
                "delete t from dbo.t t join dbo.s s on t.id=s.id where s.n=1;",
                "delete t\nfrom dbo.t t join dbo.s s on t.id = s.id\nwhere s.n = 1;",
            ),
            (
                "update t set n=(select max(n) from s); delete from t; insert t values (1); select 2;",
                "update t\nset n =(select max(n) from s);\ndelete from t;\ninsert t\nvalues (1);\nselect 2;",
            ),
            (
                "insert /* target */ into [旅]]人] values (N'😀',-1,1e-3,@旅); -- keep\nGO -- batch\nDELETE /* keep */ FROM #t WHERE n=-@n;",
                "insert /* target */ into [旅]]人]\nvalues (N'😀', - 1, 1e-3, @旅);\n-- keep\nGO -- batch\nDELETE /* keep */ FROM #t\nWHERE n = - @n;",
            ),
            ("INSERT t VALUES ((SELECT max(n) FROM s));", "INSERT t\nVALUES ((SELECT max(n) FROM s));"),
        ] {
            let formatted = format(text).unwrap();
            assert_eq!(formatted, expected, "{text}");
            assert_eq!(format(&formatted).unwrap(), formatted);
            let tokens = |s: &str| lex(s).unwrap().into_iter().map(|t| (t.kind, t.text.to_owned())).collect::<Vec<_>>();
            assert_eq!(tokens(text), tokens(&formatted));
            assert_eq!(batches(text).len(), batches(&formatted).len());
        }
    }

    #[test]
    fn formatter_refuses_unsupported_syntax_instead_of_using_the_pg_formatter() {
        for text in [
            "SET NOCOUNT ON; SELECT 1",
            "UPDATE t SET x=1 SET NOCOUNT ON",
            "SELECT 1 UPDATE t SET x=1",
            "UPDATE t SET x=1 DELETE FROM t",
            "DELETE FROM t INSERT t VALUES (1)",
            "INSERT t VALUES (1) SELECT 2",
            "INSERT t SELECT x FROM s SELECT 2",
            "UPDATE t SET x=1 SELECT 2",
            "INSERT t INTO s VALUES (1)",
            "INSERT INTO t SELECT * INTO #s FROM s",
            "INSERT t VALUES (1) OUTPUT inserted.id",
            "UPDATE t SET x=CASE WHEN id=1 THEN 2 END",
            "DELETE FROM t OUTPUT deleted.id",
            "MERGE t USING s ON t.id=s.id WHEN MATCHED THEN DELETE;",
            "UPDATE t",
            "INSERT INTO t; SELECT 1",
            "SELECT (1; UPDATE t SET x=1)",
            "DECLARE @x int; SELECT @x",
            "BEGIN SELECT 1 END",
            "CREATE PROCEDURE p AS SELECT 1",
            "WITH c AS (SELECT 1) SELECT * FROM c",
            "SELECT CASE WHEN 1=1 THEN 2 END",
            "SELECT ROW_NUMBER() OVER (ORDER BY id) FROM t",
            "SELECT 1 UNION SELECT 2",
            "SELECT * INTO #t FROM t",
            "SELECT $$pg$$",
            "SELECT (1",
            "SELECT 1)",
            "SELECT 'open",
            "SELECT 1\nGO 2",
            "-- comments only",
        ] {
            assert!(format(text).is_err(), "accepted {text:?}");
        }
        assert!(format(&format!("SELECT '{}'", "x".repeat(65_536))).is_err());
    }
}
