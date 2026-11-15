//! A notebook file is SQL. Each cell starts at a `-- %%` comment line, the cell marker of Jupytext's percent format, so
//! the file still runs in psql or sqlcmd. An optional first line, `-- l1 notebook: …`, names the database and holds the
//! notebook's parameter values. A file with neither is one cell holding the whole file, and prints back byte for byte.
//! The text here is LF-only with no BOM; `files.rs` puts the file's own line breaks and BOM back.

use crate::model::{
    CellMeta, CellTable, DatabaseKind, MetaPair, NotebookCell, NotebookHeader, NotebookText, ParamValue,
};

const HEADER: &str = "-- l1 notebook:";
const MARKER: &str = "-- %%";
/// `param="<name>=<value>"` holds a value, and `null="<name>"` holds NULL. A name never holds `=`, so the value starts
/// after the first one. Older builds keep both as keys they do not read and write them back in order.
const PARAM: &str = "param";
const NULL: &str = "null";

pub(crate) fn parse(text: &str) -> NotebookText {
    let mut lines: Vec<&str> = text.split('\n').collect();
    let header = lines.first().and_then(|line| header_rest(line));
    if header.is_none() && !lines.iter().any(|line| marker_rest(line).is_some()) {
        return NotebookText {
            plain: true,
            header: NotebookHeader::default(),
            cells: vec![NotebookCell { text: text.to_owned(), meta: CellMeta::default() }],
        };
    }
    if text.ends_with('\n') {
        lines.pop();
    }
    let body = &lines[usize::from(header.is_some())..];
    let mut cells = Vec::new();
    let mut meta = None;
    let mut cell: Vec<String> = Vec::new();
    for line in body {
        let Some(rest) = marker_rest(line) else {
            cell.push(unescape(line));
            continue;
        };
        flush(&mut cells, meta.take(), &cell);
        cell.clear();
        meta = Some(cell_meta(rest));
    }
    flush(&mut cells, meta, &cell);
    if cells.is_empty() {
        cells.push(NotebookCell::default());
    }
    NotebookText { plain: false, header: header.map(notebook_header).unwrap_or_default(), cells }
}

/// The text, and whether it is plain: one cell's text as it is, with no header and no marker.
pub(crate) fn print(notebook: &NotebookText) -> (String, bool) {
    if let Some(text) = plain_text(notebook) {
        return (text.to_owned(), true);
    }
    let h = &notebook.header;
    let mut out = String::from(HEADER);
    let kind = h.kind.map(|k| match k {
        DatabaseKind::Postgres => "postgres",
        DatabaseKind::SqlServer => "sqlserver",
    });
    let params: Vec<(&str, String)> = h
        .params
        .iter()
        .map(|p| match &p.value {
            Some(value) => (PARAM, format!("{}={value}", p.name)),
            None => (NULL, p.name.clone()),
        })
        .collect();
    let mut known = vec![("connection", h.connection.as_deref()), ("kind", kind), ("database", h.database.as_deref())];
    known.extend(params.iter().map(|(key, value)| (*key, Some(value.as_str()))));
    write_pairs(&mut out, &known, &h.unknown);
    out.push('\n');
    for cell in &notebook.cells {
        let m = &cell.meta;
        let table = m.table.as_ref().map(|t| format!("{}.{}", quote_name(&t.schema), quote_name(&t.table)));
        out.push_str(MARKER);
        let known = [
            ("title", m.title.as_deref()),
            ("schema", m.schema.as_deref()),
            ("table", table.as_deref()),
            ("mode", m.manual.then_some("manual")),
        ];
        write_pairs(&mut out, &known, &m.unknown);
        out.push('\n');
        for line in cell.text.split('\n') {
            match escape_at(line) {
                Some(at) => {
                    out.push_str(&line[..at]);
                    out.push('\\');
                    out.push_str(&line[at..]);
                }
                None => out.push_str(line),
            }
            out.push('\n');
        }
    }
    (out, false)
}

/// Plain only while the text would read back as this same notebook. Parameter values live only in the header.
fn plain_text(notebook: &NotebookText) -> Option<&str> {
    let [cell] = notebook.cells.as_slice() else { return None };
    let text = cell.text.as_str();
    let structured = text.split('\n').next().is_some_and(|first| header_rest(first).is_some())
        || text.split('\n').any(|line| marker_rest(line).is_some());
    (notebook.plain && cell.meta == CellMeta::default() && notebook.header.params.is_empty() && !structured)
        .then_some(text)
}

/// Text before the first marker is a cell only when it holds something.
fn flush(cells: &mut Vec<NotebookCell>, meta: Option<CellMeta>, cell: &[String]) {
    match meta {
        Some(meta) => cells.push(NotebookCell { text: cell.join("\n"), meta }),
        None if cell.iter().any(|l| !l.trim().is_empty()) => {
            cells.push(NotebookCell { text: cell.join("\n"), meta: CellMeta::default() })
        }
        None => {}
    }
}

/// A person may write `--%%` or `--   %%`; this build writes `-- %%`.
fn marker_rest(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("--")?.trim_start_matches([' ', '\t']).strip_prefix("%%")?;
    (rest.is_empty() || rest.starts_with([' ', '\t'])).then_some(rest)
}

fn header_rest(line: &str) -> Option<&str> {
    line.strip_prefix("--")?.trim_start_matches([' ', '\t']).strip_prefix("l1 notebook:")
}

/// Where a cell line that reads as a marker, or as a marker escaped any number of times, takes one more backslash:
/// `-- %%` is written `-- \%%`, and `-- \%%` is written `-- \\%%`. Reading takes one off, so every line round-trips.
fn escape_at(line: &str) -> Option<usize> {
    let after_dashes = line.strip_prefix("--")?;
    let at = line.len() - after_dashes.trim_start_matches([' ', '\t']).len();
    let rest = line[at..].trim_start_matches('\\').strip_prefix("%%")?;
    (rest.is_empty() || rest.starts_with([' ', '\t'])).then_some(at)
}

fn unescape(line: &str) -> String {
    match escape_at(line) {
        Some(at) if line[at..].starts_with('\\') => [&line[..at], &line[at + 1..]].concat(),
        _ => line.to_owned(),
    }
}

fn notebook_header(rest: &str) -> NotebookHeader {
    let mut header = NotebookHeader::default();
    for pair in pairs(rest).0 {
        match pair.key.as_str() {
            "connection" if header.connection.is_none() => header.connection = Some(pair.value),
            "database" if header.database.is_none() => header.database = Some(pair.value),
            "kind" if header.kind.is_none() && kind_of(&pair.value).is_some() => header.kind = kind_of(&pair.value),
            _ => match param_of(&pair) {
                Some(p) if !header.params.iter().any(|q| q.name == p.name) => header.params.push(p),
                _ => header.unknown.push(pair),
            },
        }
    }
    header
}

fn param_of(pair: &MetaPair) -> Option<ParamValue> {
    let (name, value) = match pair.key.as_str() {
        PARAM => pair.value.split_once('=').map(|(name, value)| (name, Some(value)))?,
        NULL => (pair.value.as_str(), None),
        _ => return None,
    };
    let named = name.len() > 1 && name.starts_with([':', '$', '@']) && !name.contains(['=', '\n', '\r']);
    named.then(|| ParamValue { name: name.to_owned(), value: value.map(str::to_owned) })
}

fn kind_of(value: &str) -> Option<DatabaseKind> {
    match value {
        "postgres" => Some(DatabaseKind::Postgres),
        "sqlserver" => Some(DatabaseKind::SqlServer),
        _ => None,
    }
}

/// Text after the marker that is not a pair is the cell's title, as Jupytext writes `# %% Title`.
fn cell_meta(rest: &str) -> CellMeta {
    let (pairs, free) = pairs(rest);
    let mut meta = CellMeta::default();
    for pair in pairs {
        match pair.key.as_str() {
            "title" if meta.title.is_none() => meta.title = Some(pair.value),
            "schema" if meta.schema.is_none() => meta.schema = Some(pair.value),
            "table" if meta.table.is_none() && table_of(&pair.value).is_some() => meta.table = table_of(&pair.value),
            "mode" if !meta.manual && pair.value == "manual" => meta.manual = true,
            _ => meta.unknown.push(pair),
        }
    }
    if meta.title.is_none() && !free.is_empty() {
        meta.title = Some(free.to_owned());
    }
    meta
}

/// `key="value"` pairs, in order, up to the first text that is not one, which comes back trimmed.
fn pairs(mut rest: &str) -> (Vec<MetaPair>, &str) {
    let mut found = Vec::new();
    loop {
        rest = rest.trim_start();
        match pair(rest) {
            Some((p, after)) => {
                found.push(p);
                rest = after;
            }
            None => return (found, rest.trim_end()),
        }
    }
}

fn pair(s: &str) -> Option<(MetaPair, &str)> {
    let key_len = s.find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))).unwrap_or(s.len());
    let (key, rest) = s.split_at(key_len);
    let quoted = rest.trim_start().strip_prefix('=')?.trim_start().strip_prefix('"')?;
    if key.is_empty() {
        return None;
    }
    let mut value = String::new();
    let mut chars = quoted.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => return Some((MetaPair { key: key.to_owned(), value }, &quoted[i + 1..])),
            '\\' => match chars.next()?.1 {
                'n' => value.push('\n'),
                'r' => value.push('\r'),
                c @ ('"' | '\\') => value.push(c),
                c => value.extend(['\\', c]),
            },
            c => value.push(c),
        }
    }
    None
}

fn write_pairs(out: &mut String, known: &[(&str, Option<&str>)], unknown: &[MetaPair]) {
    let written: Vec<&str> = known.iter().filter_map(|&(key, value)| value.map(|_| key)).collect();
    let known = known.iter().filter_map(|&(key, value)| Some((key, value?)));
    let unknown =
        unknown.iter().filter(|p| !written.contains(&p.key.as_str())).map(|p| (p.key.as_str(), p.value.as_str()));
    for (key, value) in known.chain(unknown) {
        out.push(' ');
        out.push_str(key);
        out.push_str("=\"");
        for c in value.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                c => out.push(c),
            }
        }
        out.push('"');
    }
}

/// Double-quoted as SQL quotes an identifier unless it is a plain word, which keeps its case.
fn quote_name(name: &str) -> String {
    let plain = name.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '$'));
    if plain { name.to_owned() } else { format!("\"{}\"", name.replace('"', "\"\"")) }
}

fn table_of(value: &str) -> Option<CellTable> {
    let (schema, rest) = name_of(value)?;
    let (table, rest) = name_of(rest.strip_prefix('.')?)?;
    rest.is_empty().then_some(CellTable { schema, table })
}

fn name_of(s: &str) -> Option<(String, &str)> {
    let Some(quoted) = s.strip_prefix('"') else {
        let end = s.find('.').unwrap_or(s.len());
        return (end > 0 && !s[..end].contains('"')).then(|| (s[..end].to_owned(), &s[end..]));
    };
    let mut name = String::new();
    let mut chars = quoted.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if c != '"' {
            name.push(c);
        } else if chars.next_if(|&(_, c)| c == '"').is_some() {
            name.push('"');
        } else {
            return Some((name, &quoted[i + 1..]));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::files::{decode, encode};

    fn cell(text: &str, meta: CellMeta) -> NotebookCell {
        NotebookCell { text: text.to_owned(), meta }
    }

    fn titled(title: &str) -> CellMeta {
        CellMeta { title: Some(title.to_owned()), ..CellMeta::default() }
    }

    fn pair(key: &str, value: &str) -> MetaPair {
        MetaPair { key: key.to_owned(), value: value.to_owned() }
    }

    fn shop() -> NotebookHeader {
        NotebookHeader {
            connection: Some("localhost / shop".into()),
            kind: Some(DatabaseKind::Postgres),
            database: Some("shop".into()),
            params: vec![],
            unknown: vec![],
        }
    }

    fn value(name: &str, value: Option<&str>) -> ParamValue {
        ParamValue { name: name.to_owned(), value: value.map(str::to_owned) }
    }

    /// The bytes a plain file holds after it is opened and saved unchanged.
    fn resaved(bytes: &[u8]) -> Vec<u8> {
        let (text, format) = decode(bytes).unwrap();
        let notebook = parse(&text);
        assert!(notebook.plain, "{bytes:?} reads as plain");
        let (printed, plain) = print(&notebook);
        assert!(plain, "{bytes:?} prints plain");
        encode(&printed, format)
    }

    #[test]
    fn a_plain_file_saves_back_byte_for_byte() {
        let files: [&[u8]; 8] = [
            b"",
            b"select 1;",
            b"select 1;\n",
            b"select 1;\nselect 2;\n\n",
            b"select 1;\r\nselect 2;\r\n",
            b"select 1;\r\nselect 2;",
            b"\xEF\xBB\xBFselect 1;\r\n",
            b"\xEF\xBB\xBF",
        ];
        for bytes in files {
            assert_eq!(resaved(bytes), bytes, "{:?}", String::from_utf8_lossy(bytes));
        }
    }

    #[test]
    fn a_plain_file_is_one_cell_with_its_whole_text() {
        let text = "-- monthly revenue\nselect 1;\n-- \\%% not a marker\n";
        assert_eq!(
            parse(text),
            NotebookText {
                plain: true,
                header: NotebookHeader::default(),
                cells: vec![cell(text, CellMeta::default())]
            }
        );
        assert_eq!(
            parse(""),
            NotebookText { plain: true, header: NotebookHeader::default(), cells: vec![cell("", CellMeta::default())] }
        );
    }

    const REVENUE: &str = "\
-- l1 notebook: connection=\"localhost / shop\" kind=\"postgres\" database=\"shop\"
-- %% title=\"Revenue by region\" schema=\"public\"
SELECT region, sum(total)
FROM orders
GROUP BY 1;
-- %% table=\"public.orders\"
SELECT * FROM \"public\".\"orders\"
";

    fn revenue() -> NotebookText {
        let first = CellMeta { schema: Some("public".into()), ..titled("Revenue by region") };
        let second = CellMeta {
            table: Some(CellTable { schema: "public".into(), table: "orders".into() }),
            ..CellMeta::default()
        };
        NotebookText {
            plain: false,
            header: shop(),
            cells: vec![
                cell("SELECT region, sum(total)\nFROM orders\nGROUP BY 1;", first),
                cell("SELECT * FROM \"public\".\"orders\"", second),
            ],
        }
    }

    #[test]
    fn a_notebook_file_reads_and_prints_back_the_same() {
        assert_eq!(parse(REVENUE), revenue());
        assert_eq!(print(&revenue()), (REVENUE.to_owned(), false));
        let (crlf, format) = decode(&encode(REVENUE, crate::model::FileFormat { bom: true, crlf: true })).unwrap();
        assert_eq!(parse(&crlf), revenue(), "{format:?}");
    }

    #[test]
    fn every_notebook_prints_to_a_file_that_reads_back_as_it() {
        let manual = CellMeta { manual: true, unknown: vec![pair("chart", "bar")], ..titled("Totals") };
        let notebooks = [
            revenue(),
            NotebookText {
                plain: false,
                header: NotebookHeader::default(),
                cells: vec![cell("", CellMeta::default())],
            },
            NotebookText {
                plain: false,
                header: shop(),
                cells: vec![cell("", CellMeta::default()), cell("\n", CellMeta::default()), cell("a\n\nb\n", manual)],
            },
            NotebookText { plain: true, header: shop(), cells: vec![cell("select 1;", titled("named"))] },
            NotebookText {
                plain: true,
                header: NotebookHeader::default(),
                cells: vec![cell("a", CellMeta::default()), cell("b", CellMeta::default())],
            },
        ];
        for notebook in notebooks {
            let (text, plain) = print(&notebook);
            assert!(!plain, "{notebook:?}");
            assert_eq!(parse(&text), NotebookText { plain: false, ..notebook }, "{text}");
        }
    }

    #[test]
    fn a_plain_notebook_edited_past_one_bare_cell_prints_the_header_and_markers() {
        let two = NotebookText {
            plain: true,
            header: shop(),
            cells: vec![cell("select 1;", CellMeta::default()), cell("select 2;", CellMeta::default())],
        };
        assert_eq!(
            print(&two).0,
            "-- l1 notebook: connection=\"localhost / shop\" kind=\"postgres\" database=\"shop\"\n-- %%\nselect 1;\n-- %%\nselect 2;\n"
        );
        let manual = NotebookText {
            plain: true,
            header: shop(),
            cells: vec![cell("select 1;", CellMeta { manual: true, ..CellMeta::default() })],
        };
        assert!(print(&manual).0.contains("\n-- %% mode=\"manual\"\nselect 1;\n"));
    }

    #[test]
    fn parameters_round_trip_in_order_with_nulls_and_escapes() {
        let params = vec![
            value(":region", Some("EU")),
            value("$1", None),
            value("@Since", Some("say \"a=b\" \\ to\nC:\\temp")),
            value(":empty", Some("")),
        ];
        let notebook = NotebookText {
            plain: false,
            header: NotebookHeader { params: params.clone(), unknown: vec![pair("theme", "dark")], ..shop() },
            cells: vec![cell("select :region, $1, @Since, :empty;", CellMeta::default())],
        };
        let (text, _) = print(&notebook);
        assert_eq!(
            text.lines().next().unwrap(),
            "-- l1 notebook: connection=\"localhost / shop\" kind=\"postgres\" database=\"shop\" param=\":region=EU\" null=\"$1\" param=\"@Since=say \\\"a=b\\\" \\\\ to\\nC:\\\\temp\" param=\":empty=\" theme=\"dark\""
        );
        assert_eq!(parse(&text), notebook);
    }

    #[test]
    fn a_malformed_or_repeated_parameter_stays_an_unknown_key() {
        let text = "-- l1 notebook: param=\":x=1\" param=\"x=2\" param=\":y\" null=\":x\" null=\"@\" param=\"=3\" null=\":a=b\" param=\":z=a=b\"\n-- %%\nselect 1;\n";
        let header = parse(text).header;
        assert_eq!(header.params, [value(":x", Some("1")), value(":z", Some("a=b"))]);
        assert_eq!(
            header.unknown,
            [
                pair("param", "x=2"),
                pair("param", ":y"),
                pair("null", ":x"),
                pair("null", "@"),
                pair("param", "=3"),
                pair("null", ":a=b")
            ]
        );
        let none = NotebookText { header: NotebookHeader { params: vec![], ..header.clone() }, ..parse(text) };
        assert!(
            print(&none).0.starts_with("-- l1 notebook: param=\"x=2\" param=\":y\" null=\":x\""),
            "a key this build cannot read is written back while no value takes its key"
        );
    }

    #[test]
    fn a_build_that_reads_params_as_unknown_keys_writes_them_back() {
        let older = NotebookText {
            plain: false,
            header: NotebookHeader { unknown: vec![pair("param", ":x=41"), pair("null", "@y")], ..shop() },
            cells: vec![cell("select :x::int, @y;", CellMeta::default())],
        };
        let (text, _) = print(&older);
        assert_eq!(parse(&text).header.params, [value(":x", Some("41")), value("@y", None)]);
    }

    #[test]
    fn a_plain_notebook_with_parameters_prints_the_header() {
        let notebook = NotebookText {
            plain: true,
            header: NotebookHeader { params: vec![value(":x", Some("41"))], ..NotebookHeader::default() },
            cells: vec![cell("select :x::int + 1;", CellMeta::default())],
        };
        let (text, plain) = print(&notebook);
        assert_eq!((text.as_str(), plain), ("-- l1 notebook: param=\":x=41\"\n-- %%\nselect :x::int + 1;\n", false));
        assert_eq!(parse(&text), NotebookText { plain: false, ..notebook });
    }

    #[test]
    fn the_header_is_optional_and_holds_names_and_parameter_values() {
        let headless = parse("-- %%\nselect 1;\n");
        assert_eq!(
            (headless.plain, headless.header, headless.cells),
            (false, NotebookHeader::default(), vec![cell("select 1;", CellMeta::default())])
        );
        let header_only = parse("-- l1 notebook: kind=\"sqlserver\" database=\"reporting\"\n");
        assert_eq!(
            header_only.header,
            NotebookHeader {
                kind: Some(DatabaseKind::SqlServer),
                database: Some("reporting".into()),
                ..NotebookHeader::default()
            }
        );
        assert_eq!(header_only.cells, vec![cell("", CellMeta::default())], "a notebook always has a cell");
        let bare = parse("-- l1 notebook:\nselect 1;\n");
        assert_eq!(
            (bare.header, bare.cells),
            (NotebookHeader::default(), vec![cell("select 1;", CellMeta::default())])
        );
        let no_connection = NotebookText {
            plain: false,
            header: NotebookHeader::default(),
            cells: vec![cell("select 1;", CellMeta::default())],
        };
        assert_eq!(print(&no_connection).0, "-- l1 notebook:\n-- %%\nselect 1;\n");
        let valued = parse("-- l1 notebook: database=\"shop\" null=\":id\"\nselect :id;\n");
        assert_eq!(
            valued.header,
            NotebookHeader {
                database: Some("shop".into()),
                params: vec![value(":id", None)],
                ..NotebookHeader::default()
            }
        );
    }

    #[test]
    fn text_before_the_first_marker_is_a_cell_unless_it_is_blank() {
        let leading = parse("select 1;\n\n-- %% title=\"two\"\nselect 2;");
        assert_eq!(leading.cells, vec![cell("select 1;\n", CellMeta::default()), cell("select 2;", titled("two"))]);
        let blank = parse("-- l1 notebook: database=\"shop\"\n\n  \n-- %%\nselect 1;\n");
        assert_eq!(blank.cells, vec![cell("select 1;", CellMeta::default())]);
    }

    #[test]
    fn empty_cells_keep_their_place() {
        let notebook = parse("-- %%\n-- %% title=\"b\"\n\n-- %%\n");
        assert_eq!(
            notebook.cells,
            vec![cell("", CellMeta::default()), cell("", titled("b")), cell("", CellMeta::default())]
        );
    }

    #[test]
    fn values_escape_quotes_backslashes_and_line_breaks() {
        let title = "say \"hi\" \\ to C:\\temp\nand\rback";
        let table = CellTable { schema: "my \"odd\" schema".into(), table: "a.b".into() };
        let notebook = NotebookText {
            plain: false,
            header: NotebookHeader { connection: Some("prod \"eu\"".into()), ..NotebookHeader::default() },
            cells: vec![cell("select 1;", CellMeta { table: Some(table), ..titled(title) })],
        };
        let (text, _) = print(&notebook);
        assert_eq!(
            text,
            "-- l1 notebook: connection=\"prod \\\"eu\\\"\"\n-- %% title=\"say \\\"hi\\\" \\\\ to C:\\\\temp\\nand\\rback\" table=\"\\\"my \\\"\\\"odd\\\"\\\" schema\\\".\\\"a.b\\\"\"\nselect 1;\n"
        );
        assert_eq!(parse(&text), notebook);
    }

    #[test]
    fn table_names_are_quoted_only_when_they_need_it() {
        let cases = [
            ("public", "orders", "public.orders"),
            ("Sales", "Order$2", "Sales.Order$2"),
            ("public", "2024", "public.\"2024\""),
            ("dbo", "order lines", "dbo.\"order lines\""),
        ];
        for (schema, table, printed) in cases {
            assert_eq!(format!("{}.{}", quote_name(schema), quote_name(table)), printed);
            assert_eq!(table_of(printed), Some(CellTable { schema: schema.into(), table: table.into() }), "{printed}");
        }
        for unreadable in ["orders", "a.b.c", "\"a.b", ".orders", "a.", "a\"b.c"] {
            assert_eq!(table_of(unreadable), None, "{unreadable}");
        }
    }

    #[test]
    fn keys_this_build_does_not_read_are_written_back() {
        let text = "\
-- l1 notebook: connection=\"c\" theme=\"dark\" kind=\"mysql\"
-- %% chart=\"bar\" title=\"t\" mode=\"later\" table=\"orders\"
select 1;
";
        let notebook = parse(text);
        assert_eq!(
            notebook.header,
            NotebookHeader {
                connection: Some("c".into()),
                unknown: vec![pair("theme", "dark"), pair("kind", "mysql")],
                ..NotebookHeader::default()
            }
        );
        assert_eq!(
            notebook.cells[0].meta,
            CellMeta {
                unknown: vec![pair("chart", "bar"), pair("mode", "later"), pair("table", "orders")],
                ..titled("t")
            }
        );
        assert_eq!(
            print(&notebook).0,
            "-- l1 notebook: connection=\"c\" theme=\"dark\" kind=\"mysql\"\n-- %% title=\"t\" chart=\"bar\" mode=\"later\" table=\"orders\"\nselect 1;\n"
        );
        let mut known = notebook.clone();
        known.header.kind = Some(DatabaseKind::Postgres);
        assert!(
            print(&known).0.starts_with("-- l1 notebook: connection=\"c\" kind=\"postgres\" theme=\"dark\"\n"),
            "a known value replaces the one it could not read"
        );
    }

    #[test]
    fn a_cell_line_that_looks_like_a_marker_is_escaped_and_reads_back() {
        let tricky =
            "select 1;\n-- %% not a cell\n--%%\n-- \\%% already escaped\n  -- %% indented\n-- %%%\n-- l1 notebook: x";
        let notebook = NotebookText {
            plain: false,
            header: NotebookHeader::default(),
            cells: vec![cell(tricky, CellMeta::default()), cell("b", CellMeta::default())],
        };
        let (text, _) = print(&notebook);
        assert!(
            text.contains("\n-- \\%% not a cell\n--\\%%\n-- \\\\%% already escaped\n  -- %% indented\n-- %%%\n"),
            "{text}"
        );
        assert_eq!(parse(&text), notebook);
        let plain = NotebookText {
            plain: true,
            header: NotebookHeader::default(),
            cells: vec![cell(tricky, CellMeta::default())],
        };
        let (text, as_plain) = print(&plain);
        assert!(!as_plain, "plain text with a marker line would read back as two cells");
        assert_eq!(parse(&text).cells, plain.cells);
        let header_first = NotebookText {
            plain: true,
            header: NotebookHeader::default(),
            cells: vec![cell("-- l1 notebook: kind=\"postgres\"\nselect 1;", CellMeta::default())],
        };
        let (text, as_plain) = print(&header_first);
        assert!(!as_plain, "a first line that reads as a header is not plain");
        assert_eq!(parse(&text).cells, header_first.cells);
    }

    #[test]
    fn a_file_a_person_wrote_with_loose_spacing_reads() {
        let text = "--   l1 notebook:   kind = \"postgres\"   database=\"shop\"  \n--%%   title = \"first\"   schema=\"s\"  \nselect 1;\n--  %%\tSecond cell\nselect 2;\n-- %% Third  title=\"x\"\nselect 3;\n";
        let notebook = parse(text);
        assert_eq!(
            notebook.header,
            NotebookHeader {
                kind: Some(DatabaseKind::Postgres),
                database: Some("shop".into()),
                ..NotebookHeader::default()
            }
        );
        assert_eq!(
            notebook.cells,
            vec![
                cell("select 1;", CellMeta { schema: Some("s".into()), ..titled("first") }),
                cell("select 2;", titled("Second cell")),
                cell("select 3;", titled("Third  title=\"x\"")),
            ]
        );
    }
}
