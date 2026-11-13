use super::{Kind, STATEMENT_WORDS, Token, lex};

pub(super) fn expression_name(token: &Token<'_>) -> bool {
    token.kind == Kind::Ident
        || token.kind == Kind::Word
            && !STATEMENT_WORDS
                .iter()
                .chain(
                    [
                        "ALL",
                        "AND",
                        "ANY",
                        "AS",
                        "ASC",
                        "BETWEEN",
                        "BY",
                        "CASE",
                        "COLLATE",
                        "DESC",
                        "DISTINCT",
                        "ESCAPE",
                        "EXCEPT",
                        "EXISTS",
                        "FOR",
                        "FROM",
                        "GROUP",
                        "HAVING",
                        "IN",
                        "INTERSECT",
                        "INTO",
                        "IS",
                        "JOIN",
                        "LIKE",
                        "NOT",
                        "NULL",
                        "OFFSET",
                        "OPTION",
                        "OR",
                        "ORDER",
                        "OVER",
                        "SOME",
                        "THEN",
                        "TOP",
                        "UNION",
                        "WHEN",
                        "WHERE",
                    ]
                    .iter(),
                )
                .any(|w| token.word(w))
}

const SCALAR_FUNCTIONS: &[&str] = &[
    "ABS",
    "ASCII",
    "CEILING",
    "CHAR",
    "CHARINDEX",
    "COALESCE",
    "CONCAT",
    "CONCAT_WS",
    "DATALENGTH",
    "DATEADD",
    "DATEDIFF",
    "DATEDIFF_BIG",
    "DATENAME",
    "DATEPART",
    "DAY",
    "EOMONTH",
    "FLOOR",
    "GETDATE",
    "GETUTCDATE",
    "ISNULL",
    "LEFT",
    "LEN",
    "LOWER",
    "LTRIM",
    "MONTH",
    "NCHAR",
    "NULLIF",
    "PATINDEX",
    "POWER",
    "REPLACE",
    "REPLICATE",
    "REVERSE",
    "RIGHT",
    "ROUND",
    "RTRIM",
    "SIGN",
    "SQRT",
    "STUFF",
    "SUBSTRING",
    "SYSDATETIME",
    "SYSDATETIMEOFFSET",
    "SYSUTCDATETIME",
    "TRIM",
    "UNICODE",
    "UPPER",
    "YEAR",
];

struct Expression<'a> {
    tokens: &'a [Token<'a>],
    at: usize,
}

impl Expression<'_> {
    fn take(&mut self, word: &str) -> bool {
        if self.tokens.get(self.at).is_some_and(|t| t.word(word) || t.kind == Kind::Symbol && t.text == word) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn require(&mut self, word: &str) -> Result<(), String> {
        if self.take(word) { Ok(()) } else { Err(format!("Expected {word} in T-SQL expression")) }
    }

    fn expr(&mut self, min: u8, depth: usize) -> Result<(), String> {
        if depth > 64 {
            return Err("T-SQL expression nesting exceeds 64".into());
        }
        if self.take("NOT") {
            self.expr(3, depth + 1)?;
        } else if self.take("+") || self.take("-") || self.take("~") {
            self.expr(6, depth + 1)?;
        } else if self.take("(") {
            self.expr(0, depth + 1)?;
            self.require(")")?;
        } else {
            let token = self.tokens.get(self.at).ok_or("Expected a T-SQL expression")?;
            if matches!(token.kind, Kind::String | Kind::Number | Kind::Variable)
                || token.word("NULL")
                || token.word("CURRENT_TIMESTAMP")
            {
                self.at += 1;
            } else if expression_name(token) {
                self.at += 1;
                if self.take("(") {
                    if token.kind != Kind::Word || !SCALAR_FUNCTIONS.iter().any(|w| token.word(w)) {
                        return Err("Unsupported T-SQL expression function".into());
                    }
                    if !self.take(")") {
                        loop {
                            self.expr(0, depth + 1)?;
                            if !self.take(",") {
                                break;
                            }
                        }
                        self.require(")")?;
                    }
                } else {
                    while self.take(".") {
                        let part = self.tokens.get(self.at).ok_or("Incomplete T-SQL column name")?;
                        if !expression_name(part) {
                            return Err("Unsupported T-SQL column name".into());
                        }
                        self.at += 1;
                    }
                }
            } else {
                return Err(format!("Unsupported T-SQL expression token {}", token.text));
            }
        }
        loop {
            let Some(token) = self.tokens.get(self.at) else {
                break;
            };
            let bp = if token.word("OR") {
                1
            } else if token.word("AND") {
                2
            } else if ["IS", "IN", "LIKE", "BETWEEN", "NOT"].iter().any(|w| token.word(w))
                || matches!(token.text, "=" | "<>" | "!=" | "<" | ">" | "<=" | ">=" | "!<" | "!>")
            {
                3
            } else if matches!(token.text, "+" | "-" | "&" | "|" | "^") {
                4
            } else if matches!(token.text, "*" | "/" | "%") {
                5
            } else {
                break;
            };
            if bp < min {
                break;
            }
            if self.take("IS") {
                self.take("NOT");
                self.require("NULL")?;
            } else if bp == 3 && ["IN", "LIKE", "BETWEEN", "NOT"].iter().any(|w| token.word(w)) {
                self.take("NOT");
                if self.take("IN") {
                    self.require("(")?;
                    loop {
                        self.expr(0, depth + 1)?;
                        if !self.take(",") {
                            break;
                        }
                    }
                    self.require(")")?;
                } else if self.take("BETWEEN") {
                    self.expr(4, depth + 1)?;
                    self.require("AND")?;
                    self.expr(4, depth + 1)?;
                } else if self.take("LIKE") {
                    self.expr(4, depth + 1)?;
                    if self.take("ESCAPE") {
                        self.expr(4, depth + 1)?;
                    }
                } else {
                    return Err("Expected IN, BETWEEN or LIKE after NOT".into());
                }
            } else {
                self.at += 1;
                self.expr(bp + 1, depth + 1)?;
            }
        }
        Ok(())
    }
}

pub(crate) fn validate_expression(text: &str, order_by: bool) -> Result<(), String> {
    if text.trim().is_empty() {
        return Ok(());
    }
    if text.len() > 65_536 {
        return Err("T-SQL expression exceeds 64 KiB".into());
    }
    let tokens = lex(text)?;
    if tokens.len() > 4096 {
        return Err("T-SQL expression has too many tokens".into());
    }
    if tokens.iter().any(|t| t.comment() || t.text == ";") {
        return Err("Comments and statement separators are unsupported in T-SQL expressions".into());
    }
    let mut parser = Expression { tokens: &tokens, at: 0 };
    loop {
        parser.expr(0, 0)?;
        if !order_by {
            break;
        }
        if !parser.take("ASC") {
            parser.take("DESC");
        }
        if !parser.take(",") {
            break;
        }
    }
    if parser.at == tokens.len() {
        Ok(())
    } else {
        Err(format!("Unsupported trailing T-SQL expression syntax at byte {}", tokens[parser.at].start))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expression_subset_consumes_predicates_arithmetic_and_scalar_calls() {
        for text in [
            "",
            "id = 1",
            "([Price] * quantity + 2) >= .5e+2 AND active = 1",
            "name IS NOT NULL OR id IS NULL",
            "NOT (id BETWEEN 1 AND 10 OR id NOT IN (11, 12, 13))",
            "name NOT LIKE N'%x~_%' ESCAPE N'~'",
            "LOWER([Name]) = LOWER(N'A') AND LEN([Name]) > 2",
            "id = @id",
            "data = 0xDEADBEEF",
            "data = 0x",
            "date >= CURRENT_TIMESTAMP",
            "dbo.[t].[id] <= -12.25",
            "amount & 1 = 0",
            "COALESCE(a, ISNULL(b, 0)) > 0",
            "[EXEC] = N'GO\nDELETE FROM x; -- /*'",
            "\"DROP\" = 'it''s safe'",
            "name = N'\\'",
        ] {
            assert!(validate_expression(text, false).is_ok(), "{text}");
        }
        for text in ["[Name] DESC, id ASC", "LOWER([Name]) DESC, id", "(price * quantity) DESC", "名 ASC"] {
            assert!(validate_expression(text, true).is_ok(), "{text}");
        }
    }

    #[test]
    fn raw_fragments_refuse_statement_injection_without_a_semicolon() {
        for text in [
            "1=1 DELETE FROM dbo.t",
            "id=1 UPDATE dbo.t SET x=2",
            "1=1 INSERT INTO t VALUES (1)",
            "1=1 DROP TABLE t",
            "1=1 ALTER TABLE t ADD a int",
            "1=1 CREATE TABLE t (id int)",
            "1=1 EXEC(N'DROP TABLE t')",
            "1=1 USE master",
            "1=1 DBCC CHECKDB",
            "1=1 WAITFOR DELAY '00:01'",
            "1=1 SELECT 2",
            "1=1 COMMIT TRAN",
            "1=1 ROLLBACK",
            "1=1 BEGIN TRAN",
            "1=1 PRINT 'x'",
            "1=1 DECLARE @x int",
            "1=1; SELECT 2",
            "1=1\nGO\nDELETE t",
            "1=1 -- hidden",
            "1=1 /* hidden */",
            "1=1 */ DELETE t /*",
            "1=1 UNION SELECT 2",
            "id IN (SELECT id FROM t)",
            "EXISTS (SELECT 1)",
            "id=1 OPTION(RECOMPILE)",
            "[id]=1) DELETE t; --",
            "EXEC('delete t')",
            "OPENROWSET('a','b','c')=1",
            "dbo.f()=1",
            "[f]()=1",
            "name = 'open",
            "name = N'open",
            "[id = 1",
            "id = 1e",
            "id += 1",
            "id = 1 2",
            "CASE WHEN id=1 THEN 2 ELSE 3 END=2",
            "CAST(id AS int)=1",
            "name COLLATE Latin1_General_BIN = 'a'",
        ] {
            assert!(validate_expression(text, false).is_err(), "accepted {text:?}");
        }
        for text in [
            "id DESC DELETE FROM t",
            "id OFFSET 0 ROWS FETCH NEXT 1 ROWS ONLY",
            "id DESC; SELECT 1",
            "id, EXEC('x')",
            "id DESC -- tail",
            "id NULLS LAST",
            "id ASC DESC",
            "id,",
        ] {
            assert!(validate_expression(text, true).is_err(), "accepted order {text:?}");
        }
    }

    #[test]
    fn expression_resource_limits_refuse_deep_or_large_input() {
        let nested = format!("{}id{}", "(".repeat(66), ")".repeat(66));
        assert!(validate_expression(&nested, false).is_err());
        assert!(validate_expression(&format!("{}id=1", "NOT ".repeat(66)), false).is_err());
        assert!(validate_expression(&"a".repeat(65_537), false).is_err());
        assert!(
            validate_expression(&std::iter::repeat_n("id=1", 1500).collect::<Vec<_>>().join(" OR "), false).is_err()
        );
    }
}
