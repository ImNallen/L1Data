//! What a connection's tree shows: which databases, and which schemas under each. Pure, no I/O.
//! The engine is the only writer; the pickers send `ViewEdit` intents that `apply` resolves
//! against the engine's own lists.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::connection::DatabaseInfo;
use crate::model::{Catalog, DbAccess, Preset, ViewEdit};

/// Persisted per connection in `connection_views`. Never crosses IPC: the UI sees the resolved
/// `InstanceState`. A connection with no row resolves as `initial()`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionView {
    pub databases: Selection,
    pub show_system_databases: bool,
    /// Keyed by datname. A database with no entry resolves through `database()`.
    pub schemas: BTreeMap<String, DatabaseView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseView {
    pub schemas: Selection,
    pub show_system_schemas: bool,
}

/// A mode, not a snapshot: `All` keeps showing members created later and `DefaultOnly` follows a
/// changed search_path. `Only` keeps names that no longer exist, so a dropped-then-recreated
/// object reappears; they simply do not count until then.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum Selection {
    All,
    DefaultOnly,
    Only { names: BTreeSet<String> },
}

impl Selection {
    fn preset(p: Preset) -> Self {
        match p {
            Preset::All => Selection::All,
            Preset::DefaultOnly => Selection::DefaultOnly,
            Preset::None => Selection::Only { names: BTreeSet::new() },
        }
    }

    fn set(&mut self, name: &str, shown: bool) {
        let Selection::Only { names } = self else { unreachable!("materialized before set") };
        if shown {
            names.insert(name.to_owned());
        } else {
            names.remove(name);
        }
    }
}

// Hand-written so a row this build writes still loads after a later build adds a field, as
// `Layout` does in model.rs.
impl<'de> Deserialize<'de> for ConnectionView {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Saved {
            databases: Option<Selection>,
            show_system_databases: Option<bool>,
            schemas: Option<BTreeMap<String, DatabaseView>>,
        }
        let s = Saved::deserialize(d)?;
        let initial = Self::initial();
        Ok(Self {
            databases: s.databases.unwrap_or(initial.databases),
            show_system_databases: s.show_system_databases.unwrap_or(initial.show_system_databases),
            schemas: s.schemas.unwrap_or(initial.schemas),
        })
    }
}

impl<'de> Deserialize<'de> for DatabaseView {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Saved {
            schemas: Option<Selection>,
            show_system_schemas: Option<bool>,
        }
        let s = Saved::deserialize(d)?;
        Ok(Self {
            schemas: s.schemas.unwrap_or(Selection::DefaultOnly),
            show_system_schemas: s.show_system_schemas.unwrap_or(false),
        })
    }
}

/// What the engine knows when it resolves: the `pg_database` list, the home database, and the
/// served catalog of every slot that has one.
pub struct Known<'a> {
    pub home: &'a str,
    pub databases: &'a [DatabaseInfo],
    pub catalogs: &'a HashMap<String, Arc<Catalog>>,
}

impl ConnectionView {
    /// The view of a connection with no stored row: the home database with all its schemas.
    pub fn initial() -> Self {
        Self { databases: Selection::DefaultOnly, show_system_databases: false, schemas: BTreeMap::new() }
    }

    /// The stored entry, else `All` for the home database so an upgrading user loses nothing, else
    /// `DefaultOnly`: "Adding a database also shows its default schema".
    pub fn database(&self, name: &str, home: &str) -> DatabaseView {
        match self.schemas.get(name) {
            Some(entry) => entry.clone(),
            None => DatabaseView {
                schemas: if name == home { Selection::All } else { Selection::DefaultOnly },
                show_system_schemas: false,
            },
        }
    }

    /// Applies one intent. Idempotent: `apply(apply(v, e), e) == apply(v, e)`.
    pub fn apply(&self, edit: &ViewEdit, known: &Known<'_>) -> ConnectionView {
        let mut next = self.clone();
        match edit {
            ViewEdit::Databases { preset } => next.databases = Selection::preset(*preset),
            ViewEdit::Database { name, shown } => {
                if !matches!(next.databases, Selection::Only { .. }) {
                    next.databases = Selection::Only { names: resolve_databases(self, known) };
                }
                next.databases.set(name, *shown);
                let entry = self.database(name, known.home);
                let empty = matches!(entry.schemas, Selection::Only { .. })
                    && known.catalogs.get(name).is_some_and(|c| resolve_schemas(&entry, c).is_empty());
                if *shown && empty {
                    next.schemas.insert(name.clone(), DatabaseView { schemas: Selection::DefaultOnly, ..entry });
                }
            }
            ViewEdit::ShowSystemDatabases { show } => next.show_system_databases = *show,
            ViewEdit::Schemas { database, preset } => {
                let mut entry = self.database(database, known.home);
                entry.schemas = Selection::preset(*preset);
                next.schemas.insert(database.clone(), entry);
            }
            ViewEdit::Schema { database, name, shown } => {
                let mut entry = self.database(database, known.home);
                if !matches!(entry.schemas, Selection::Only { .. }) {
                    let names = known.catalogs.get(database).map(|c| resolve_schemas(&entry, c)).unwrap_or_default();
                    entry.schemas = Selection::Only { names: names.into_iter().collect() };
                }
                entry.schemas.set(name, *shown);
                next.schemas.insert(database.clone(), entry);
            }
            ViewEdit::ShowSystemSchemas { database, show } => {
                let mut entry = self.database(database, known.home);
                entry.show_system_schemas = *show;
                next.schemas.insert(database.clone(), entry);
            }
        }
        next
    }
}

/// The databases the tree shows. A database without `DbAccess::Ok` is never shown, and a system
/// database only through `Only` while `show_system_databases` is on.
pub fn resolve_databases(view: &ConnectionView, known: &Known<'_>) -> BTreeSet<String> {
    known
        .databases
        .iter()
        .filter(|d| d.access == DbAccess::Ok)
        .filter(|d| match &view.databases {
            Selection::All => !d.system,
            Selection::DefaultOnly => d.name == known.home,
            Selection::Only { names } => names.contains(&d.name) && (!d.system || view.show_system_databases),
        })
        .map(|d| d.name.clone())
        .collect()
}

/// The schemas the tree shows under one database, in the catalog's name order.
pub fn resolve_schemas(view: &DatabaseView, catalog: &Catalog) -> Vec<String> {
    match &view.schemas {
        Selection::All => catalog.schemas.iter().filter(|s| !s.system).map(|s| s.name.clone()).collect(),
        Selection::DefaultOnly => catalog.default_schema.iter().cloned().collect(),
        Selection::Only { names } => catalog
            .schemas
            .iter()
            .filter(|s| names.contains(&s.name) && (!s.system || view.show_system_schemas))
            .map(|s| s.name.clone())
            .collect(),
    }
}

/// Postgres reserves the `pg_` prefix for its own schemas. `catalog.rs` applies the same rule in
/// SQL; this copy lets `system_schemas_to_load` change only when a system schema's visibility does.
pub fn is_system_schema(name: &str, kind: crate::model::DatabaseKind) -> bool {
    match kind {
        crate::model::DatabaseKind::Postgres => name == "information_schema" || name.starts_with("pg_"),
        crate::model::DatabaseKind::SqlServer => matches!(
            name,
            "sys"
                | "INFORMATION_SCHEMA"
                | "db_owner"
                | "db_accessadmin"
                | "db_securityadmin"
                | "db_ddladmin"
                | "db_backupoperator"
                | "db_datareader"
                | "db_datawriter"
                | "db_denydatareader"
                | "db_denydatawriter"
        ),
    }
}

/// The system schemas whose relations the catalog must load: the ones in `Only`, while
/// `show_system_schemas` is on. Types load for them too. Functions, sequences, and synonyms do not.
pub fn system_schemas_to_load(view: &DatabaseView, kind: crate::model::DatabaseKind) -> Vec<String> {
    match &view.schemas {
        Selection::Only { names } if view.show_system_schemas => {
            names.iter().filter(|n| is_system_schema(n, kind)).cloned().collect()
        }
        _ => Vec::new(),
    }
}

/// Shown databases past this many get a metadata session only when something asks for their
/// catalog, so `All` on a 200-database role does not open 200 sessions.
pub const AUTO_OPEN_LIMIT: usize = 20;

pub fn auto_open(shown: &BTreeSet<String>) -> BTreeSet<String> {
    shown.iter().take(AUTO_OPEN_LIMIT).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ConnectionId, DatabaseRef, SchemaInfo};

    fn db(name: &str, system: bool, access: DbAccess) -> DatabaseInfo {
        DatabaseInfo { name: name.into(), system, access }
    }

    fn catalog(database: &str, default: Option<&str>, schemas: &[(&str, bool)]) -> Arc<Catalog> {
        Arc::new(Catalog {
            database_kind: crate::model::DatabaseKind::Postgres,
            db: DatabaseRef { connection_id: ConnectionId(uuid::Uuid::nil()), database: database.into() },
            version: 1,
            default_schema: default.map(str::to_owned),
            schemas: schemas
                .iter()
                .map(|(name, system)| SchemaInfo {
                    name: (*name).into(),
                    quoted: (*name).into(),
                    system: *system,
                    table_count: 0,
                    relations: Vec::new(),
                    functions: Vec::new(),
                    sequences: Vec::new(),
                    types: Vec::new(),
                    synonyms: Vec::new(),
                })
                .collect(),
            extensions: Vec::new(),
        })
    }

    struct Server {
        databases: Vec<DatabaseInfo>,
        catalogs: HashMap<String, Arc<Catalog>>,
    }

    impl Server {
        fn seed() -> Self {
            Self {
                databases: vec![
                    db("postgres", false, DbAccess::Ok),
                    db("reporting", false, DbAccess::Ok),
                    db("secret", false, DbAccess::NoPrivilege),
                    db("shop", false, DbAccess::Ok),
                    db("template0", true, DbAccess::NoConnections),
                    db("template1", true, DbAccess::Ok),
                ],
                catalogs: HashMap::from([
                    (
                        "shop".to_owned(),
                        catalog(
                            "shop",
                            Some("public"),
                            &[("information_schema", true), ("lab", false), ("pg_catalog", true), ("public", false)],
                        ),
                    ),
                    (
                        "reporting".to_owned(),
                        catalog(
                            "reporting",
                            Some("public"),
                            &[("audit", false), ("pg_catalog", true), ("public", false)],
                        ),
                    ),
                ]),
            }
        }

        fn known(&self) -> Known<'_> {
            Known { home: "shop", databases: &self.databases, catalogs: &self.catalogs }
        }
    }

    fn names(v: &[&str]) -> BTreeSet<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    fn only(v: &[&str]) -> Selection {
        Selection::Only { names: names(v) }
    }

    fn shown_schemas(view: &ConnectionView, server: &Server, database: &str) -> Vec<String> {
        resolve_schemas(&view.database(database, "shop"), &server.catalogs[database])
    }

    #[test]
    fn the_initial_view_shows_the_home_database_with_all_its_schemas() {
        let server = Server::seed();
        let view = ConnectionView::initial();
        assert_eq!(resolve_databases(&view, &server.known()), names(&["shop"]));
        assert_eq!(shown_schemas(&view, &server, "shop"), ["lab", "public"]);
        assert_eq!(
            shown_schemas(&view, &server, "reporting"),
            ["public"],
            "a database with no entry shows its default schema"
        );
    }

    #[test]
    fn every_edit_is_idempotent() {
        let server = Server::seed();
        let known = server.known();
        let edits = [
            ViewEdit::Databases { preset: Preset::All },
            ViewEdit::Databases { preset: Preset::None },
            ViewEdit::Database { name: "reporting".into(), shown: true },
            ViewEdit::Database { name: "shop".into(), shown: false },
            ViewEdit::ShowSystemDatabases { show: true },
            ViewEdit::Schemas { database: "shop".into(), preset: Preset::DefaultOnly },
            ViewEdit::Schema { database: "shop".into(), name: "lab".into(), shown: false },
            ViewEdit::Schema { database: "reporting".into(), name: "audit".into(), shown: true },
            ViewEdit::ShowSystemSchemas { database: "shop".into(), show: true },
        ];
        let mut view = ConnectionView::initial();
        for edit in &edits {
            let once = view.apply(edit, &known);
            let twice = once.apply(edit, &known);
            assert_eq!(once, twice, "{edit:?}");
            view = once;
        }
    }

    #[test]
    fn all_keeps_a_database_and_a_schema_that_appear_later() {
        let mut server = Server::seed();
        let known = server.known();
        let view = ConnectionView::initial()
            .apply(&ViewEdit::Databases { preset: Preset::All }, &known)
            .apply(&ViewEdit::Schemas { database: "shop".into(), preset: Preset::All }, &known);
        assert_eq!(
            resolve_databases(&view, &known),
            names(&["postgres", "reporting", "shop"]),
            "no-access and system databases stay out"
        );

        server.databases.push(db("analytics", false, DbAccess::Ok));
        server.catalogs.insert(
            "shop".into(),
            catalog(
                "shop",
                Some("public"),
                &[("archive", false), ("lab", false), ("pg_catalog", true), ("public", false)],
            ),
        );
        assert_eq!(resolve_databases(&view, &server.known()), names(&["analytics", "postgres", "reporting", "shop"]));
        assert_eq!(shown_schemas(&view, &server, "shop"), ["archive", "lab", "public"]);
    }

    #[test]
    fn the_first_checkbox_materializes_the_resolved_set() {
        let server = Server::seed();
        let known = server.known();
        let from_default =
            ConnectionView::initial().apply(&ViewEdit::Database { name: "reporting".into(), shown: true }, &known);
        assert_eq!(from_default.databases, only(&["reporting", "shop"]));

        let all = ConnectionView::initial().apply(&ViewEdit::Databases { preset: Preset::All }, &known);
        let from_all = all.apply(&ViewEdit::Database { name: "postgres".into(), shown: false }, &known);
        assert_eq!(from_all.databases, only(&["reporting", "shop"]));

        let schema_from_all =
            from_all.apply(&ViewEdit::Schema { database: "shop".into(), name: "lab".into(), shown: false }, &known);
        assert_eq!(schema_from_all.schemas["shop"].schemas, only(&["public"]));
        let schema_from_default = from_all
            .apply(&ViewEdit::Schema { database: "reporting".into(), name: "audit".into(), shown: true }, &known);
        assert_eq!(schema_from_default.schemas["reporting"].schemas, only(&["audit", "public"]));
        let unknown_catalog = from_all
            .apply(&ViewEdit::Schema { database: "postgres".into(), name: "public".into(), shown: true }, &known);
        assert_eq!(
            unknown_catalog.schemas["postgres"].schemas,
            only(&["public"]),
            "an unknown catalog materializes as empty, then takes the name"
        );
    }

    #[test]
    fn showing_a_database_whose_selection_resolves_to_nothing_resets_it_to_its_default() {
        let server = Server::seed();
        let known = server.known();
        let mut view = ConnectionView::initial();
        view.schemas
            .insert("reporting".into(), DatabaseView { schemas: only(&["dropped"]), show_system_schemas: true });
        view.schemas
            .insert("postgres".into(), DatabaseView { schemas: only(&["dropped"]), show_system_schemas: false });

        let shown = view.apply(&ViewEdit::Database { name: "reporting".into(), shown: true }, &known);
        assert_eq!(
            shown.schemas["reporting"],
            DatabaseView { schemas: Selection::DefaultOnly, show_system_schemas: true }
        );
        assert_eq!(shown_schemas(&shown, &server, "reporting"), ["public"]);

        let deferred = view.apply(&ViewEdit::Database { name: "postgres".into(), shown: true }, &known);
        assert_eq!(
            deferred.schemas["postgres"].schemas,
            only(&["dropped"]),
            "without a catalog the check waits for the first load"
        );
    }

    #[test]
    fn none_shows_nothing() {
        let server = Server::seed();
        let known = server.known();
        let view = ConnectionView::initial()
            .apply(&ViewEdit::Databases { preset: Preset::None }, &known)
            .apply(&ViewEdit::Schemas { database: "shop".into(), preset: Preset::None }, &known);
        assert_eq!(resolve_databases(&view, &known), BTreeSet::new());
        assert_eq!(shown_schemas(&view, &server, "shop"), Vec::<String>::new());
    }

    #[test]
    fn a_system_schema_in_only_follows_the_toggle_without_being_forgotten() {
        let server = Server::seed();
        let known = server.known();
        let checked = ConnectionView::initial()
            .apply(&ViewEdit::Schema { database: "shop".into(), name: "pg_catalog".into(), shown: true }, &known);
        assert_eq!(shown_schemas(&checked, &server, "shop"), ["lab", "public"]);
        assert_eq!(
            system_schemas_to_load(&checked.database("shop", "shop"), crate::model::DatabaseKind::Postgres),
            Vec::<String>::new()
        );

        let on = checked.apply(&ViewEdit::ShowSystemSchemas { database: "shop".into(), show: true }, &known);
        assert_eq!(shown_schemas(&on, &server, "shop"), ["lab", "pg_catalog", "public"]);
        assert_eq!(
            system_schemas_to_load(&on.database("shop", "shop"), crate::model::DatabaseKind::Postgres),
            ["pg_catalog"]
        );

        let off = on.apply(&ViewEdit::ShowSystemSchemas { database: "shop".into(), show: false }, &known);
        assert_eq!(shown_schemas(&off, &server, "shop"), ["lab", "public"]);
        assert_eq!(off.schemas["shop"].schemas, only(&["lab", "pg_catalog", "public"]));
    }

    #[test]
    fn a_no_access_or_system_database_is_shown_only_when_allowed() {
        let server = Server::seed();
        let known = server.known();
        let view = ConnectionView::initial()
            .apply(&ViewEdit::Database { name: "secret".into(), shown: true }, &known)
            .apply(&ViewEdit::Database { name: "template1".into(), shown: true }, &known);
        assert_eq!(view.databases, only(&["secret", "shop", "template1"]));
        assert_eq!(resolve_databases(&view, &known), names(&["shop"]));
        let with_system = view.apply(&ViewEdit::ShowSystemDatabases { show: true }, &known);
        assert_eq!(
            resolve_databases(&with_system, &known),
            names(&["shop", "template1"]),
            "secret has no CONNECT privilege"
        );
    }

    #[test]
    fn auto_open_keeps_the_first_twenty_by_name() {
        let shown: BTreeSet<String> = (0..25).map(|i| format!("db{i:02}")).collect();
        let open = auto_open(&shown);
        assert_eq!(open.len(), 20);
        assert_eq!(open.iter().next().map(String::as_str), Some("db00"));
        assert_eq!(open.iter().last().map(String::as_str), Some("db19"));
        assert_eq!(auto_open(&names(&["a", "b"])), names(&["a", "b"]));
    }

    #[test]
    fn a_view_round_trips_through_json() {
        let mut view = ConnectionView {
            databases: only(&["reporting", "shop"]),
            show_system_databases: true,
            schemas: BTreeMap::new(),
        };
        view.schemas.insert("shop".into(), DatabaseView { schemas: Selection::All, show_system_schemas: false });
        view.schemas
            .insert("reporting".into(), DatabaseView { schemas: Selection::DefaultOnly, show_system_schemas: true });
        view.schemas
            .insert("postgres".into(), DatabaseView { schemas: only(&["pg_catalog"]), show_system_schemas: true });
        let json = serde_json::to_value(&view).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "databases": { "kind": "Only", "names": ["reporting", "shop"] },
                "showSystemDatabases": true,
                "schemas": {
                    "postgres": { "schemas": { "kind": "Only", "names": ["pg_catalog"] }, "showSystemSchemas": true },
                    "reporting": { "schemas": { "kind": "DefaultOnly" }, "showSystemSchemas": true },
                    "shop": { "schemas": { "kind": "All" }, "showSystemSchemas": false },
                }
            })
        );
        assert_eq!(serde_json::from_value::<ConnectionView>(json).unwrap(), view);
    }

    #[test]
    fn a_row_from_an_older_build_takes_the_initial_value_for_a_missing_field() {
        let view: ConnectionView = serde_json::from_value(serde_json::json!({
            "databases": { "kind": "All" },
            "schemas": { "shop": { "schemas": { "kind": "DefaultOnly" } } }
        }))
        .unwrap();
        assert_eq!(view.databases, Selection::All);
        assert!(!view.show_system_databases);
        assert_eq!(view.schemas["shop"], DatabaseView { schemas: Selection::DefaultOnly, show_system_schemas: false });
    }
}
