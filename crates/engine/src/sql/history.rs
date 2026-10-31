use super::lex::{LexKind, Lexeme, is_ident_cont, lex, run};
use super::split::skip_block_comment;

#[derive(Debug, PartialEq)]
pub(super) enum Token<'a> {
    Word(&'a str),
    Quoted(&'a str),
    UnicodeEscape,
    Dot,
    Open,
    Close,
    Comma,
    Number(&'a str),
    /// A run of operator characters, lexed as Postgres lexes it, with `!=` read as `<>`.
    Operator(&'a str),
    Other,
}

/// The lexemes without comments and whitespace, in the shape `vet` and `history_is_write` read:
/// a string's prefix is a word of its own, and a number splits at its dots, signs, and letters.
pub(super) fn tokens(sql: &str) -> Vec<Token<'_>> {
    let mut out = Vec::new();
    for Lexeme { kind, span } in lex(sql) {
        let text = &sql[span];
        match kind {
            LexKind::Space | LexKind::LineComment | LexKind::BlockComment => {}
            LexKind::Word => out.push(Token::Word(text)),
            LexKind::QuotedIdent => {
                let quoted = match text.strip_prefix(['U', 'u']) {
                    Some(rest) => {
                        out.push(Token::UnicodeEscape);
                        &rest[1..]
                    }
                    None => text,
                };
                let closed = quoted.len() > 1 && quoted.ends_with('"');
                out.push(Token::Quoted(&quoted[1..if closed { quoted.len() - 1 } else { quoted.len() }]));
            }
            LexKind::String => {
                match &text[..text.find('\'').unwrap_or(0)] {
                    "" => {}
                    "U&" | "u&" => out.push(Token::UnicodeEscape),
                    prefix => out.push(Token::Word(prefix)),
                }
                out.push(Token::Other);
            }
            LexKind::Number | LexKind::Param => split_number(text, &mut out),
            LexKind::Operator => match text {
                "::" => out.extend([Token::Other, Token::Other]),
                "=>" => out.push(Token::Other),
                "!=" => out.push(Token::Operator("<>")),
                op => out.push(Token::Operator(op)),
            },
            LexKind::Dot => out.push(Token::Dot),
            LexKind::Comma => out.push(Token::Comma),
            LexKind::Open => out.push(Token::Open),
            LexKind::Close => out.push(Token::Close),
            LexKind::DollarString | LexKind::OpenBracket | LexKind::CloseBracket | LexKind::Semicolon | LexKind::Other => out.push(Token::Other),
        }
    }
    out
}

fn split_number<'a>(text: &'a str, out: &mut Vec<Token<'a>>) {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        i = match bytes[i] {
            c if c.is_ascii_digit() => run(bytes, i, |c| c.is_ascii_digit()),
            b'$' | b'.' | b'+' | b'-' => i + 1,
            _ => run(bytes, i + 1, is_ident_cont),
        };
        let text = &text[start..i];
        out.push(match bytes[start] {
            b'$' => Token::Other,
            b'.' => Token::Dot,
            b'+' | b'-' => Token::Operator(text),
            c if c.is_ascii_digit() => Token::Number(text),
            _ => Token::Word(text),
        });
    }
}

fn word(token: Option<&Token<'_>>, expected: &str) -> bool {
    matches!(token, Some(Token::Word(w)) if w.eq_ignore_ascii_case(expected))
}

fn closing(tokens: &[Token<'_>], open: usize) -> Option<usize> {
    let mut depth = 0;
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

fn writes(tokens: &[Token<'_>]) -> bool {
    let Some(Token::Word(first)) = tokens.first() else {
        return false;
    };
    match first.to_ascii_uppercase().as_str() {
        "INSERT" | "UPDATE" | "DELETE" | "MERGE" | "TRUNCATE" | "CREATE" | "ALTER" | "DROP"
        | "COMMENT" | "GRANT" | "REVOKE" | "REINDEX" | "RENAME" | "IMPORT" | "SECURITY" | "CALL" | "DO" | "EXECUTE" | "REFRESH"
        | "VACUUM" | "CLUSTER" => true,
        "PREPARE" => word(tokens.get(1), "TRANSACTION"),
        "COMMIT" | "ROLLBACK" => word(tokens.get(1), "PREPARED"),
        "COPY" => {
            let mut i = 1;
            if word(tokens.get(i), "BINARY") {
                i += 1;
            }
            if tokens.get(i) == Some(&Token::Open) {
                return closing(tokens, i).is_some_and(|end| writes(&tokens[i + 1..end]));
            }
            while i < tokens.len() {
                if word(tokens.get(i), "FROM") {
                    return true;
                }
                if word(tokens.get(i), "TO") {
                    return false;
                }
                if tokens.get(i) == Some(&Token::Open) {
                    let Some(end) = closing(tokens, i) else {
                        return false;
                    };
                    i = end;
                }
                i += 1;
            }
            false
        }
        "WITH" => {
            let mut i = 1;
            if word(tokens.get(i), "RECURSIVE") {
                i += 1;
            }
            loop {
                let unicode = tokens.get(i) == Some(&Token::UnicodeEscape);
                if unicode {
                    i += 1;
                }
                if !matches!(tokens.get(i), Some(Token::Word(_) | Token::Quoted(_))) {
                    return false;
                }
                i += 1;
                if unicode && word(tokens.get(i), "UESCAPE") {
                    i += 1;
                    if word(tokens.get(i), "E") {
                        i += 1;
                    }
                    if tokens.get(i) != Some(&Token::Other) {
                        return false;
                    }
                    i += 1;
                }
                if tokens.get(i) == Some(&Token::Open) {
                    let Some(end) = closing(tokens, i) else {
                        return false;
                    };
                    i = end + 1;
                }
                if !word(tokens.get(i), "AS") {
                    return false;
                }
                i += 1;
                if word(tokens.get(i), "NOT") {
                    i += 1;
                }
                if word(tokens.get(i), "MATERIALIZED") {
                    i += 1;
                }
                if tokens.get(i) != Some(&Token::Open) {
                    return false;
                }
                let Some(end) = closing(tokens, i) else {
                    return false;
                };
                if writes(&tokens[i + 1..end]) {
                    return true;
                }
                i = end + 1;
                if tokens.get(i) != Some(&Token::Comma) {
                    return writes(&tokens[i..]);
                }
                i += 1;
            }
        }
        "EXPLAIN" => {
            let mut i = 1;
            let mut analyze = false;
            if tokens.get(i) == Some(&Token::Open) {
                let Some(end) = closing(tokens, i) else {
                    return false;
                };
                for n in i + 1..end {
                    if word(tokens.get(n), "ANALYZE") || word(tokens.get(n), "ANALYSE") {
                        analyze = !word(tokens.get(n + 1), "FALSE")
                            && !word(tokens.get(n + 1), "OFF")
                            && tokens.get(n + 1) != Some(&Token::Number("0"));
                    }
                }
                i = end + 1;
            } else {
                while word(tokens.get(i), "ANALYZE")
                    || word(tokens.get(i), "ANALYSE")
                    || word(tokens.get(i), "VERBOSE")
                {
                    analyze |= word(tokens.get(i), "ANALYZE") || word(tokens.get(i), "ANALYSE");
                    i += 1;
                }
            }
            analyze && writes(&tokens[i..])
        }
        _ => false,
    }
}

/// History metadata describes recognizable writes, independently of transaction classification.
pub(crate) fn history_is_write(sql: &str) -> bool {
    writes(&tokens(sql))
}

fn excerpt(text: &str, limit: usize) -> String {
    let mut out = String::new();
    let mut space = false;
    for c in text.chars() {
        if c.is_whitespace() {
            space = !out.is_empty();
            continue;
        }
        if space {
            out.push(' ');
            space = false;
        }
        out.push(c);
        if out.chars().count() >= limit {
            out.push('…');
            break;
        }
    }
    out
}

pub(crate) fn history_text(sql: &str) -> (String, String) {
    let mut rest = sql.trim_start();
    let mut title = None;
    loop {
        if let Some(comment) = rest.strip_prefix("--") {
            let end = comment.find('\n').unwrap_or(comment.len());
            if title.is_none() && !comment[..end].trim().is_empty() {
                title = Some(excerpt(comment[..end].trim(), 90));
            }
            rest = comment[end..].trim_start();
        } else if rest.starts_with("/*") {
            let end = skip_block_comment(rest.as_bytes(), 0);
            let content_end = if rest[..end].ends_with("*/") {
                end - 2
            } else {
                end
            };
            if title.is_none() {
                title = Some(excerpt(rest[2..content_end].trim(), 90));
            }
            rest = rest[end..].trim_start();
        } else {
            break;
        }
    }
    let sql_excerpt = excerpt(rest, 180);
    let title = title
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| excerpt(rest, 72));
    (
        if title.is_empty() {
            "SQL statement".into()
        } else {
            title
        },
        sql_excerpt,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_cte_names_keep_the_write_statement_recognizable() {
        assert!(history_is_write(
            "WITH café AS (SELECT 1) DELETE FROM orders"
        ));
        assert!(history_is_write(
            "WITH café AS (DELETE FROM orders RETURNING *) SELECT * FROM café"
        ));
        assert!(!history_is_write(
            "WITH café AS (SELECT 1) SELECT * FROM café"
        ));
    }

    #[test]
    fn escaped_cte_names_preserve_write_classification() {
        for name in [r#"U&"ch\0061nged""#, r#"u&"ch!0061nged" UESCAPE '!'"#, r#"U&"changed" UESCAPE E'!'"#, "U&\"changed\" UESCAPE '!'\n''"] {
            for columns in ["", "(n)"] {
                for sql in [
                    format!("WITH {name}{columns} AS (DELETE FROM orders RETURNING n) SELECT * FROM changed"),
                    format!("WITH RECURSIVE seed AS (SELECT 1), {name}{columns} AS NOT MATERIALIZED (SELECT 1) DELETE FROM orders"),
                    format!("WITH RECURSIVE {name}{columns} AS (SELECT 1), later AS (UPDATE orders SET n = 2 RETURNING n) SELECT * FROM later"),
                ] {
                    assert!(history_is_write(&sql), "{sql}");
                    assert!(history_is_write(&format!("COPY ({sql}) TO PROGRAM 'cat'")), "{sql}");
                }
            }
        }
        for sql in [
            r#"WITH U&"DELETE" AS (SELECT 'UPDATE') SELECT * FROM "DELETE""#,
            r#"WITH U&"UPDATE" UESCAPE '!' ("DELETE") AS (SELECT $$INSERT INTO orders$$) SELECT * FROM "UPDATE""#,
            r#"WITH RECURSIVE U&"DROP" AS (SELECT 'DELETE'), "INSERT" AS (SELECT 'UPDATE') SELECT * FROM "DROP""#,
        ] {
            assert!(!history_is_write(sql), "{sql}");
            assert!(!history_is_write(&format!("COPY ({sql}) TO PROGRAM 'cat'")), "{sql}");
        }
    }

    #[test]
    fn tokens_keep_the_shape_vet_reads() {
        use Token::*;
        assert_eq!(
            tokens(r#"select E'\'', U&"d!0061" , u&'x', $1::int, 1.5e-3, a != b, f(x => 1) -- c"#),
            vec![
                Word("select"),
                Word("E"),
                Other,
                Comma,
                UnicodeEscape,
                Quoted("d!0061"),
                Comma,
                UnicodeEscape,
                Other,
                Comma,
                Other,
                Number("1"),
                Other,
                Other,
                Word("int"),
                Comma,
                Number("1"),
                Dot,
                Number("5"),
                Word("e"),
                Operator("-"),
                Number("3"),
                Comma,
                Word("a"),
                Operator("<>"),
                Word("b"),
                Comma,
                Word("f"),
                Open,
                Word("x"),
                Other,
                Number("1"),
                Close,
            ]
        );
        assert_eq!(tokens("x #>> 'p' /* c */ .. \"open"), vec![Word("x"), Operator("#>>"), Other, Other, Quoted("open")]);
    }

    #[test]
    fn recognizes_writes_without_changing_runner_classification() {
        for sql in [
            "insert into t values (1) returning *",
            "update t set x = 1 returning *",
            "delete from t returning *",
            "merge into t using s on true when matched then delete",
            "truncate t",
            "create table t (id int)",
            "drop table t",
            "WITH x AS (DELETE FROM t RETURNING *) SELECT * FROM x",
            "with x(a) as materialized (select 1), y as (update t set x=2 returning *) select * from y",
            "with x as (select 1) insert into t select * from x",
            "explain analyze update t set x=1",
            "explain (analyze true, buffers) delete from t",
            "explain (analyze 1) delete from t",
            "call purge_orders()",
            "do $$ begin delete from t; end $$",
            "execute purge_orders_plan(7)",
            "refresh materialized view v",
            "vacuum full t",
            "cluster t using t_pkey",
            "prepare transaction 'p1'",
            "commit prepared 'p1'",
            "ROLLBACK PREPARED 'p1'",
        ] {
            assert!(history_is_write(sql), "{sql}");
        }
        assert_eq!(
            super::super::classify("with x as (delete from t returning *) select * from x"),
            super::super::StmtKind::Query
        );
    }

    #[test]
    fn copy_from_and_copy_write_queries_are_writes() {
        for sql in [
            "copy public.orders from '/tmp/orders.csv' with (format csv)",
            "copy public.orders from program 'printf 7'",
            "COPY BINARY public.orders FROM '/tmp/orders.bin'",
            "/* load */ COPY \"from\".\"to\" (\"to\", \"from\") /* direction */ FROM PROGRAM 'printf 7'",
            "copy public.orders (id, total) from $file$/tmp/from.csv$file$",
            "copy public.orders from stdin",
            "copy (insert into orders values (1) returning *) to '/tmp/orders.csv'",
            "copy (update orders set total = 1 returning *) to program 'cat'",
            "copy (delete from orders returning *) to '/tmp/orders.csv'",
            "copy (with changed as (delete from orders returning *) select * from changed) to '/tmp/orders.csv'",
        ] {
            assert!(history_is_write(sql), "{sql}");
        }
    }

    #[test]
    fn copy_to_reads_ignore_from_in_queries_names_and_strings() {
        for sql in [
            "copy public.orders to '/tmp/from.csv'",
            "copy public.orders to program 'echo FROM'",
            "copy binary \"from\".\"to\" (\"from\") to '/tmp/orders.bin'",
            "copy (select * from orders) to '/tmp/orders.csv'",
            "copy (select 'from', \"from\" from orders where id in (select id from items)) to program 'cat'",
            "copy (with data as (select * from orders) select * from data) to '/tmp/orders.csv'",
            "copy public.orders (id) /* FROM '/tmp/wrong' */ to stdout",
            "copy (select $$delete from orders$$) to stdout",
        ] {
            assert!(!history_is_write(sql), "{sql}");
        }
    }

    #[test]
    fn ignores_quoted_words_comments_and_unexecuted_explain() {
        for sql in [
            "select 'update'",
            "/* outer /* update */ delete */ select 1 -- insert",
            "select $$ update t $$",
            "select $tag$ delete from t $tag$",
            "select E'it\\'s update'",
            "select \"update\" from t",
            "with x as (select 'delete') select * from x",
            "with \"update\" as (select 1) select * from \"update\"",
            "select (select 'update')",
            "explain update t set x=1",
            "explain (analyze false) delete from t",
            "explain (analyze off) delete from t",
            "explain (analyze 0) delete from t",
            "explain analyze select pg_sleep(1)",
            "prepare q as select 1",
            "commit",
            "rollback and chain",
        ] {
            assert!(!history_is_write(sql), "{sql}");
        }
    }

    #[test]
    fn titles_use_leading_comments_and_excerpts_are_single_line() {
        assert_eq!(
            history_text("-- Revenue by region\nSELECT region,\n count(*) FROM orders"),
            (
                "Revenue by region".into(),
                "SELECT region, count(*) FROM orders".into()
            )
        );
        assert_eq!(
            history_text("/* inventory */ update t set x=1").0,
            "inventory"
        );
        assert_eq!(history_text("select 1").0, "select 1");
        assert_eq!(history_text("/*🦀"), ("🦀".into(), "".into()));
        assert_eq!(history_text("/*é"), ("é".into(), "".into()));
        assert_eq!(history_text("/*"), ("SQL statement".into(), "".into()));
        assert!(history_text(&"界".repeat(500)).1.chars().count() <= 181);
    }
}
