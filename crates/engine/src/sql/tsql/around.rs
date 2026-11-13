use std::ops::Range;

use super::{word_cont, word_start};

/// A word at depth 0 that the statement-start rules name, or `""` for any other word. It picks candidates by length and
/// compares with the standard library, so the scan allocates and copies nothing per word.
fn keyword(word: &[u8]) -> &'static str {
    let candidates: &[&'static str] = match word.len() {
        2 => &["AS", "IF", "OF", "ON", "OR"],
        3 => &["ALL", "FOR", "NOT", "SET", "TRY", "USE"],
        4 => &["DENY", "DROP", "ELSE", "EXEC", "THEN", "WITH"],
        5 => &["AFTER", "ALTER", "BEGIN", "CATCH", "GRANT", "MERGE", "PRINT", "THROW", "UNION", "WHILE"],
        6 => &["CREATE", "DELETE", "EXCEPT", "INSERT", "RETURN", "REVOKE", "SELECT", "UPDATE", "VALUES"],
        7 => &["DECLARE", "EXECUTE", "INSTEAD"],
        8 => &["TRUNCATE"],
        9 => &["INTERSECT", "RAISERROR"],
        _ => return "",
    };
    for k in candidates {
        if k.as_bytes().eq_ignore_ascii_case(word) {
            return k;
        }
    }
    ""
}

#[derive(Clone, Copy, PartialEq)]
enum Prev {
    None,
    Word(&'static str),
    Comma,
    Other,
}

/// What decides whether the next word starts a statement. T-SQL needs no `;`, so a statement verb that cannot continue the
/// statement before it starts the next one.
#[derive(Default)]
struct Statement {
    first: Option<&'static str>,
    /// The verb after a `WITH`'s CTEs, or the first word.
    verb: Option<&'static str>,
    source_begun: bool,
    update: bool,
    update_set: bool,
}

impl Statement {
    fn starts_new(&self, w: &'static str, prev: Prev) -> bool {
        if w.is_empty() || self.first.is_none() {
            return false;
        }
        let prev_word = match prev {
            Prev::Word(p) => p,
            _ => "",
        };
        let set_option = self.first == Some("SET");
        let continues = prev == Prev::Comma
            || [
                "UNION",
                "ALL",
                "EXCEPT",
                "INTERSECT",
                "FOR",
                "AFTER",
                "OF",
                "THEN",
                "INSTEAD",
                "OR",
                "NOT",
                "GRANT",
                "DENY",
                "REVOKE",
            ]
            .contains(&prev_word)
            || prev_word == "ON" && !set_option;
        let cte_body = self.first == Some("WITH") && self.verb.is_none();
        let insert_source = self.verb == Some("INSERT") && !self.source_begun;
        self.first.is_some()
            && !continues
            && match w {
                "SELECT" => !cte_body && !insert_source,
                "INSERT" | "UPDATE" | "DELETE" | "MERGE" => !cte_body,
                "SET" => !self.update || self.update_set,
                "EXEC" | "EXECUTE" => !insert_source,
                "IF" => !matches!(self.first, Some("DROP" | "ALTER" | "CREATE")),
                "WITH" => {
                    matches!(prev_word, "BEGIN" | "TRY" | "CATCH" | "ELSE")
                        || prev_word == "AS" && matches!(self.first, Some("CREATE" | "ALTER"))
                }
                "DECLARE" | "PRINT" | "WHILE" | "RETURN" | "BEGIN" | "CREATE" | "ALTER" | "DROP" | "TRUNCATE"
                | "THROW" | "RAISERROR" | "USE" | "GRANT" | "DENY" | "REVOKE" => true,
                _ => false,
            }
    }

    fn word(&mut self, w: &'static str) {
        if w.is_empty() && self.first.is_some() {
            return;
        }
        if self.first.is_none() {
            self.first = Some(w);
            if w != "WITH" {
                self.verb = Some(w);
            }
        } else if self.first == Some("WITH")
            && self.verb.is_none()
            && matches!(w, "SELECT" | "INSERT" | "UPDATE" | "DELETE" | "MERGE")
        {
            self.verb = Some(w);
        }
        self.source_begun |= matches!(w, "VALUES" | "SELECT" | "EXEC" | "EXECUTE");
        self.update_set = self.update && (self.update_set || w == "SET") && w != "UPDATE";
        self.update |= w == "UPDATE";
    }
}

/// The end of a `GO` line that starts at `line`, when the line holds only `GO` and comments.
fn go_line(b: &[u8], line: usize) -> Option<usize> {
    let mut i = line;
    while i < b.len() && matches!(b[i], b' ' | b'\t') {
        i += 1;
    }
    if !(b.get(i..i + 2).is_some_and(|w| w.eq_ignore_ascii_case(b"GO"))
        && b.get(i + 2).is_none_or(|c| !c.is_ascii_alphanumeric() && !matches!(c, b'_' | b'@' | b'#' | b'$')))
    {
        return None;
    }
    i += 2;
    loop {
        while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\r') {
            i += 1;
        }
        match b.get(i) {
            None | Some(b'\n') => return Some(i),
            Some(b'-') if b.get(i + 1) == Some(&b'-') => return Some(skip_line(b, i)),
            Some(b'/') if b.get(i + 1) == Some(&b'*') => i = skip_block(b, i),
            _ => return None,
        }
    }
}

fn skip_line(b: &[u8], i: usize) -> usize {
    b[i..].iter().position(|&c| c == b'\n').map_or(b.len(), |p| i + p)
}

fn skip_block(b: &[u8], mut i: usize) -> usize {
    let mut depth = 0usize;
    while i < b.len() {
        if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
            depth += 1;
            i += 2;
        } else if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
            depth -= 1;
            i += 2;
            if depth == 0 {
                return i;
            }
        } else {
            i += 1;
        }
    }
    b.len()
}

fn skip_quoted(b: &[u8], start: usize, close: u8) -> usize {
    let mut i = start + 1;
    while i < b.len() {
        if b[i] == close {
            if b.get(i + 1) == Some(&close) {
                i += 2;
            } else {
                return i + 1;
            }
        } else {
            i += 1;
        }
    }
    b.len()
}

/// The statement around `at` inside its `GO` batch. One pass over the bytes, with no tokens, so a long script stays cheap.
pub(in crate::sql) fn statement_around(text: &str, at: usize) -> Range<usize> {
    let b = text.as_bytes();
    let mut start = 0;
    let mut statement = Statement::default();
    let mut prev = Prev::None;
    let mut depth = 0usize;
    let mut line_start = true;
    let mut i = 0;
    while i < b.len() {
        if line_start {
            line_start = false;
            if let Some(end) = go_line(b, i) {
                if i >= at {
                    return start..i;
                }
                (start, statement, prev, depth) = (end, Statement::default(), Prev::None, 0);
                i = end;
                continue;
            }
        }
        let c = b[i];
        match c {
            b'\n' => {
                line_start = true;
                i += 1;
            }
            _ if c.is_ascii_whitespace() => i += 1,
            b'-' if b.get(i + 1) == Some(&b'-') => i = skip_line(b, i),
            b'/' if b.get(i + 1) == Some(&b'*') => i = skip_block(b, i),
            b'\'' => {
                i = skip_quoted(b, i, b'\'');
                prev = Prev::Other;
            }
            b'[' | b'"' => {
                i = skip_quoted(b, i, if c == b'[' { b']' } else { b'"' });
                prev = Prev::Other;
            }
            b'{' => {
                i = crate::sql::refs::end_at(text, i).unwrap_or(i + 1);
                prev = Prev::Other;
            }
            b'(' => {
                depth += 1;
                prev = Prev::Other;
                i += 1;
            }
            b')' => {
                depth = depth.saturating_sub(1);
                prev = Prev::Other;
                i += 1;
            }
            b',' => {
                prev = Prev::Comma;
                i += 1;
            }
            b';' if depth == 0 => {
                i += 1;
                if i > at {
                    return start..i;
                }
                (start, statement, prev) = (i, Statement::default(), Prev::None);
            }
            _ => {
                let word = if c.is_ascii() {
                    c.is_ascii_alphabetic() || matches!(c, b'_' | b'@' | b'#')
                } else {
                    text[i..].chars().next().is_some_and(word_start)
                };
                if !word {
                    prev = Prev::Other;
                    i += text[i..].chars().next().map_or(1, char::len_utf8);
                    continue;
                }
                let mut word_end = i + 1;
                while word_end < b.len() {
                    match b[word_end] {
                        b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'@' | b'#' | b'$' => word_end += 1,
                        0x80..
                            if !text.is_char_boundary(word_end)
                                || text[word_end..].chars().next().is_some_and(word_cont) =>
                        {
                            word_end += 1
                        }
                        _ => break,
                    }
                }
                let w = keyword(&b[i..word_end]);
                if depth == 0 {
                    if statement.starts_new(w, prev) {
                        if i > at {
                            return start..i;
                        }
                        (start, statement) = (i, Statement::default());
                    }
                    statement.word(w);
                }
                prev = Prev::Word(w);
                i = word_end;
            }
        }
    }
    start..b.len()
}

#[cfg(test)]
mod tests {
    use super::statement_around;

    fn around(marked: &str) -> String {
        let at = marked.find('|').expect("a | marks the cursor");
        let text = marked.replacen('|', "", 1);
        text[statement_around(&text, at)].trim().to_owned()
    }

    #[test]
    fn a_verb_starts_a_statement_in_any_case() {
        assert_eq!(around("select 1\nselect * from orders o where o.|"), "select * from orders o where o.");
        assert_eq!(around("Update orders Set total = 0\nSelect |"), "Select");
    }

    #[test]
    fn a_go_line_ends_the_batch_and_only_a_bare_go_line_does() {
        assert_eq!(around("SELECT * FROM orders o WHERE o.|\nGO\nSELECT 2"), "SELECT * FROM orders o WHERE o.");
        assert_eq!(around("SELECT 1\nGO\nSELECT * FROM orders o WHERE o.|"), "SELECT * FROM orders o WHERE o.");
        assert_eq!(around("SELECT * FROM orders o\nGO -- next\nWHERE o.|"), "WHERE o.");
        assert_eq!(around("SELECT * FROM orders o\nGO 5\nWHERE o.|"), "SELECT * FROM orders o\nGO 5\nWHERE o.");
        assert_eq!(
            around("SELECT * FROM orders o WHERE o.id = 1\n|GO\nSELECT 2"),
            "SELECT * FROM orders o WHERE o.id = 1"
        );
    }

    #[test]
    fn a_semicolon_or_go_inside_a_comment_bracket_string_or_parentheses_ends_nothing() {
        for marked in [
            "SELECT * FROM orders o -- ; SELECT\nWHERE o.|",
            "SELECT * FROM orders o /* ;\nGO\n*/ WHERE o.|",
            "SELECT * FROM [a;b\nGO\n] o WHERE o.|",
            "SELECT * FROM \"a;b\" o WHERE o.|",
            "SELECT * FROM [x]] SELECT y] o WHERE o.|",
            "SELECT 'it''s; SELECT\nGO\n' AS t, o.| FROM orders o",
            "SELECT N'x; SELECT' AS t, o.| FROM orders o",
            "SELECT (SELECT 1; ) AS t, o.| FROM orders o",
        ] {
            let text = marked.replacen('|', "", 1);
            assert_eq!(around(marked), text.trim(), "{marked:?}");
        }
    }
}
