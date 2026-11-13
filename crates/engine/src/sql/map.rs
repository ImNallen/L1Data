use super::scope::{Class, Lexeme, closed, map_lexemes};
use crate::model::*;
use crate::text::OffsetMap;

struct Token<'a> {
    class: Class,
    text: &'a str,
    start: usize,
    end: usize,
}

impl Token<'_> {
    fn word(&self, word: &str) -> bool {
        self.class == Class::Word && self.text.eq_ignore_ascii_case(word)
    }
    fn name(&self, kind: DatabaseKind) -> Option<String> {
        match self.class {
            Class::Word if !self.text.starts_with(['#', '@']) => {
                Some(if kind == DatabaseKind::Postgres { self.text.to_ascii_lowercase() } else { self.text.to_owned() })
            }
            Class::Name if self.text.starts_with('"') && self.text.ends_with('"') && self.text.len() >= 2 => {
                Some(self.text[1..self.text.len() - 1].replace("\"\"", "\""))
            }
            Class::Name if self.text.starts_with('[') && self.text.ends_with(']') && self.text.len() >= 2 => {
                Some(self.text[1..self.text.len() - 1].replace("]]", "]"))
            }
            _ => None,
        }
    }
}

fn boundary(t: &Token<'_>) -> bool {
    [
        "join",
        "left",
        "right",
        "inner",
        "outer",
        "full",
        "cross",
        "where",
        "group",
        "order",
        "having",
        "limit",
        "offset",
        "fetch",
        "union",
        "except",
        "intersect",
        "for",
        "window",
        "returning",
        "on",
        "using",
    ]
    .iter()
    .any(|w| t.word(w))
        || t.class == Class::Semi
}

fn range(map: &OffsetMap<'_>, start: usize, end: usize) -> DocRange {
    DocRange { from: map.to_utf16(start), to: map.to_utf16(end) }
}

fn resolve(parts: &[String], catalog: &Catalog, ctes: &[String]) -> MapResolution {
    let unresolved = |reason: &str| MapResolution::Unresolved { reason: reason.into() };
    if parts.len() == 1 && ctes.contains(&parts[0]) {
        return unresolved("CTE source");
    }
    if parts.is_empty() || parts.len() > 2 || parts.iter().any(|p| p.starts_with(['#', '@'])) {
        return unresolved("Unsupported or cross-database source");
    }
    let candidates: Vec<_> = catalog
        .schemas
        .iter()
        .filter(|s| parts.len() == 1 || s.name == parts[0])
        .flat_map(|s| &s.relations)
        .filter(|r| Some(&r.name) == parts.last())
        .collect();
    match candidates.as_slice() {
        [r] if parts.len() == 2 => MapResolution::CatalogMatch { oid: r.oid },
        [r] => MapResolution::Tentative { oid: r.oid },
        [] => unresolved("No exact catalog match"),
        _ => unresolved("Ambiguous catalog name"),
    }
}

pub(crate) fn analyze(text: &str, cursor: DocOffset, catalog: &Catalog) -> MapQuery {
    let kind = catalog.database_kind;
    let map = OffsetMap::new(text);
    let mut result = MapQuery {
        db: catalog.db.clone(),
        catalog_version: catalog.version,
        statement: None,
        occurrences: vec![],
        joins: vec![],
        incomplete: false,
    };
    let at = map.to_byte(cursor);
    let selected = match kind {
        DatabaseKind::Postgres => super::pick(&super::split_statements(text), text, super::ByteTarget::Cursor(at))
            .into_iter()
            .next()
            .map(|s| s.start..s.end),
        DatabaseKind::SqlServer => {
            let mut span = super::tsql::statement_around(text, at);
            if text[span.clone()].trim().is_empty() && at > 0 && text.as_bytes()[at - 1] == b';' {
                span = super::tsql::statement_around(text, at - 1);
            }
            (!text[span.clone()].trim().is_empty()).then_some(span)
        }
    };
    let Some(selected) = selected else {
        return result;
    };
    result.statement = Some(range(&map, selected.start, selected.end));
    let sql = &text[selected.clone()];
    let lexemes = map_lexemes(sql, kind);
    let unclosed = lexemes.iter().any(|l| {
        matches!(l.class, Class::Name | Class::Str | Class::BlockComment) && !closed(kind, &sql[l.span.clone()])
    });
    let tokens: Vec<_> = lexemes
        .into_iter()
        .filter(|l| !matches!(l.class, Class::LineComment | Class::BlockComment))
        .map(|Lexeme { class, span }| Token {
            class,
            text: &sql[span.clone()],
            start: selected.start + span.start,
            end: selected.start + span.end,
        })
        .collect();
    let mut ctes = vec![];
    let mut depth = 0usize;
    let mut cte_name = false;
    let mut cte_body = false;
    let mut scopes = vec![];
    let mut i = 0;
    let mut joins_sound = !unclosed;
    result.incomplete = unclosed;
    let mut from_list = false;
    while i < tokens.len() {
        let t = &tokens[i];
        if depth == 0 && t.word("with") {
            cte_name = true;
            result.incomplete = true;
            joins_sound = false;
            i += 1;
            continue;
        }
        if cte_name && !t.word("recursive") {
            if let Some(name) = t.name(kind) {
                ctes.push(name);
            }
            cte_name = false;
            cte_body = true;
            result.incomplete = true;
            joins_sound = false;
        }
        if t.class == Class::Open {
            depth += 1;
        }
        if t.class == Class::Close {
            depth = depth.saturating_sub(1);
        }
        if depth > 0 {
            if t.word("select") {
                result.incomplete = true;
                joins_sound = false;
            }
            i += 1;
            continue;
        }
        if cte_body && t.class == Class::Comma {
            cte_name = true;
            i += 1;
            continue;
        }
        if cte_body && t.word("select") {
            cte_body = false;
        }
        let source = t.word("from") || t.word("join") || (from_list && t.class == Class::Comma);
        if source {
            if t.class == Class::Comma {
                result.incomplete = true;
                joins_sound = false;
            }
            from_list = true;
            let joined = t.word("join");
            i += 1;
            let start = i;
            let mut parts = vec![];
            let mut complete_name = true;
            if tokens.get(i).is_some_and(|t| t.word("only") || t.word("lateral")) {
                result.incomplete = true;
                joins_sound = false;
                i += 1;
            }
            if let Some(name) = tokens.get(i).and_then(|t| t.name(kind)) {
                parts.push(name);
                i += 1;
                while tokens.get(i).is_some_and(|t| t.class == Class::Dot) {
                    i += 1;
                    if let Some(name) = tokens.get(i).and_then(|t| t.name(kind)) {
                        parts.push(name);
                        i += 1;
                    } else {
                        complete_name = false;
                        break;
                    }
                }
            }
            if parts.is_empty() {
                result.incomplete = true;
                joins_sound = false;
                continue;
            }
            let end = i;
            let mut resolution = resolve(&parts, catalog, &ctes);
            if !complete_name || tokens.get(i).is_some_and(|t| t.class == Class::Open || t.class == Class::Dot) {
                resolution = MapResolution::Unresolved { reason: "Function or unsupported source".into() };
                result.incomplete = true;
                joins_sound = false;
            }
            let explicit_alias = tokens.get(i).is_some_and(|t| t.word("as"));
            if explicit_alias {
                i += 1;
            }
            let alias = tokens
                .get(i)
                .filter(|t| !boundary(t) && t.class != Class::Comma && t.class != Class::Open)
                .and_then(|t| t.name(kind));
            if explicit_alias && alias.is_none() {
                result.incomplete = true;
                joins_sound = false;
            }
            if alias.is_some() {
                i += 1;
            }
            if tokens.get(i).is_some_and(|t| !boundary(t) && t.class != Class::Comma) {
                result.incomplete = true;
                joins_sound = false;
            }
            if !matches!(resolution, MapResolution::CatalogMatch { .. }) {
                result.incomplete = true;
            }
            let id = result.occurrences.len() as u32;
            result.occurrences.push(MapOccurrence {
                id,
                name: text[tokens[start].start..tokens[end - 1].end].into(),
                alias,
                range: range(&map, tokens[start].start, tokens[end - 1].end),
                resolution,
            });
            if joined && tokens.get(i).is_some_and(|t| t.word("on")) {
                let on = i + 1;
                i = on;
                while i < tokens.len() && !boundary(&tokens[i]) {
                    i += 1;
                }
                scopes.push((on, i, id));
            } else if joined {
                result.incomplete = true;
                joins_sound = false;
            }
            continue;
        }
        if t.word("where") || t.word("on") || t.word("group") || t.word("order") {
            from_list = false;
        }
        if t.word("select") && i > 0 && !ctes.is_empty() {
            joins_sound = false;
        }
        if ["union", "except", "intersect", "apply", "pivot", "unpivot", "using", "natural"].iter().any(|w| t.word(w)) {
            result.incomplete = true;
            joins_sound = false;
        }
        i += 1;
    }
    if depth != 0 {
        result.incomplete = true;
        joins_sound = false;
    }
    if tokens.iter().filter(|t| t.word("select")).count() != 1 || tokens.iter().any(|t| t.word("or") || t.text == "::")
    {
        result.incomplete = true;
        joins_sound = false;
    }
    let bindings: Vec<_> = result
        .occurrences
        .iter()
        .filter_map(|o| {
            o.alias.clone().or_else(|| match o.resolution {
                MapResolution::CatalogMatch { oid } => {
                    catalog.schemas.iter().flat_map(|s| &s.relations).find(|r| r.oid == oid).map(|r| r.name.clone())
                }
                _ => None,
            })
        })
        .collect();
    if bindings.iter().enumerate().any(|(i, b)| bindings[..i].contains(b)) {
        result.incomplete = true;
        joins_sound = false;
    }
    if joins_sound {
        for (start, end, joined) in scopes {
            match predicates(&tokens[start..end], joined, &result.occurrences, catalog, &map) {
                Some(edges) => result.joins.extend(edges),
                None => result.incomplete = true,
            }
        }
    }
    result
}

fn predicates(
    tokens: &[Token<'_>],
    joined: u32,
    occurrences: &[MapOccurrence],
    catalog: &Catalog,
    map: &OffsetMap<'_>,
) -> Option<Vec<MapQueryJoin>> {
    if tokens.is_empty() {
        return None;
    }
    let mut edges = vec![];
    for chunk in tokens.split(|t| t.word("and")) {
        let [a, dot_a, ac, eq, b, dot_b, bc] = chunk else {
            return None;
        };
        if dot_a.class != Class::Dot || dot_b.class != Class::Dot || eq.text != "=" {
            return None;
        }
        let bind = |alias: &Token<'_>, column: &Token<'_>| -> Option<(u32, String)> {
            let alias = alias.name(catalog.database_kind)?;
            let column = column.name(catalog.database_kind)?;
            let mut matches = occurrences.iter().filter(|o| {
                o.id <= joined
                    && o.alias.as_ref().map_or_else(
                        || match o.resolution {
                            MapResolution::CatalogMatch { oid } => catalog
                                .schemas
                                .iter()
                                .flat_map(|s| &s.relations)
                                .any(|r| r.oid == oid && r.name == alias),
                            _ => false,
                        },
                        |a| *a == alias,
                    )
            });
            let occurrence = matches.next()?;
            if matches.next().is_some() {
                return None;
            }
            let MapResolution::CatalogMatch { oid } = occurrence.resolution else {
                return None;
            };
            let relation = catalog.schemas.iter().flat_map(|s| &s.relations).find(|r| r.oid == oid)?;
            relation.columns.iter().any(|c| c.name == column).then_some((occurrence.id, column))
        };
        let (source, source_column) = bind(a, ac)?;
        let (target, target_column) = bind(b, bc)?;
        if source == target || (source != joined && target != joined) {
            return None;
        }
        edges.push(MapQueryJoin {
            source,
            target,
            columns: MapColumnPair { source: source_column, target: target_column },
            range: range(map, a.start, bc.end),
        });
    }
    Some(edges)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog(kind: DatabaseKind) -> Catalog {
        let relation = |oid, name: &str| Relation {
            oid,
            name: name.into(),
            qualified_name: format!("public.{name}"),
            quoted: name.into(),
            kind: RelKind::Table,
            row_estimate: None,
            columns: ["id", "parent_id", "tenant", "odd\"column"]
                .into_iter()
                .enumerate()
                .map(|(i, name)| ColumnInfo {
                    name: name.into(),
                    quoted: name.into(),
                    attnum: i as i16 + 1,
                    writable: true,
                    type_display: "int".into(),
                    category: TypeCategory::Integer,
                    not_null: false,
                    pk: false,
                    fk: false,
                })
                .collect(),
        };
        Catalog {
            db: DatabaseRef { connection_id: ConnectionId(uuid::Uuid::nil()), database: "shop".into() },
            database_kind: kind,
            version: 7,
            default_schema: Some("public".into()),
            extensions: vec![],
            schemas: vec![SchemaInfo {
                name: "public".into(),
                quoted: "public".into(),
                system: false,
                table_count: 3,
                relations: vec![relation(1, "orders"), relation(2, "people"), relation(3, "Odd\"Table")],
                functions: vec![],
                sequences: vec![],
                types: vec![],
                synonyms: vec![],
            }],
        }
    }

    fn analyze_at_end(sql: &str, kind: DatabaseKind) -> MapQuery {
        analyze(sql, DocOffset(sql.encode_utf16().count() as u32), &catalog(kind))
    }
    fn slice(sql: &str, range: DocRange) -> &str {
        let map = OffsetMap::new(sql);
        &sql[map.to_byte(range.from)..map.to_byte(range.to)]
    }

    #[test]
    fn preserves_self_join_occurrences_and_ordered_direct_predicates() {
        for kind in [DatabaseKind::Postgres, DatabaseKind::SqlServer] {
            let sql = "SELECT a.id FROM public.orders AS a LEFT JOIN public.orders b ON a.parent_id = b.id AND a.tenant=b.tenant WHERE a.id>1";
            let result = analyze_at_end(sql, kind);
            assert!(!result.incomplete);
            assert_eq!(result.occurrences.len(), 2);
            assert_eq!(result.occurrences[0].alias.as_deref(), Some("a"));
            assert_ne!(result.occurrences[0].id, result.occurrences[1].id);
            assert_eq!(result.joins.len(), 2);
            assert_eq!((result.joins[0].source, result.joins[0].target), (0, 1));
            assert_eq!(slice(sql, result.joins[0].range), "a.parent_id = b.id");
            assert_eq!(slice(sql, result.occurrences[1].range), "public.orders");
        }
    }

    #[test]
    fn quoted_names_comments_and_strings_keep_exact_utf16_ranges() {
        let sql = "-- 😀漢 FROM fake\nSELECT 'JOIN nope ON x=y', a.id FROM \"public\".\"Odd\"\"Table\" a /* JOIN fake */ JOIN public.people p ON a.\"odd\"\"column\"=p.id";
        let result = analyze_at_end(sql, DatabaseKind::Postgres);
        assert_eq!(result.occurrences.len(), 2);
        assert!(matches!(result.occurrences[0].resolution, MapResolution::CatalogMatch { oid: 3 }));
        assert_eq!(slice(sql, result.occurrences[0].range), "\"public\".\"Odd\"\"Table\"");
        assert_eq!(slice(sql, result.joins[0].range), "a.\"odd\"\"column\"=p.id");
        assert_eq!(
            result.occurrences[0].range.from.0,
            sql[..sql.find("\"public\"").unwrap()].encode_utf16().count() as u32
        );
        let result = analyze_at_end(
            "SELECT * FROM [public].[orders] a JOIN [public].[people] b ON a.[id]=b.[id]",
            DatabaseKind::SqlServer,
        );
        assert_eq!(result.joins.len(), 1);
        assert!(!result.incomplete);
    }

    #[test]
    fn bare_names_are_tentative_and_sql_server_case_is_conservative() {
        for kind in [DatabaseKind::Postgres, DatabaseKind::SqlServer] {
            let result = analyze_at_end("SELECT * FROM orders a JOIN people b ON a.id=b.id", kind);
            assert!(result.incomplete && result.joins.is_empty());
            assert!(matches!(result.occurrences[0].resolution, MapResolution::Tentative { oid: 1 }));
        }
        assert!(matches!(
            analyze_at_end("SELECT * FROM PUBLIC.ORDERS", DatabaseKind::Postgres).occurrences[0].resolution,
            MapResolution::CatalogMatch { oid: 1 }
        ));
        assert!(matches!(
            analyze_at_end("SELECT * FROM PUBLIC.ORDERS", DatabaseKind::SqlServer).occurrences[0].resolution,
            MapResolution::Unresolved { .. }
        ));
        let mut catalog = catalog(DatabaseKind::Postgres);
        let mut other = catalog.schemas[0].clone();
        other.name = "other".into();
        catalog.schemas.push(other);
        assert!(matches!(
            analyze("SELECT * FROM orders", DocOffset(5), &catalog).occurrences[0].resolution,
            MapResolution::Unresolved { .. }
        ));
    }

    #[test]
    fn ctes_derived_temporary_and_cross_database_sources_are_not_catalog_tables() {
        for kind in [DatabaseKind::Postgres, DatabaseKind::SqlServer] {
            let result = analyze_at_end(
                "WITH orders AS (SELECT * FROM public.people) SELECT * FROM orders o JOIN public.people p ON o.id=p.id",
                kind,
            );
            assert_eq!(result.occurrences.len(), 2);
            assert!(matches!(result.occurrences[0].resolution, MapResolution::Unresolved { .. }));
            assert!(result.incomplete && result.joins.is_empty());
            let derived =
                analyze_at_end("SELECT * FROM (SELECT * FROM public.orders) x JOIN public.people p ON x.id=p.id", kind);
            assert_eq!(derived.occurrences.len(), 1);
            assert!(derived.incomplete && derived.joins.is_empty());
            let cross = analyze_at_end("SELECT * FROM shop.public.orders", kind);
            assert!(matches!(cross.occurrences[0].resolution, MapResolution::Unresolved { .. }));
            assert!(cross.joins.is_empty());
        }
        let temp = analyze_at_end("SELECT * FROM #orders", DatabaseKind::SqlServer);
        assert!(temp.occurrences.is_empty() && temp.incomplete);
    }

    #[test]
    fn explain_wrappers_do_not_resolve_ctes_as_catalog_tables() {
        for prefix in ["EXPLAIN ", "EXPLAIN (ANALYZE, BUFFERS) ", "EXPLAIN ANALYZE "] {
            let sql = format!("{prefix}WITH orders AS (SELECT * FROM public.people) SELECT * FROM orders");
            let result = analyze_at_end(&sql, DatabaseKind::Postgres);
            assert_eq!(result.occurrences.len(), 1);
            assert!(
                matches!(result.occurrences[0].resolution, MapResolution::Unresolved { .. }),
                "{prefix}: {:?}",
                result.occurrences[0].resolution
            );
            assert!(result.incomplete && result.joins.is_empty());
        }
    }

    #[test]
    fn unsupported_on_scopes_never_contribute_equality_fragments() {
        for kind in [DatabaseKind::Postgres, DatabaseKind::SqlServer] {
            for predicate in [
                "a.id=b.id OR a.tenant=b.tenant",
                "a.id=b.id AND b.id>0",
                "CAST(a.id AS int)=b.id",
                "a.id=b.id AND EXISTS(SELECT 1)",
                "a.id::int=b.id",
                "(a.id=b.id)",
                "a.missing=b.id",
                "a.id=a.parent_id",
                "a.id=b.id AND",
            ] {
                let sql = format!("SELECT * FROM public.orders a JOIN public.people b ON {predicate}");
                let result = analyze_at_end(&sql, kind);
                assert!(result.incomplete && result.joins.is_empty(), "{kind:?}: {predicate}");
                assert_eq!(result.occurrences.len(), 2, "{predicate}");
            }
            let result = analyze_at_end("SELECT * FROM public.orders a JOIN public.people a ON a.id=a.id", kind);
            assert!(result.joins.is_empty() && result.incomplete);
        }
    }

    #[test]
    fn incomplete_aliases_qualifiers_and_comma_scopes_have_no_guessed_edges() {
        for kind in [DatabaseKind::Postgres, DatabaseKind::SqlServer] {
            for sql in [
                "SELECT * FROM public.orders AS JOIN public.people b ON orders.id=b.id",
                "SELECT * FROM public.orders a, public.people b JOIN public.people c ON a.id=c.id",
                "SELECT * FROM public.orders a JOIN public.people b ON c.id=b.id",
            ] {
                let result = analyze_at_end(sql, kind);
                assert!(result.incomplete && result.joins.is_empty(), "{sql}");
            }
            let result = analyze_at_end("SELECT * FROM public.orders.", kind);
            assert!(result.incomplete);
            assert!(matches!(result.occurrences[0].resolution, MapResolution::Unresolved { .. }));
        }
    }

    #[test]
    fn incomplete_lexemes_and_unsupported_sources_do_not_fabricate_joins() {
        for kind in [DatabaseKind::Postgres, DatabaseKind::SqlServer] {
            for suffix in [" AND (", " AND 'unterminated", " AND \"unterminated", " AND CAST(a.id AS int)=b.id"] {
                let sql = format!("SELECT * FROM public.orders a JOIN public.people b ON a.id=b.id{suffix}");
                let result = analyze_at_end(&sql, kind);
                assert!(result.incomplete && result.joins.is_empty(), "{sql}");
            }
            let result =
                analyze_at_end("SELECT * FROM public.orders a WITH (NOLOCK) JOIN public.people b ON a.id=b.id", kind);
            assert!(result.incomplete && result.joins.is_empty());
        }
    }

    #[test]
    fn a_cursor_after_a_terminal_semicolon_keeps_the_query_overlay() {
        let sql = "SELECT * FROM public.orders a JOIN public.people b ON a.id = b.id;";
        for kind in [DatabaseKind::Postgres, DatabaseKind::SqlServer] {
            let result = analyze_at_end(sql, kind);
            assert_eq!(result.occurrences.len(), 2, "{kind:?}");
            assert_eq!(result.joins.len(), 1, "{kind:?}");
        }
        let next = "SELECT * FROM public.orders;SELECT * FROM public.people;";
        let at = next.find(';').unwrap() + 1;
        let result = analyze(next, DocOffset(at as u32), &catalog(DatabaseKind::SqlServer));
        assert!(matches!(result.occurrences[0].resolution, MapResolution::CatalogMatch { oid: 2 }));
        assert!(analyze_at_end("SELECT * FROM public.orders;\n\n", DatabaseKind::SqlServer).statement.is_none());
    }

    #[test]
    fn statement_choice_uses_cursor_in_each_dialect_and_ignores_fake_sources() {
        for (kind, sql) in [
            (DatabaseKind::Postgres, "SELECT * FROM public.orders;\nSELECT * FROM public.people;"),
            (DatabaseKind::SqlServer, "SELECT * FROM public.orders\nGO\nSELECT * FROM public.people"),
        ] {
            let result = analyze(sql, DocOffset(10), &catalog(kind));
            assert_eq!(result.occurrences.len(), 1);
            assert!(matches!(result.occurrences[0].resolution, MapResolution::CatalogMatch { oid: 1 }));
            let result = analyze_at_end(sql, kind);
            assert!(matches!(result.occurrences[0].resolution, MapResolution::CatalogMatch { oid: 2 }));
            assert!(analyze_at_end("SELECT 'FROM public.orders -- JOIN public.people'", kind).occurrences.is_empty());
        }
    }
}
