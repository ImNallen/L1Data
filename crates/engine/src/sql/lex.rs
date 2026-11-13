use std::ops::Range;

use super::split::{dollar_tag, line_comment_end, skip_block_comment, skip_dollar_quoted, skip_quoted, skip_string};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LexKind {
    Space,
    LineComment,
    BlockComment,
    Word,
    /// `"…"` or `U&"…"`.
    QuotedIdent,
    /// `'…'`, or the same with an `E`, `B`, `X`, `N`, or `U&` prefix, with every `'…'` that continues it
    /// across a newline.
    String,
    DollarString,
    Number,
    /// `$1`.
    Param,
    /// A run of operator characters, lexed as Postgres lexes it, or `::`.
    Operator,
    Dot,
    Comma,
    Open,
    Close,
    OpenBracket,
    CloseBracket,
    Semicolon,
    /// A notebook cell reference, `{{name}}`.
    Reference,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Lexeme {
    pub kind: LexKind,
    pub span: Range<usize>,
}

pub(super) fn is_operator_char(c: u8) -> bool {
    b"+-*/<>=~!@#%^&|`?".contains(&c)
}

pub(super) fn is_ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c >= 0x80
}

pub(super) fn is_ident_cont(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'$' || c >= 0x80
}

pub(super) fn run(bytes: &[u8], mut i: usize, keep: impl Fn(u8) -> bool) -> usize {
    while i < bytes.len() && keep(bytes[i]) {
        i += 1;
    }
    i
}

/// How a plain `'…'` or `N'…'` literal reads a backslash. With `standard_conforming_strings` off,
/// which a tab may `SET`, it escapes as it does in `E'…'`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Strings {
    Standard,
    Escaped,
}

/// Lexes all of `sql`: the spans are contiguous, start at 0, and end at `sql.len()`.
pub(crate) fn lex(sql: &str) -> Vec<Lexeme> {
    lex_strings(sql, Strings::Standard)
}

pub(crate) fn lex_strings(sql: &str, strings: Strings) -> Vec<Lexeme> {
    lexemes(sql, strings).collect()
}

pub(crate) fn lexemes(sql: &str, strings: Strings) -> impl Iterator<Item = Lexeme> + '_ {
    let mut i = 0;
    std::iter::from_fn(move || {
        if i >= sql.len() {
            return None;
        }
        let (kind, end) = lexeme_at(sql, i, strings);
        let lexeme = Lexeme { kind, span: i..end };
        i = end;
        Some(lexeme)
    })
}

fn lexeme_at(sql: &str, i: usize, strings: Strings) -> (LexKind, usize) {
    use LexKind::*;
    let bytes = sql.as_bytes();
    let at = |k: usize| bytes.get(k).copied();
    let digit_at = |k: usize| at(k).is_some_and(|c| c.is_ascii_digit());
    match bytes[i] {
        c if c.is_ascii_whitespace() => (Space, run(bytes, i, |c| c.is_ascii_whitespace())),
        b'-' if at(i + 1) == Some(b'-') => (LineComment, line_comment_end(bytes, i)),
        b'/' if at(i + 1) == Some(b'*') => (BlockComment, skip_block_comment(bytes, i)),
        b'"' => (QuotedIdent, skip_quoted(bytes, i, b'"', false)),
        b'\'' => (String, skip_string(bytes, i, strings == Strings::Escaped)),
        b'$' => match dollar_tag(bytes, i) {
            Some(tag_len) => (DollarString, skip_dollar_quoted(bytes, i, tag_len)),
            None if digit_at(i + 1) => (Param, junk(bytes, run(bytes, i + 1, |c| c.is_ascii_digit()))),
            None => (Other, i + 1),
        },
        c if is_ident_start(c) => {
            let end = run(bytes, i + 1, is_ident_cont);
            let word = &sql[i..end];
            match at(end) {
                Some(b'\'') if matches!(word, "e" | "E" | "b" | "B" | "x" | "X" | "n" | "N") => {
                    let escapes = word.eq_ignore_ascii_case("e")
                        || (word.eq_ignore_ascii_case("n") && strings == Strings::Escaped);
                    (String, skip_string(bytes, end, escapes))
                }
                Some(b'&') if word.eq_ignore_ascii_case("u") && matches!(at(end + 1), Some(b'"' | b'\'')) => {
                    if bytes[end + 1] == b'"' {
                        (QuotedIdent, skip_quoted(bytes, end + 1, b'"', false))
                    } else {
                        (String, skip_string(bytes, end + 1, false))
                    }
                }
                _ => (Word, end),
            }
        }
        c if c.is_ascii_digit() => (Number, number_end(bytes, i)),
        b'.' if digit_at(i + 1) => (Number, number_end(bytes, i)),
        b'.' if at(i + 1) == Some(b'.') => (Other, i + 2), // scan.l's `dot_dot`, one token
        b':' if at(i + 1) == Some(b':') => (Operator, i + 2),
        c if is_operator_char(c) => (Operator, operator_end(sql, i)),
        b'.' => (Dot, i + 1),
        b',' => (Comma, i + 1),
        b'(' => (Open, i + 1),
        b')' => (Close, i + 1),
        b'[' => (OpenBracket, i + 1),
        b']' => (CloseBracket, i + 1),
        b';' => (Semicolon, i + 1),
        b'{' => super::refs::end_at(sql, i).map_or((Other, i + 1), |end| (Reference, end)),
        _ => (Other, i + 1),
    }
}

/// `1_000`, `1.5`, `.5`, `1.`, and `1.5e-3`, then any word glued on. Postgres reads `0x1F`, `0o17`,
/// and `0b1010` as one number and rejects every other glued word (`1abc`, `0b102`) as trailing junk,
/// so the word stays in the lexeme rather than become a word the layout could space apart.
fn number_end(bytes: &[u8], i: usize) -> usize {
    let mut j = decinteger_end(bytes, i);
    if bytes.get(j) == Some(&b'.') && bytes.get(j + 1) != Some(&b'.') {
        j = decinteger_end(bytes, j + 1);
    }
    if matches!(bytes.get(j), Some(b'e' | b'E')) {
        let digits = j + 1 + usize::from(matches!(bytes.get(j + 1), Some(b'+' | b'-')));
        let end = decinteger_end(bytes, digits);
        if end > digits {
            j = end;
        }
    }
    junk(bytes, j)
}

/// The end of `scan.l`'s `{decdigit}(_?{decdigit})*` at `i`, or `i` when no digit starts there.
fn decinteger_end(bytes: &[u8], i: usize) -> usize {
    let digit = |k: usize| bytes.get(k).is_some_and(u8::is_ascii_digit);
    if !digit(i) {
        return i;
    }
    let mut j = i + 1;
    loop {
        if digit(j) {
            j += 1;
        } else if bytes.get(j) == Some(&b'_') && digit(j + 1) {
            j += 2;
        } else {
            return j;
        }
    }
}

fn junk(bytes: &[u8], j: usize) -> usize {
    if !bytes.get(j).is_some_and(|&c| is_ident_start(c)) {
        return j;
    }
    let end = run(bytes, j, is_ident_cont);
    let unicode_escape = end == j + 1
        && matches!(bytes[j], b'u' | b'U')
        && bytes.get(end) == Some(&b'&')
        && matches!(bytes.get(end + 1), Some(b'\'' | b'"'));
    if unicode_escape { j } else { end }
}

fn operator_end(sql: &str, start: usize) -> usize {
    let bytes = sql.as_bytes();
    let mut i = start + 1;
    while i < bytes.len() && is_operator_char(bytes[i]) && !sql[i..].starts_with("--") && !sql[i..].starts_with("/*") {
        i += 1;
    }
    // A multi-character operator ends in + or - only when it also has one of ~ ! @ # % ^ & | ` ?
    if !bytes[start..i].iter().any(|b| b"~!@#%^&|`?".contains(b)) {
        while i - start > 1 && matches!(bytes[i - 1], b'+' | b'-') {
            i -= 1;
        }
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;
    use LexKind::*;

    fn kinds(sql: &str) -> Vec<(LexKind, &str)> {
        lex(sql).into_iter().map(|l| (l.kind, &sql[l.span])).collect()
    }

    const AWKWARD: &[&str] = &[
        "",
        " ",
        "select 1",
        "/* outer /* inner */ still outer */ select 1",
        "/* unterminated /* nested */",
        "$body$ begin; end $body$; $$x$$ $a$ $a $1 $ $é$",
        "$tag$ never closed",
        r"select E'\'', e'\\', E'unterminated \'",
        "select 'it''s', 'unterminated",
        "select \"q\"\"x\", \"unterminated",
        "U&'d\\0061t' UESCAPE '!', u&\"x\", B'01', X'1F', N'n', xe'x'",
        "select café, 漢字, '😀', \"é\" -- ünïcödé\n/* ✓ */",
        "a::int[], x #>> '{a,b}', y @> '[1]', z !~ 'q', a!=b, f(a => 1), x := 1, a - -1, a+-1, a*-1",
        "1 1.5 .5 1. 1e5 1e-5 1.5E+10 0x1F 1_000 1abc 1..2 $1abc 1e 1e- 1e--x",
        "a[1:2] :foo :'v' \\gset {x} ` ? \x0b",
        "select {{a}}, {{ bob's -- notes }}, {{}} {{x\n}} {{y}",
        "select 1;\n\nselect 2 -- trailing",
        "-",
        "/",
        "'",
        "\"",
        "U&",
        "e",
        "$",
        "1.",
    ];

    #[test]
    fn spans_tile_the_input() {
        for sql in AWKWARD {
            let lexemes = lex(sql);
            let mut at = 0;
            for l in &lexemes {
                assert_eq!(l.span.start, at, "{sql:?}: a gap or an overlap at {at}");
                assert!(l.span.end > l.span.start, "{sql:?}: an empty lexeme at {at}");
                assert!(sql.is_char_boundary(l.span.end), "{sql:?}: a span ends inside a character");
                at = l.span.end;
            }
            assert_eq!(at, sql.len(), "{sql:?}: the lexemes stop short of the end");
        }
    }

    #[test]
    fn prefixed_strings_and_quoted_names_are_one_lexeme() {
        assert_eq!(
            kinds(r"E'\'' U&'x' u&'y' U&&'z' x'1F' n'é' ex'1'"),
            vec![
                (String, r"E'\''"),
                (Space, " "),
                (String, "U&'x'"),
                (Space, " "),
                (String, "u&'y'"),
                (Space, " "),
                (Word, "U"),
                (Operator, "&&"),
                (String, "'z'"),
                (Space, " "),
                (String, "x'1F'"),
                (Space, " "),
                (String, "n'é'"),
                (Space, " "),
                (Word, "ex"),
                (String, "'1'"),
            ]
        );
        assert_eq!(kinds("U&\"a\"\"b\" \"c"), vec![(QuotedIdent, "U&\"a\"\"b\""), (Space, " "), (QuotedIdent, "\"c")]);
    }

    #[test]
    fn numbers_lex_as_postgres_16_does() {
        fn numbers(sql: &str) -> Vec<(LexKind, &str)> {
            kinds(sql).into_iter().filter(|&(k, _)| k != Space).collect()
        }
        assert_eq!(
            numbers("1_000.5 1_000.25e-3 0x1F 0b1010 0o17 1. .5 1.e5 1_000e1_0 0x_1F"),
            ["1_000.5", "1_000.25e-3", "0x1F", "0b1010", "0o17", "1.", ".5", "1.e5", "1_000e1_0", "0x_1F"]
                .map(|n| (Number, n))
        );
        assert_eq!(numbers("1..2"), vec![(Number, "1"), (Other, ".."), (Number, "2")]);
        assert_eq!(numbers("0x1F.5 1.5.3"), vec![(Number, "0x1F"), (Number, ".5"), (Number, "1.5"), (Number, ".3")]);
        assert_eq!(
            numbers("1_000_ 0b102 0x 1._5 1e+x"),
            vec![
                (Number, "1_000_"),
                (Number, "0b102"),
                (Number, "0x"),
                (Number, "1._5"),
                (Number, "1e"),
                (Operator, "+"),
                (Word, "x")
            ]
        );
    }

    #[test]
    fn a_string_continued_across_a_newline_is_one_lexeme() {
        assert_eq!(
            kinds("E'it\\'s'\n'\\'  two  spaces' x"),
            vec![(String, "E'it\\'s'\n'\\'  two  spaces'"), (Space, " "), (Word, "x")]
        );
        assert_eq!(kinds("'a' -- c\n  'b'\n-- d\n'c'"), vec![(String, "'a' -- c\n  'b'\n-- d\n'c'")]);
        assert_eq!(kinds("'a'\r'b'"), vec![(String, "'a'\r'b'")]);
        assert_eq!(
            kinds("B'01'\n'10' X'1F'\n'F' U&'a'\n'b' N'a'\n'b'"),
            vec![
                (String, "B'01'\n'10'"),
                (Space, " "),
                (String, "X'1F'\n'F'"),
                (Space, " "),
                (String, "U&'a'\n'b'"),
                (Space, " "),
                (String, "N'a'\n'b'")
            ]
        );
    }

    #[test]
    fn a_line_comment_ends_at_a_carriage_return() {
        assert_eq!(kinds("--c\rselect"), vec![(LineComment, "--c"), (Space, "\r"), (Word, "select")]);
    }

    #[test]
    fn strings_without_a_newline_between_them_stay_apart() {
        assert_eq!(kinds("'a' 'b'"), vec![(String, "'a'"), (Space, " "), (String, "'b'")]);
        assert_eq!(
            kinds("'a' /* c */\n'b'"),
            vec![(String, "'a'"), (Space, " "), (BlockComment, "/* c */"), (Space, "\n"), (String, "'b'")]
        );
        assert_eq!(
            kinds("'a'\n/* c */ 'b'"),
            vec![(String, "'a'"), (Space, "\n"), (BlockComment, "/* c */"), (Space, " "), (String, "'b'")]
        );
        assert_eq!(kinds("'a' -- c"), vec![(String, "'a'"), (Space, " "), (LineComment, "-- c")]);
        assert_eq!(kinds("\"a\"\n\"b\""), vec![(QuotedIdent, "\"a\""), (Space, "\n"), (QuotedIdent, "\"b\"")]);
    }

    #[test]
    fn a_dollar_quote_tag_may_hold_non_ascii_letters() {
        assert_eq!(
            kinds("$é$select x$é$ $ü1$a$ü1$ $1é"),
            vec![
                (DollarString, "$é$select x$é$"),
                (Space, " "),
                (DollarString, "$ü1$a$ü1$"),
                (Space, " "),
                (Param, "$1é")
            ]
        );
    }

    #[test]
    fn numbers_params_and_operators_lex_as_postgres_does() {
        assert_eq!(
            kinds("1.5e-3 .5 0x1F 1e5x $1abc $2"),
            vec![
                (Number, "1.5e-3"),
                (Space, " "),
                (Number, ".5"),
                (Space, " "),
                (Number, "0x1F"),
                (Space, " "),
                (Number, "1e5x"),
                (Space, " "),
                (Param, "$1abc"),
                (Space, " "),
                (Param, "$2"),
            ]
        );
        assert_eq!(
            kinds("a::int<>-1 x#>>'p' y--c"),
            vec![
                (Word, "a"),
                (Operator, "::"),
                (Word, "int"),
                (Operator, "<>"),
                (Operator, "-"),
                (Number, "1"),
                (Space, " "),
                (Word, "x"),
                (Operator, "#>>"),
                (String, "'p'"),
                (Space, " "),
                (Word, "y"),
                (LineComment, "--c"),
            ]
        );
        assert_eq!(
            kinds("a[1:2];"),
            vec![
                (Word, "a"),
                (OpenBracket, "["),
                (Number, "1"),
                (Other, ":"),
                (Number, "2"),
                (CloseBracket, "]"),
                (Semicolon, ";")
            ]
        );
    }
}
