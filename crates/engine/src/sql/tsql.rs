mod around;
mod expression;
mod format;
mod table;

pub(crate) use expression::validate_expression;
pub(crate) use format::format;
pub(crate) use table::{browse_sql, count_sql, format_order_by, parse_order_by};

use super::scope::{Class, Lexeme};
use super::{ByteTarget, DmlVerb, Invalidates, StmtKind, StmtSpan, relations::NameRef};
pub(super) use around::statement_around;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Word,
    Ident,
    /// A notebook cell reference, `{{name}}`.
    Reference,
    String,
    Number,
    Variable,
    Symbol,
    LineComment,
    BlockComment,
}

#[derive(Debug, Clone, Copy)]
struct Token<'a> {
    kind: Kind,
    text: &'a str,
    start: usize,
    end: usize,
}

impl Token<'_> {
    fn word(&self, word: &str) -> bool {
        self.kind == Kind::Word && self.text.eq_ignore_ascii_case(word)
    }

    fn comment(&self) -> bool {
        matches!(self.kind, Kind::LineComment | Kind::BlockComment)
    }

    fn code(&self) -> bool {
        !self.comment() && self.text != ";"
    }

    fn name(&self) -> Option<String> {
        match self.kind {
            Kind::Word => Some(self.text.to_owned()),
            Kind::Ident if self.text.starts_with('[') => Some(self.text[1..self.text.len() - 1].replace("]]", "]")),
            Kind::Ident => Some(self.text[1..self.text.len() - 1].replace("\"\"", "\"")),
            _ => None,
        }
    }
}

pub(crate) fn param_occurrences(text: &str) -> Result<Vec<std::ops::Range<usize>>, String> {
    let tokens: Vec<_> = lex(text)?.into_iter().filter(|t| !t.comment()).collect();
    let mut code = tokens.iter().filter(|t| t.code());
    let routine =
        code.clone().take(5).any(|t| {
            t.word("procedure") || t.word("proc") || t.word("function") || t.word("trigger") || t.word("rule")
        }) && code.next().is_some_and(|t| t.word("create") || t.word("alter"));
    if routine {
        return Ok(Vec::new());
    }
    let mut declared = std::collections::HashSet::new();
    for (i, token) in tokens.iter().enumerate() {
        if !token.word("declare") {
            continue;
        }
        let mut at = i + 1;
        while let Some(variable) = tokens.get(at).filter(|t| t.kind == Kind::Variable) {
            declared.insert(variable.text.to_lowercase());
            at += 1;
            let mut depth = 0usize;
            while let Some(next) = tokens.get(at) {
                if depth == 0 {
                    if next.text == "," && tokens.get(at + 1).is_some_and(|t| t.kind == Kind::Variable) {
                        at += 1;
                        break;
                    }
                    if next.text == ";"
                        || [
                            "select",
                            "set",
                            "declare",
                            "insert",
                            "update",
                            "delete",
                            "exec",
                            "execute",
                            "print",
                            "if",
                            "while",
                            "open",
                            "fetch",
                            "close",
                            "deallocate",
                            "begin",
                            "return",
                        ]
                        .iter()
                        .any(|word| next.word(word))
                    {
                        break;
                    }
                }
                match next.text {
                    "(" => depth += 1,
                    ")" => depth = depth.saturating_sub(1),
                    _ => {}
                }
                at += 1;
            }
        }
    }
    let mut exec = implicit_execution(&tokens);
    let mut out = Vec::new();
    for (i, token) in tokens.iter().enumerate() {
        if token.word("exec") || token.word("execute") {
            exec = true;
        } else if token.text == ";"
            || ["select", "set", "declare", "insert", "update", "delete", "print", "if", "while", "begin", "return"]
                .iter()
                .any(|word| token.word(word))
        {
            exec = false;
        }
        if token.kind == Kind::Variable
            && !token.text.starts_with("@@")
            && !declared.contains(&token.text.to_lowercase())
        {
            if exec && tokens.get(i + 1).is_some_and(|t| t.text == "=") {
                continue;
            }
            if token.text.len() == 1 {
                return Err("Invalid parameter @. Use a variable name such as @id.".into());
            }
            out.push(token.start..token.end);
        }
    }
    Ok(out)
}

fn word_start(ch: char) -> bool {
    ch.is_alphabetic() || matches!(ch, '_' | '@' | '#')
}

fn word_cont(ch: char) -> bool {
    word_start(ch) || ch.is_numeric() || ch == '$'
}

fn quoted_end(bytes: &[u8], start: usize, close: u8) -> Result<usize, String> {
    let mut i = start + 1;
    while i < bytes.len() {
        if bytes[i] == close {
            if bytes.get(i + 1) == Some(&close) {
                i += 2;
            } else {
                return Ok(i + 1);
            }
        } else {
            i += 1;
        }
    }
    Err(format!("Unclosed T-SQL quote or bracket at byte {start}"))
}

fn lex(text: &str) -> Result<Vec<Token<'_>>, String> {
    lex_with(text, false)
}

fn lex_unclosed_to_end(text: &str) -> Vec<Token<'_>> {
    lex_with(text, true).unwrap_or_default()
}

fn lex_with(text: &str, unclosed_to_end: bool) -> Result<Vec<Token<'_>>, String> {
    let quoted_end = |bytes: &[u8], start: usize, close: u8| match quoted_end(bytes, start, close) {
        Err(_) if unclosed_to_end => Ok(bytes.len()),
        end => end,
    };
    if text.contains('\0') && !unclosed_to_end {
        return Err("NUL is not supported in T-SQL".into());
    }
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let ch = text[i..].chars().next().unwrap();
        if ch.is_whitespace() {
            i += ch.len_utf8();
            continue;
        }
        let start = i;
        let kind = match bytes[i] {
            b'-' if bytes.get(i + 1) == Some(&b'-') => {
                i += 2;
                while i < bytes.len() && !matches!(bytes[i], b'\r' | b'\n') {
                    i += 1;
                }
                Kind::LineComment
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                let mut depth = 1usize;
                while i < bytes.len() && depth > 0 {
                    if bytes[i..].starts_with(b"/*") {
                        depth += 1;
                        i += 2;
                    } else if bytes[i..].starts_with(b"*/") {
                        depth -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                if depth != 0 && !unclosed_to_end {
                    return Err(format!("Unclosed T-SQL block comment at byte {start}"));
                }
                Kind::BlockComment
            }
            b'*' if bytes.get(i + 1) == Some(&b'/') && !unclosed_to_end => {
                return Err(format!("Unexpected comment terminator at byte {i}"));
            }
            b'\'' => {
                i = quoted_end(bytes, i, b'\'')?;
                Kind::String
            }
            b'N' | b'n' if bytes.get(i + 1) == Some(&b'\'') => {
                i = quoted_end(bytes, i + 1, b'\'')?;
                Kind::String
            }
            b'[' | b'"' => {
                i = quoted_end(bytes, i, if bytes[i] == b'[' { b']' } else { b'"' })?;
                Kind::Ident
            }
            b'{' => match super::refs::end_at(text, i) {
                Some(end) => {
                    i = end;
                    Kind::Reference
                }
                None => {
                    i += 1;
                    Kind::Symbol
                }
            },
            b'0' if matches!(bytes.get(i + 1), Some(b'x' | b'X')) => {
                i += 2;
                while i < bytes.len() && bytes[i].is_ascii_hexdigit() {
                    i += 1;
                }
                Kind::Number
            }
            byte if byte.is_ascii_digit() || (byte == b'.' && bytes.get(i + 1).is_some_and(u8::is_ascii_digit)) => {
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                if bytes.get(i) == Some(&b'.') {
                    i += 1;
                    while i < bytes.len() && bytes[i].is_ascii_digit() {
                        i += 1;
                    }
                }
                if matches!(bytes.get(i), Some(b'e' | b'E')) {
                    i += 1;
                    if matches!(bytes.get(i), Some(b'+' | b'-')) {
                        i += 1;
                    }
                    let digits = i;
                    while i < bytes.len() && bytes[i].is_ascii_digit() {
                        i += 1;
                    }
                    if i == digits && !unclosed_to_end {
                        return Err(format!("Incomplete T-SQL exponent at byte {start}"));
                    }
                }
                Kind::Number
            }
            byte if word_start(ch) => {
                i += ch.len_utf8();
                while let Some(next) = text[i..].chars().next().filter(|ch| word_cont(*ch)) {
                    i += next.len_utf8();
                }
                if byte == b'@' { Kind::Variable } else { Kind::Word }
            }
            _ => {
                i += ch.len_utf8();
                if i < bytes.len()
                    && matches!(
                        &bytes[start..=i],
                        b"<="
                            | b">="
                            | b"<>"
                            | b"!="
                            | b"!<"
                            | b"!>"
                            | b"+="
                            | b"-="
                            | b"*="
                            | b"/="
                            | b"%="
                            | b"&="
                            | b"^="
                            | b"|="
                            | b"::"
                            | b"!!"
                    )
                {
                    i += 1;
                }
                Kind::Symbol
            }
        };
        out.push(Token { kind, text: &text[start..i], start, end: i });
    }
    Ok(out)
}

fn line_bounds(text: &str, at: usize) -> (usize, usize, usize) {
    let start = text[..at].rfind(['\r', '\n']).map_or(0, |i| i + 1);
    let end = text[at..].find(['\r', '\n']).map_or(text.len(), |i| at + i);
    let mut after = end;
    if text.as_bytes().get(after) == Some(&b'\r') {
        after += 1;
    }
    if text.as_bytes().get(after) == Some(&b'\n') {
        after += 1;
    }
    (start, end, after)
}

fn separators(text: &str, tokens: &[Token<'_>]) -> Result<Vec<StmtSpan>, String> {
    let mut out = Vec::new();
    for (i, token) in tokens.iter().enumerate().filter(|(_, t)| !t.comment()) {
        if token.text.ends_with('$') && tokens.get(i + 1).is_some_and(|t| t.text == "(" && t.start == token.end) {
            return Err(format!("sqlcmd substitutions are unsupported at byte {}", token.start));
        }
        if !token.word("GO") && !matches!(token.text, ":" | "!!") {
            continue;
        }
        let (start, end, after) = line_bounds(text, token.start);
        let first_on_line = text[start..token.start].trim().is_empty();
        if first_on_line && matches!(token.text, ":" | "!!") {
            return Err(format!("sqlcmd directives are unsupported at byte {}", token.start));
        }
        if !first_on_line || !token.word("GO") {
            continue;
        }
        let mut separator_end = after;
        for next in tokens[i + 1..].iter().take_while(|t| t.start < end) {
            if !next.comment() {
                return Err("GO must be alone on its line. Repetition counts are unsupported".into());
            }
            separator_end = separator_end.max(next.end);
        }
        out.push(StmtSpan { start, end: separator_end });
    }
    Ok(out)
}

pub(crate) fn split_batches(text: &str) -> Result<Vec<StmtSpan>, String> {
    let tokens = lex(text)?;
    let separators = separators(text, &tokens)?;
    let mut spans = Vec::new();
    let mut start = 0;
    for separator in separators.into_iter().chain(std::iter::once(StmtSpan { start: text.len(), end: text.len() })) {
        let end = separator.start;
        let first = tokens.partition_point(|t| t.end <= start);
        if tokens[first..].iter().take_while(|t| t.start < end).any(Token::code) {
            let part = &text[start..end];
            let trimmed = part.trim();
            let leading = part.len() - part.trim_start().len();
            spans.push(StmtSpan { start: start + leading, end: start + leading + trimmed.len() });
        }
        start = separator.end;
    }
    Ok(spans)
}

pub(crate) fn validate_batch(text: &str) -> Result<(), String> {
    let tokens = lex(text)?;
    if !separators(text, &tokens)?.is_empty() {
        return Err("Expected one T-SQL batch without a GO separator".into());
    }
    if tokens.iter().any(|t| t.word("USE")) {
        return Err("USE is unsupported. Select the database through the console instead".into());
    }
    Ok(())
}

pub(crate) fn pick(text: &str, target: ByteTarget) -> Result<Vec<StmtSpan>, String> {
    let valid = |offset| text.is_char_boundary(offset);
    let picked = match target {
        ByteTarget::Selection { from, to } if from != to => {
            if from > to || !valid(from) || !valid(to) {
                return Err("Invalid T-SQL selection byte range".into());
            }
            Ok(split_batches(&text[from..to])?
                .into_iter()
                .map(|s| StmtSpan { start: s.start + from, end: s.end + from })
                .collect())
        }
        ByteTarget::Selection { from, .. } | ByteTarget::Cursor(from) => {
            if !valid(from) {
                return Err("Invalid T-SQL cursor byte offset".into());
            }
            let spans = split_batches(text)?;
            let tokens = lex(text)?;
            let (line_start, line_end, _) = line_bounds(text, from);
            Ok(spans
                .into_iter()
                .filter(|span| {
                    let first = tokens.partition_point(|t| t.end <= span.start);
                    let mut code = tokens[first..].iter().take_while(|t| t.start < span.end).filter(|t| t.code());
                    let Some(first) = code.next() else {
                        return false;
                    };
                    let last = code.last().unwrap_or(first);
                    first.start <= line_end && last.end >= line_start
                })
                .take(1)
                .collect())
        }
        ByteTarget::All => split_batches(text),
    }?;
    for span in &picked {
        validate_batch(span.text(text))?;
    }
    Ok(picked)
}

fn significant(text: &str) -> Result<Vec<Token<'_>>, String> {
    Ok(lex(text)?.into_iter().filter(|t| !t.comment()).collect())
}

fn word_at(tokens: &[Token<'_>], at: usize, word: &str) -> bool {
    tokens.get(at).is_some_and(|t| t.word(word))
}

fn transaction_at(tokens: &[Token<'_>], at: usize) -> Option<StmtKind> {
    let token = tokens.get(at)?;
    if at > 0 && tokens[at - 1].text == "." {
        return None;
    }
    let tran = |at| word_at(tokens, at, "TRAN") || word_at(tokens, at, "TRANSACTION");
    if token.word("BEGIN") && (tran(at + 1) || word_at(tokens, at + 1, "DISTRIBUTED") && tran(at + 2)) {
        Some(StmtKind::Begin)
    } else if token.word("COMMIT") {
        Some(StmtKind::Commit)
    } else if token.word("ROLLBACK") {
        let named = tran(at + 1) && tokens.get(at + 2).is_some_and(|t| t.name().is_some() || t.kind == Kind::Variable);
        Some(if named { StmtKind::RollbackTo } else { StmtKind::Rollback })
    } else if token.word("SAVE") && tran(at + 1) {
        Some(StmtKind::Savepoint)
    } else {
        None
    }
}

fn ddl_at(tokens: &[Token<'_>], at: usize) -> Option<StmtKind> {
    let token = &tokens[at];
    if !["CREATE", "ALTER", "DROP", "TRUNCATE", "GRANT", "DENY", "REVOKE"].iter().any(|w| token.word(w)) {
        return None;
    }
    let object = tokens[at + 1..]
        .iter()
        .find(|t| !["OR", "ALTER", "UNIQUE", "CLUSTERED", "NONCLUSTERED"].iter().any(|w| t.word(w)));
    let verb = token.text.to_ascii_uppercase();
    Some(StmtKind::Ddl(
        object
            .filter(|t| t.kind == Kind::Word)
            .map_or(verb.clone(), |t| format!("{verb} {}", t.text.to_ascii_uppercase())),
    ))
}

fn batch_schema_change(tokens: &[Token<'_>]) -> Option<StmtKind> {
    let mut changes = (0..tokens.len()).filter_map(|i| ddl_at(tokens, i));
    let first = changes.next()?;
    if matches!(&first, StmtKind::Ddl(tag) if tag.ends_with(" DATABASE")) {
        return Some(first);
    }
    Some(changes.find(|kind| matches!(kind, StmtKind::Ddl(tag) if tag.ends_with(" DATABASE"))).unwrap_or(first))
}

fn implicit_execution(tokens: &[Token<'_>]) -> bool {
    tokens
        .iter()
        .find(|t| t.code())
        .is_some_and(|t| matches!(t.kind, Kind::Word | Kind::Ident) && !STATEMENT_WORDS.iter().any(|w| t.word(w)))
}

fn opaque_execution(tokens: &[Token<'_>]) -> bool {
    tokens.iter().any(|t| t.word("EXEC") || t.word("EXECUTE")) || implicit_execution(tokens)
}

fn selects_into(tokens: &[Token<'_>]) -> bool {
    let mut selecting = vec![false];
    for token in tokens {
        match token.text {
            "(" => selecting.push(false),
            ")" if selecting.len() > 1 => {
                selecting.pop();
            }
            ";" => *selecting.last_mut().unwrap() = false,
            _ if token.word("SELECT") => *selecting.last_mut().unwrap() = true,
            _ if ["INSERT", "UPDATE", "DELETE", "MERGE"].iter().any(|word| token.word(word)) => {
                *selecting.last_mut().unwrap() = false;
            }
            _ if token.word("INTO") && *selecting.last().unwrap() => return true,
            _ => {}
        }
    }
    false
}

pub(crate) fn invalidates(text: &str) -> Invalidates {
    let Ok(tokens) = significant(text) else {
        return Invalidates::Nothing;
    };
    if opaque_execution(&tokens) {
        return Invalidates::Both;
    }
    let mut catalog = selects_into(&tokens);
    let mut databases = false;
    for kind in (0..tokens.len()).filter_map(|i| ddl_at(&tokens, i)) {
        match kind.invalidates() {
            Invalidates::Catalog => catalog = true,
            Invalidates::DatabaseList => databases = true,
            _ => {}
        }
    }
    match (catalog, databases) {
        (false, false) => Invalidates::Nothing,
        (true, false) => Invalidates::Catalog,
        (false, true) => Invalidates::DatabaseList,
        (true, true) => Invalidates::Both,
    }
}

pub(crate) fn classify(text: &str) -> StmtKind {
    let Ok(tokens) = significant(text) else {
        return StmtKind::Other("BATCH".into());
    };
    if let Some(kind) = batch_schema_change(&tokens) {
        return kind;
    }
    let first = tokens.iter().position(Token::code).unwrap_or(tokens.len());
    if let Some(kind) = transaction_at(&tokens, first) {
        return kind;
    }
    for (word, verb) in [
        ("INSERT", DmlVerb::Insert),
        ("UPDATE", DmlVerb::Update),
        ("DELETE", DmlVerb::Delete),
        ("MERGE", DmlVerb::Merge),
    ] {
        if tokens.iter().any(|t| t.word(word)) {
            return StmtKind::Dml(verb);
        }
    }
    if tokens.iter().any(|t| t.word("SELECT")) {
        return StmtKind::Query;
    }
    StmtKind::Other(
        tokens.get(first).filter(|t| t.kind == Kind::Word).map_or(String::new(), |t| t.text.to_ascii_uppercase()),
    )
}

pub(crate) fn is_write(text: &str) -> bool {
    significant(text).map_or(true, |tokens| {
        opaque_execution(&tokens)
            || tokens.iter().any(|t| {
                [
                    "INSERT", "UPDATE", "DELETE", "MERGE", "INTO", "CREATE", "ALTER", "DROP", "TRUNCATE", "GRANT",
                    "DENY", "REVOKE", "DBCC", "BULK", "BACKUP", "RESTORE",
                ]
                .iter()
                .any(|w| t.word(w))
            })
    })
}

pub(crate) fn changes_showplan(text: &str) -> bool {
    let Ok(tokens) = significant(text) else {
        return true;
    };
    tokens.iter().enumerate().any(|(i, token)| {
        token.word("SHOWPLAN")
            || token.word("SHOWPLAN_XML")
            || token.word("SHOWPLAN_ALL")
            || token.word("SHOWPLAN_TEXT")
            || token.word("STATISTICS") && (word_at(&tokens, i + 1, "XML") || word_at(&tokens, i + 1, "PROFILE"))
    })
}

pub(crate) fn rerun_refusal(text: &str) -> Option<&'static str> {
    const MAY_WRITE: &str = "The batch may write, so it does not run again.";
    let Ok(tokens) = significant(text) else { return Some(MAY_WRITE) };
    if is_write(text) {
        Some(MAY_WRITE)
    } else if (0..tokens.len()).any(|i| transaction_at(&tokens, i).is_some()) {
        Some("The batch begins or ends a transaction, so it does not run again.")
    } else if tokens.iter().any(|t| t.word("IMPLICIT_TRANSACTIONS")) {
        Some("The batch turns on implicit transactions, so it does not run again.")
    } else if (0..tokens.len())
        .any(|i| tokens[i].word("NEXT") && word_at(&tokens, i + 1, "VALUE") && word_at(&tokens, i + 2, "FOR"))
    {
        Some("The batch advances a sequence, so it does not run again.")
    } else if tokens.iter().any(|t| ["OPENQUERY", "OPENROWSET", "OPENDATASOURCE"].iter().any(|w| t.word(w))) {
        Some("The batch reads from another server, so it does not run again.")
    } else {
        None
    }
}

fn name_path(tokens: &[Token<'_>], mut at: usize) -> Option<(Vec<String>, usize)> {
    let mut names = vec![tokens.get(at)?.name()?];
    at += 1;
    while tokens.get(at).is_some_and(|t| t.text == ".") {
        names.push(tokens.get(at + 1)?.name()?);
        at += 2;
    }
    Some((names, at))
}

pub(crate) fn relation_names(text: &str) -> Vec<NameRef> {
    let Ok(tokens) = significant(text) else {
        return Vec::new();
    };
    let mut ctes = Vec::new();
    for (i, t) in tokens.iter().enumerate() {
        if t.word("AS") && tokens.get(i + 1).is_some_and(|t| t.text == "(") {
            let mut name_at = i.checked_sub(1);
            if name_at.is_some_and(|j| tokens[j].text == ")") {
                let mut depth = 1;
                let mut j = i - 1;
                while j > 0 && depth != 0 {
                    j -= 1;
                    match tokens[j].text {
                        ")" => depth += 1,
                        "(" => depth -= 1,
                        _ => {}
                    }
                }
                name_at = j.checked_sub(1);
            }
            if let Some(name) = name_at.and_then(|j| tokens[j].name()) {
                ctes.push(name);
            }
        }
    }
    let mut found = Vec::new();
    let mut from_levels = vec![false];
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Expect {
        None,
        From,
        Target,
    }
    let mut expect = Expect::None;
    let mut i = 0;
    while i < tokens.len() {
        let t = tokens[i];
        match t.text {
            "(" => {
                from_levels.push(false);
                expect = Expect::None;
            }
            ")" => {
                if from_levels.len() > 1 {
                    from_levels.pop();
                }
                expect = Expect::None;
            }
            "," if *from_levels.last().unwrap() => expect = Expect::From,
            ";" => {
                *from_levels.last_mut().unwrap() = false;
                expect = Expect::None;
            }
            _ if t.word("FROM") || t.word("JOIN") || t.word("APPLY") => {
                *from_levels.last_mut().unwrap() = true;
                expect = Expect::From;
            }
            _ if ["INTO", "UPDATE", "TABLE", "MERGE"].iter().any(|w| t.word(w)) => expect = Expect::Target,
            _ if [
                "WHERE",
                "ON",
                "SET",
                "GROUP",
                "ORDER",
                "HAVING",
                "UNION",
                "EXCEPT",
                "INTERSECT",
                "VALUES",
                "OUTPUT",
                "OPTION",
            ]
            .iter()
            .any(|w| t.word(w)) =>
            {
                *from_levels.last_mut().unwrap() = false;
                expect = Expect::None;
            }
            _ if expect != Expect::None => {
                let context = expect;
                expect = Expect::None;
                if let Some((names, after)) = name_path(&tokens, i) {
                    if names.len() <= 2
                        && !(context == Expect::From && tokens.get(after).is_some_and(|t| t.text == "("))
                    {
                        let name = NameRef {
                            schema: if names.len() == 2 { Some(names[0].clone()) } else { None },
                            name: names.last().unwrap().clone(),
                        };
                        if !(name.schema.is_none() && ctes.contains(&name.name)) && !found.contains(&name) {
                            found.push(name);
                        }
                    }
                    i = after - 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    found
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ModuleName {
    pub parts: Vec<String>,
    pub span: std::ops::Range<usize>,
    /// A trigger's `ON` table, as written.
    pub on: Option<(Vec<String>, std::ops::Range<usize>)>,
}

pub(crate) fn module_name(text: &str) -> Option<ModuleName> {
    let tokens = significant(text).ok()?;
    if !word_at(&tokens, 0, "CREATE") {
        return None;
    }
    let mut at = 1;
    if word_at(&tokens, at, "OR") && word_at(&tokens, at + 1, "ALTER") {
        at += 2;
    }
    if !["PROC", "PROCEDURE", "FUNCTION", "VIEW", "TRIGGER"].iter().any(|w| word_at(&tokens, at, w)) {
        return None;
    }
    let (parts, after) = name_path(&tokens, at + 1)?;
    let on = (word_at(&tokens, at, "TRIGGER") && word_at(&tokens, after, "ON"))
        .then(|| name_path(&tokens, after + 1))
        .flatten()
        .filter(|(on, _)| on.len() <= 2)
        .map(|(on, end)| (on, tokens[after + 1].start..tokens[end - 1].end));
    (parts.len() <= 2).then(|| ModuleName { parts, span: tokens[at + 1].start..tokens[after - 1].end, on })
}

/// The lexemes of `text[range]`, an unclosed quote or comment running to its end.
pub(super) fn completion_lexemes(text: &str, range: std::ops::Range<usize>) -> Vec<Lexeme> {
    let tokens = lex_unclosed_to_end(&text[range.clone()]);
    let class = |t: &Token<'_>| match t.kind {
        Kind::Word => Class::Word,
        Kind::Ident => Class::Name,
        Kind::Reference => Class::Reference,
        Kind::String => Class::Str,
        Kind::Number => Class::Number,
        Kind::Variable => Class::Param,
        Kind::LineComment => Class::LineComment,
        Kind::BlockComment => Class::BlockComment,
        Kind::Symbol => match t.text {
            "(" => Class::Open,
            ")" => Class::Close,
            "," => Class::Comma,
            "." => Class::Dot,
            ";" => Class::Semi,
            "*" => Class::Star,
            _ => Class::Other,
        },
    };
    tokens.iter().map(|t| Lexeme { class: class(t), span: range.start + t.start..range.start + t.end }).collect()
}

pub(super) fn lexeme_closed(lexeme: &str) -> bool {
    lex_unclosed_to_end(&format!("{lexeme} x")).first().is_some_and(|t| t.end == lexeme.len())
}

pub(crate) fn quote_ident(name: &str) -> String {
    format!("[{}]", name.replace(']', "]]"))
}

pub(crate) fn quote_literal(value: &str) -> String {
    format!("N'{}'", value.replace('\'', "''"))
}

/// SQL Server's reserved keywords, which a name must bracket. From "Reserved keywords (Transact-SQL)".
pub(crate) const RESERVED: &[&str] = &[
    "ADD",
    "ALL",
    "ALTER",
    "AND",
    "ANY",
    "AS",
    "ASC",
    "AUTHORIZATION",
    "BACKUP",
    "BEGIN",
    "BETWEEN",
    "BREAK",
    "BROWSE",
    "BULK",
    "BY",
    "CASCADE",
    "CASE",
    "CHECK",
    "CHECKPOINT",
    "CLOSE",
    "CLUSTERED",
    "COALESCE",
    "COLLATE",
    "COLUMN",
    "COMMIT",
    "COMPUTE",
    "CONSTRAINT",
    "CONTAINS",
    "CONTAINSTABLE",
    "CONTINUE",
    "CONVERT",
    "CREATE",
    "CROSS",
    "CURRENT",
    "CURRENT_DATE",
    "CURRENT_TIME",
    "CURRENT_TIMESTAMP",
    "CURRENT_USER",
    "CURSOR",
    "DATABASE",
    "DBCC",
    "DEALLOCATE",
    "DECLARE",
    "DEFAULT",
    "DELETE",
    "DENY",
    "DESC",
    "DISK",
    "DISTINCT",
    "DISTRIBUTED",
    "DOUBLE",
    "DROP",
    "DUMP",
    "ELSE",
    "END",
    "ERRLVL",
    "ESCAPE",
    "EXCEPT",
    "EXEC",
    "EXECUTE",
    "EXISTS",
    "EXIT",
    "EXTERNAL",
    "FETCH",
    "FILE",
    "FILLFACTOR",
    "FOR",
    "FOREIGN",
    "FREETEXT",
    "FREETEXTTABLE",
    "FROM",
    "FULL",
    "FUNCTION",
    "GOTO",
    "GRANT",
    "GROUP",
    "HAVING",
    "HOLDLOCK",
    "IDENTITY",
    "IDENTITYCOL",
    "IDENTITY_INSERT",
    "IF",
    "IN",
    "INDEX",
    "INNER",
    "INSERT",
    "INTERSECT",
    "INTO",
    "IS",
    "JOIN",
    "KEY",
    "KILL",
    "LEFT",
    "LIKE",
    "LINENO",
    "LOAD",
    "MERGE",
    "NATIONAL",
    "NOCHECK",
    "NONCLUSTERED",
    "NOT",
    "NULL",
    "NULLIF",
    "OF",
    "OFF",
    "OFFSETS",
    "ON",
    "OPEN",
    "OPENDATASOURCE",
    "OPENQUERY",
    "OPENROWSET",
    "OPENXML",
    "OPTION",
    "OR",
    "ORDER",
    "OUTER",
    "OVER",
    "PERCENT",
    "PIVOT",
    "PLAN",
    "PRECISION",
    "PRIMARY",
    "PRINT",
    "PROC",
    "PROCEDURE",
    "PUBLIC",
    "RAISERROR",
    "READ",
    "READTEXT",
    "RECONFIGURE",
    "REFERENCES",
    "REPLICATION",
    "RESTORE",
    "RESTRICT",
    "RETURN",
    "REVERT",
    "REVOKE",
    "RIGHT",
    "ROLLBACK",
    "ROWCOUNT",
    "ROWGUIDCOL",
    "RULE",
    "SAVE",
    "SCHEMA",
    "SECURITYAUDIT",
    "SELECT",
    "SEMANTICKEYPHRASETABLE",
    "SEMANTICSIMILARITYDETAILSTABLE",
    "SEMANTICSIMILARITYTABLE",
    "SESSION_USER",
    "SET",
    "SETUSER",
    "SHUTDOWN",
    "SOME",
    "STATISTICS",
    "SYSTEM_USER",
    "TABLE",
    "TABLESAMPLE",
    "TEXTSIZE",
    "THEN",
    "TO",
    "TOP",
    "TRAN",
    "TRANSACTION",
    "TRIGGER",
    "TRUNCATE",
    "TRY_CONVERT",
    "TSEQUAL",
    "UNION",
    "UNIQUE",
    "UNPIVOT",
    "UPDATE",
    "UPDATETEXT",
    "USE",
    "USER",
    "VALUES",
    "VARYING",
    "VIEW",
    "WAITFOR",
    "WHEN",
    "WHERE",
    "WHILE",
    "WITH",
    "WRITETEXT",
];

const STATEMENT_WORDS: &[&str] = &[
    "ALTER",
    "BACKUP",
    "BEGIN",
    "BREAK",
    "BULK",
    "CHECKPOINT",
    "CLOSE",
    "COMMIT",
    "CONTINUE",
    "CREATE",
    "DBCC",
    "DEALLOCATE",
    "DECLARE",
    "DELETE",
    "DENY",
    "DROP",
    "ELSE",
    "END",
    "EXEC",
    "EXECUTE",
    "FETCH",
    "GO",
    "GOTO",
    "GRANT",
    "IF",
    "INSERT",
    "KILL",
    "MERGE",
    "OPEN",
    "PRINT",
    "RAISERROR",
    "RECONFIGURE",
    "RESTORE",
    "RETURN",
    "REVERT",
    "REVOKE",
    "ROLLBACK",
    "SAVE",
    "SELECT",
    "SEND",
    "SET",
    "SHUTDOWN",
    "THROW",
    "TRUNCATE",
    "UPDATE",
    "USE",
    "WAITFOR",
    "WHILE",
    "WITH",
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql::Invalidates;

    fn batches(text: &str) -> Vec<&str> {
        split_batches(text).unwrap().iter().map(|s| s.text(text)).collect()
    }

    fn selected(text: &str, target: ByteTarget) -> Vec<&str> {
        pick(text, target).unwrap().iter().map(|s| s.text(text)).collect()
    }

    #[test]
    fn empty_binary_literal_is_a_valid_batch_and_formats_without_changing_it() {
        assert_eq!(selected("SELECT 0x", ByteTarget::All), ["SELECT 0x"]);
        assert_eq!(format("SELECT 0x").unwrap(), "SELECT 0x");
    }

    #[test]
    fn only_go_splits_batches_and_preserves_variable_and_procedure_scope() {
        let declare = "DECLARE @x int = 1;\n\nSELECT @x; SELECT 2;";
        let procedure = "CREATE OR ALTER PROCEDURE [dbo].[p] AS\nBEGIN\nDECLARE @n int; SELECT @n;\nEND;";
        assert_eq!(batches(declare), [declare]);
        assert_eq!(batches(procedure), [procedure]);
        let text = format!("  {declare}\r\ngO -- next batch\r\n{procedure}  ");
        assert_eq!(batches(&text), [declare, procedure]);
        assert_eq!(batches("SELECT 1\rGO\rSELECT 2"), ["SELECT 1", "SELECT 2"]);
        assert!(batches("-- note\nGO\n/* nested /* x */ */\nGO\n;;").is_empty());
    }

    #[test]
    fn go_inside_every_tsql_quote_and_comment_form_stays_in_its_batch() {
        for text in [
            "SELECT 'first''\nGO\nlast';",
            "SELECT N'first''\nGO\nlast';",
            "SELECT n'GO';",
            "SELECT \"first\"\"\nGO\nlast\";",
            "SELECT [first]]\nGO\nlast];",
            "SELECT 1; -- GO\nSELECT 2;",
            "SELECT 1; /*\nGO\n/* nested */\nGO\n*/ SELECT 2;",
            "SELECT @GO, #GO, [GO], N'--GO', '/*GO*/';",
            "SELECT 'backslash\\'; SELECT 2;",
        ] {
            assert_eq!(batches(text), [text], "{text}");
        }
        assert_eq!(batches("SELECT 1\nGO /* next */ -- note\nSELECT 2"), ["SELECT 1", "SELECT 2"]);
        assert_eq!(batches("SELECT 1\nGO /* next\n/* nested */ */ SELECT 2"), ["SELECT 1", "SELECT 2"]);
    }

    #[test]
    fn malformed_text_counts_and_sqlcmd_are_rejected_before_any_batch_runs() {
        for tail in [
            "GO 2",
            "go 0",
            "GO -1",
            "GO +3",
            "GO 1.5",
            "GO something",
            "GO /* comment */ 2",
            ":r file.sql",
            ":setvar name value",
            "!! echo shell",
            "SELECT $(name)",
            "SELECT prefix$(name)",
            "SELECT 'open",
            "SELECT N'open",
            "SELECT \"open",
            "SELECT [open",
            "/* open",
            "/* nested /* open */",
            "*/",
            "SELECT '\0'",
        ] {
            let text = format!("SELECT 1\nGO\n{tail}");
            assert!(split_batches(&text).is_err(), "accepted {tail:?}");
        }
        assert_eq!(
            batches("SELECT N':r file', [$(name)], 'GO 2' -- !!\n"),
            ["SELECT N':r file', [$(name)], 'GO 2' -- !!"]
        );
    }

    #[test]
    fn spans_are_original_utf8_bytes_and_selections_never_expand() {
        let text = "-- café\nSELECT N'😀';\nGO\nSELECT [日本語] FROM [Å].[旅];\n";
        let spans = split_batches(text).unwrap();
        assert_eq!(spans[0].start, 0);
        assert_eq!(spans[0].end, text.find("\nGO").unwrap());
        assert_eq!(spans[1].text(text), "SELECT [日本語] FROM [Å].[旅];");
        let from = text.find("N'😀'").unwrap();
        let to = from + "N'😀'".len();
        assert_eq!(pick(text, ByteTarget::Selection { from, to }).unwrap(), [StmtSpan { start: from, end: to }]);
        let from = text.find("SELECT N").unwrap();
        let to = text.find(" FROM").unwrap();
        assert_eq!(selected(text, ByteTarget::Selection { from, to }), ["SELECT N'😀';", "SELECT [日本語]"]);
        assert_eq!(selected("'unclosed\nSELECT 1", ByteTarget::Selection { from: 10, to: 18 }), ["SELECT 1"]);
        assert!(pick(text, ByteTarget::Cursor(text.find('😀').unwrap() + 1)).is_err());
        assert!(pick(text, ByteTarget::Cursor(text.len() + 1)).is_err());
        assert!(pick(text, ByteTarget::Selection { from: 20, to: 1 }).is_err());
    }

    #[test]
    fn cursor_picks_the_batch_but_not_the_trivia_between_batches() {
        let text =
            "-- header\nDECLARE @x int;\n\nSELECT @x;\n-- footer\n\nGO -- split\n\n/* header */\nSELECT 2;\n-- end\n";
        let spans = split_batches(text).unwrap();
        for needle in ["DECLARE", "\n\nSELECT", "SELECT @x"] {
            assert_eq!(pick(text, ByteTarget::Cursor(text.find(needle).unwrap())).unwrap(), [spans[0]], "{needle}");
        }
        for needle in ["-- header", "-- footer", "GO", "/* header */", "-- end"] {
            assert!(pick(text, ByteTarget::Cursor(text.find(needle).unwrap())).unwrap().is_empty(), "{needle}");
        }
        assert!(pick(text, ByteTarget::Cursor(text.len())).unwrap().is_empty());
        assert_eq!(selected("SELECT 1", ByteTarget::Selection { from: 2, to: 2 }), ["SELECT 1"]);
    }

    #[test]
    fn postgres_brackets_keep_their_existing_meaning() {
        assert_eq!(super::super::split_statements("SELECT ARRAY[1, 2]; SELECT 3").len(), 2);
        assert_eq!(batches("SELECT [a;b]; SELECT 3").len(), 1);
    }

    #[test]
    fn batch_validation_rejects_use_but_preserves_quoted_text() {
        for text in [
            "USE master",
            "use [db]",
            "SELECT 1; USE master",
            "IF 1=1 USE master",
            "SELECT 1\nGO\nSELECT 2",
            "SELECT 'open",
            ":r file.sql",
            "GO 2",
        ] {
            assert!(validate_batch(text).is_err(), "accepted {text:?}");
        }
        for text in [
            "SELECT N'USE master', [USE], \"USE\", @USE, #USE",
            "SELECT useful FROM reuse",
            "/* USE master */ SELECT 1 -- USE tempdb",
        ] {
            assert!(validate_batch(text).is_ok(), "{text:?}");
        }
    }

    #[test]
    fn planning_validates_all_picked_batches_before_returning_any() {
        let text = "SELECT 1\nGO\nUSE master\nGO\nSELECT 2";
        assert!(pick(text, ByteTarget::All).is_err());
        assert!(pick(text, ByteTarget::Selection { from: 0, to: text.len() }).is_err());
        assert_eq!(selected(text, ByteTarget::Cursor(0)), ["SELECT 1"]);
        let from = text.find("SELECT 2").unwrap();
        assert_eq!(selected(text, ByteTarget::Selection { from, to: text.len() }), ["SELECT 2"]);
        let text = "USE master\nSELECT 'unclosed\nSELECT 1";
        let from = text.rfind("SELECT 1").unwrap();
        assert_eq!(selected(text, ByteTarget::Selection { from, to: text.len() }), ["SELECT 1"]);
    }

    #[test]
    fn transactions_are_distinct_from_procedural_blocks_and_quoted_words() {
        for text in [
            "BEGIN",
            "BEGIN SELECT 1; END",
            "BEGIN TRY SELECT 1 END TRY BEGIN CATCH SELECT 2 END CATCH",
            "SELECT [COMMIT], \"ROLLBACK\", N'BEGIN TRAN', @COMMIT; /* SAVE TRAN */",
        ] {
            assert!(!matches!(classify(text), StmtKind::Begin | StmtKind::Commit | StmtKind::Rollback));
        }
        for (text, kind) in [
            ("BEGIN TRAN", StmtKind::Begin),
            ("BEGIN /* x */ TRANSACTION named", StmtKind::Begin),
            ("BEGIN DISTRIBUTED TRANSACTION", StmtKind::Begin),
            ("COMMIT", StmtKind::Commit),
            ("COMMIT WORK", StmtKind::Commit),
            ("ROLLBACK TRANSACTION", StmtKind::Rollback),
            ("ROLLBACK TRAN [save]", StmtKind::RollbackTo),
            ("SAVE TRANSACTION [save]", StmtKind::Savepoint),
        ] {
            assert_eq!(classify(text), kind, "{text}");
        }
    }

    #[test]
    fn advisory_classification_accounts_for_writes_and_ddl_later_in_a_batch() {
        assert_eq!(classify("DECLARE @x int = 1; SELECT @x"), StmtKind::Query);
        assert_eq!(classify("WITH c AS (SELECT 1 AS id) UPDATE t SET id = 1"), StmtKind::Dml(DmlVerb::Update));
        assert_eq!(
            classify("SELECT 1; CREATE OR ALTER PROCEDURE p AS SELECT 2"),
            StmtKind::Ddl("CREATE PROCEDURE".into())
        );
        assert_eq!(invalidates("SELECT 1; ALTER TABLE t ADD x int"), Invalidates::Catalog);
        assert_eq!(invalidates("CREATE TABLE t (id int); DROP DATABASE d"), Invalidates::Both);
        assert!(is_write("SELECT 1; INSERT INTO t VALUES (1)"));
        assert!(is_write("SELECT id INTO #copy FROM t"));
        assert!(is_write("EXEC p"));
        assert!(!is_write("SELECT [UPDATE], N'DROP TABLE t', @INSERT /* DELETE */"));
    }

    #[test]
    fn refresh_obligations_accumulate_independently_of_batch_classification() {
        for (text, expected) in [
            ("SELECT 1", Invalidates::Nothing),
            ("CREATE TABLE t (id int)", Invalidates::Catalog),
            ("DROP DATABASE IF EXISTS d", Invalidates::DatabaseList),
            ("ALTER DATABASE d SET READ_WRITE; ALTER TABLE t ADD n int", Invalidates::Both),
            ("CREATE PROCEDURE p AS BEGIN DROP DATABASE IF EXISTS d; END", Invalidates::Both),
            ("CREATE OR ALTER PROCEDURE p AS BEGIN DROP DATABASE IF EXISTS d; END", Invalidates::Both),
            ("SELECT N'CREATE TABLE t'; /* ALTER DATABASE d */ SELECT [DROP], @CREATE", Invalidates::Nothing),
        ] {
            assert_eq!(invalidates(text), expected, "{text}");
        }
    }

    #[test]
    fn opaque_execution_can_change_both_metadata_snapshots_and_counts_as_a_write() {
        for text in [
            "EXEC dbo.p",
            "execute sys.sp_executesql N'CREATE TABLE dbo.t (id int)'",
            "SELECT 1; EXEC sys.sp_rename N'dbo.t', N'renamed'",
            "DECLARE @sql nvarchar(max)=N'DROP DATABASE d'; EXEC(@sql)",
            "IF 1=1 EXECUTE @procedure",
            "EXECUTE AS USER = N'other'",
            "dbo.p @x=1",
            "p",
            "/* header /* nested */ */ [dbo].[p] @x=N'readable'",
            "-- header\n\"dbo\".\"p\"",
            ";; #temporary_procedure",
            "dbo.旅",
        ] {
            assert_eq!(invalidates(text), Invalidates::Both, "{text}");
            assert!(is_write(text), "{text}");
        }
    }

    #[test]
    fn ordinary_statements_and_quoted_execution_words_do_not_imply_opaque_calls() {
        for text in [
            "",
            ";; -- EXEC dbo.p\n/* EXECUTE dbo.p */",
            "SELECT 1",
            "SELECT [EXEC], \"EXECUTE\", N'EXEC dbo.p', 'EXECUTE dbo.p', @EXEC, #EXECUTE",
            "SELECT 1 AS execution_count /* nested /* EXEC */ EXECUTE */ -- EXEC dbo.p",
            "DECLARE @sql nvarchar(max)=N'EXEC dbo.p'",
            "PRINT N'EXECUTE dbo.p'",
            "SET NOCOUNT ON",
            "BEGIN TRANSACTION",
            "COMMIT TRANSACTION",
            "ROLLBACK TRANSACTION",
            "WITH c AS (SELECT 1 AS id) SELECT * FROM c",
            "IF 1=1 SELECT 1",
        ] {
            assert_eq!(invalidates(text), Invalidates::Nothing, "{text}");
            assert!(!is_write(text), "{text}");
        }
    }

    #[test]
    fn select_into_creates_catalog_objects_without_treating_insert_or_output_into_as_ddl() {
        for text in [
            "SELECT 7 AS id INTO dbo.t",
            "IF 1=1 SELECT (SELECT 7) AS id INTO dbo.t",
            "WITH c AS (SELECT 7 AS id) SELECT id INTO reporting.dbo.t FROM c",
            "SELECT 1; SELECT N'value' AS v INTO #local",
            "BEGIN TRANSACTION; SELECT 7 AS id INTO dbo.t; COMMIT",
        ] {
            assert_eq!(invalidates(text), Invalidates::Catalog, "{text}");
            assert!(is_write(text), "{text}");
        }
        for text in [
            "SELECT 7 AS id",
            "SELECT N'INTO', [INTO], @INTO /* INTO dbo.t */",
            "SELECT 1; INSERT INTO dbo.t VALUES (1)",
            "SELECT 1 UPDATE t SET n=(SELECT 7) OUTPUT inserted.n INTO audit",
            "SELECT 1 DELETE FROM t OUTPUT deleted.n INTO audit",
            "INSERT INTO dbo.t SELECT 7",
            "UPDATE t SET n=(SELECT 7) OUTPUT inserted.n INTO audit",
            "MERGE INTO t USING (SELECT 7 AS n) s ON t.n=s.n WHEN MATCHED THEN UPDATE SET n=s.n OUTPUT inserted.n INTO audit;",
        ] {
            assert_eq!(invalidates(text), Invalidates::Nothing, "{text}");
        }
    }

    #[test]
    fn only_plain_reads_run_again_for_an_export() {
        for read in [
            "SELECT * FROM dbo.orders WHERE note = N'BEGIN TRAN; COMMIT'",
            "DECLARE @n int = 3; SELECT TOP (@n) * FROM dbo.orders; SELECT 1 AS [next value for]",
            "WITH t AS (SELECT 1 AS x) SELECT x FROM t -- ROLLBACK",
            "SET DATEFORMAT dmy; SELECT CONVERT(datetime, N'02/01/2024')",
        ] {
            assert_eq!(rerun_refusal(read), None, "{read}");
        }
        for (batch, why) in [
            ("UPDATE dbo.orders SET total = 0", "may write"),
            ("SELECT * INTO #copy FROM dbo.orders", "may write"),
            ("EXEC dbo.report", "may write"),
            ("sp_who", "may write"),
            ("SELECT 1; BEGIN TRANSACTION", "begins or ends a transaction"),
            ("SELECT 1; COMMIT", "begins or ends a transaction"),
            ("SAVE TRAN s; SELECT 1", "begins or ends a transaction"),
            ("SET IMPLICIT_TRANSACTIONS ON; SELECT 1", "implicit transactions"),
            ("SELECT NEXT VALUE FOR dbo.ids", "advances a sequence"),
            ("SELECT * FROM OPENQUERY(remote, 'SELECT 1')", "another server"),
            ("SELECT * FROM OPENROWSET('SQLNCLI', 'x', 'SELECT 1') AS r", "another server"),
            ("SELECT * FROM OPENDATASOURCE('SQLNCLI', 'x').db.dbo.t", "another server"),
            ("SELECT 'unterminated", "may write"),
        ] {
            assert!(rerun_refusal(batch).is_some_and(|r| r.contains(why)), "{batch}: {:?}", rerun_refusal(batch));
        }
    }

    #[test]
    fn relation_names_preserve_case_quoting_temp_tables_and_cte_exclusions() {
        assert_eq!(
            relation_names("SELECT * FROM [Å].[旅]]人] JOIN \"Sales\".\"Order\" ON 1=1"),
            [
                NameRef { schema: Some("Å".into()), name: "旅]人".into() },
                NameRef { schema: Some("Sales".into()), name: "Order".into() },
            ]
        );
        assert_eq!(
            relation_names("INSERT INTO [dbo].[T] ([id]) SELECT id FROM #Temp"),
            [NameRef { schema: Some("dbo".into()), name: "T".into() }, NameRef { schema: None, name: "#Temp".into() },]
        );
        assert_eq!(
            relation_names("WITH C (id) AS (SELECT id FROM dbo.Source) SELECT * FROM C, dbo.Other JOIN dbo.f() ON 1=1"),
            [
                NameRef { schema: Some("dbo".into()), name: "Source".into() },
                NameRef { schema: Some("dbo".into()), name: "Other".into() },
            ]
        );
        assert!(relation_names("SELECT N'FROM bad' -- FROM worse\n").is_empty());
        assert!(relation_names("SELECT * FROM OtherDb.dbo.T").is_empty());
    }

    #[test]
    fn unicode_identifiers_and_literals_are_quoted_without_case_folding() {
        assert_eq!(quote_ident("Å].旅"), "[Å]].旅]");
        assert_eq!(quote_ident(""), "[]");
        assert_eq!(quote_literal("旅's 😀\\"), "N'旅''s 😀\\'");
        assert_eq!(quote_literal(""), "N''");
        assert!(validate_expression("[旅]=N'😀' AND 名称=N'日本語'", false).is_ok());
    }

    #[test]
    fn unicode_boundaries_cannot_hide_statement_tokens_or_panic_the_lexer() {
        for whitespace in ['\u{a0}', '\u{2003}', '\u{2028}', '\u{feff}'] {
            for fragment in
                [format!("id{whitespace}DESC{whitespace}DELETE{whitespace}t"), format!("id=1{whitespace}COMMIT")]
            {
                assert!(validate_expression(&fragment, true).is_err(), "{fragment:?}");
            }
        }
        for text in ["旅=名", "(旅)", "id+😀", ":旅", "SELECT $action", "SELECT $$pg$$"] {
            let _ = split_batches(text);
            let _ = validate_expression(text, false);
            let _ = format(text);
        }
    }

    #[test]
    fn module_name_finds_the_created_name_past_comments() {
        let name = |text: &'static str| module_name(text).map(|n| (n.parts, &text[n.span]));
        let parts = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<Vec<_>>();
        assert_eq!(
            name("-- note\n/* CREATE VIEW x */ CREATE PROCEDURE oldname AS SELECT 1"),
            Some((parts(&["oldname"]), "oldname"))
        );
        assert_eq!(
            name("CREATE OR ALTER PROC [we]]ird s].[p]]q] AS SELECT 1"),
            Some((parts(&["we]ird s", "p]q"]), "[we]]ird s].[p]]q]"))
        );
        assert_eq!(
            name("create function \"a\"\"b\" . \"c\" () RETURNS int AS BEGIN RETURN 1 END"),
            Some((parts(&["a\"b", "c"]), "\"a\"\"b\" . \"c\""))
        );
        assert_eq!(
            name("CREATE TRIGGER dbo.t ON dbo.x AFTER INSERT AS SELECT 1"),
            Some((parts(&["dbo", "t"]), "dbo.t"))
        );
        let text = "CREATE TRIGGER t ON /* x */ [old t] AFTER INSERT AS SELECT 1";
        let on = module_name(text).unwrap().on.unwrap();
        assert_eq!((on.0, &text[on.1]), (parts(&["old t"]), "[old t]"));
        assert_eq!(
            module_name("CREATE TRIGGER t ON DATABASE FOR CREATE_TABLE AS SELECT 1").unwrap().on.unwrap().0,
            parts(&["DATABASE"])
        );
        assert_eq!(module_name("CREATE PROCEDURE p AS SELECT 1").unwrap().on, None);
        assert_eq!(name("CREATE VIEW dbo./* gap */v AS SELECT 1"), Some((parts(&["dbo", "v"]), "dbo./* gap */v")));
        for text in [
            "ALTER VIEW dbo.v AS SELECT 1",
            "CREATE TABLE dbo.t (a int)",
            "CREATE PROCEDURE a.b.c AS SELECT 1",
            "CREATE PROCEDURE 'x' AS SELECT 1",
            "CREATE PROCEDURE p AS SELECT 'unclosed",
        ] {
            assert_eq!(module_name(text), None, "{text}");
        }
    }
}
