use super::lex::{LexKind, lex};

pub(crate) fn parse(text: &str) -> Option<Vec<(String, bool)>> {
    let tokens: Vec<_> = lex(text).into_iter().filter(|t| t.kind != LexKind::Space).collect();
    if tokens.is_empty() {
        return Some(vec![]);
    }
    let mut keys = Vec::new();
    let mut at = 0;
    while at < tokens.len() {
        let token = &tokens[at];
        let raw = &text[token.span.clone()];
        let column = match token.kind {
            LexKind::Word => raw.to_lowercase(),
            LexKind::QuotedIdent if raw.starts_with('"') && raw.ends_with('"') && raw.len() >= 2 => {
                raw[1..raw.len() - 1].replace("\"\"", "\"")
            }
            _ => return None,
        };
        at += 1;
        let mut desc = false;
        if let Some(token) = tokens.get(at) {
            let raw = &text[token.span.clone()];
            if token.kind == LexKind::Word && (raw.eq_ignore_ascii_case("ASC") || raw.eq_ignore_ascii_case("DESC")) {
                desc = raw.eq_ignore_ascii_case("DESC");
                at += 1;
            }
        }
        keys.push((column, desc));
        if at == tokens.len() {
            break;
        }
        if tokens[at].kind != LexKind::Comma || at + 1 == tokens.len() {
            return None;
        }
        at += 1;
    }
    Some(keys)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn simple_columns_and_directions_preserve_quoted_case() {
        assert_eq!(
            parse("SomeName DESC, \"a\"\"b\" asc"),
            Some(vec![("somename".into(), true), ("a\"b".into(), false)])
        );
        for advanced in ["lower(name)", "a NULLS LAST", "a; select 1", "a -- hidden", "a,"] {
            assert_eq!(parse(advanced), None);
        }
    }
}
