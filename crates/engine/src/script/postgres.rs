use super::{Ddl, Privilege, privileges};
use crate::text::{quote_ident_if_needed as ident, quote_literal};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Table {
    pub schema: String,
    pub name: String,
    pub unlogged: bool,
    pub foreign: Option<Foreign>,
    pub of_type: Option<String>,
    pub access_method: Option<String>,
    pub options: Vec<String>,
    pub tablespace: Option<String>,
    pub comment: Option<String>,
    pub columns: Vec<Column>,
    pub owned_sequences: Vec<Sequence>,
    pub sequence_states: Vec<SequenceState>,
    pub constraints: Vec<Constraint>,
    pub parent: Parent,
    pub partition_key: Option<String>,
    pub partitions: Vec<Table>,
    pub indexes: Vec<Index>,
    pub attached_indexes: Vec<AttachedIndex>,
    pub replica_identity: ReplicaIdentity,
    pub triggers: Vec<Trigger>,
    pub rules: Vec<Rule>,
    pub row_security: bool,
    pub force_row_security: bool,
    pub policies: Vec<Policy>,
    pub statistics: Vec<Statistics>,
    pub unscripted: Unscripted,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Unscripted {
    pub privileges: Vec<Privilege>,
    pub sequence_privileges: Vec<SequencePrivileges>,
    /// The relation, its columns, and its sequences that carry a security label, by name.
    pub security_labels: Vec<String>,
    pub publications: Vec<String>,
    /// Publications of every table in the relation's schema.
    pub schema_publications: Vec<String>,
    pub owner: Owner,
    /// Inherited not-null constraints whose name or `NOT VALID` state a replay cannot give them.
    pub inherited_not_nulls: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Owner {
    pub name: String,
    pub connecting_role: bool,
}

impl Default for Owner {
    fn default() -> Owner {
        Owner { name: String::new(), connecting_role: true }
    }
}

/// What a sequence the table owns carries that creating it does not give it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SequenceState {
    pub schema: String,
    pub name: String,
    pub unlogged: Option<bool>,
    pub comment: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SequencePrivileges {
    pub schema: String,
    pub name: String,
    pub privileges: Vec<Privilege>,
}

/// An index column's statistics target, by the column's number in the index.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ColumnStatistics {
    pub column: i32,
    pub target: i32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Foreign {
    pub server: String,
    pub options: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Parent {
    #[default]
    None,
    Partition {
        schema: String,
        name: String,
        bound: String,
    },
    Inherits(Vec<(String, String)>),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ReplicaIdentity {
    #[default]
    Default,
    Nothing,
    Full,
    Index,
}

/// A partition's copy of its parent's index, which the replay creates under a name PostgreSQL
/// chooses, so the script finds it by `parent`, the parent's index, and gives it back `name`.
/// The copy takes the parent index's storage parameters and tablespace, so `tablespace` is set
/// only when the copy's differs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AttachedIndex {
    pub name: String,
    pub parent: String,
    pub clustered: bool,
    pub replica_identity: bool,
    pub comment: Option<String>,
    pub options: Vec<String>,
    pub parent_options: Vec<String>,
    pub tablespace: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Column {
    pub name: String,
    pub type_name: String,
    pub collation: Option<(String, String)>,
    pub not_null: bool,
    pub default: Option<String>,
    pub generated: Option<Generated>,
    pub identity: Option<Identity>,
    pub comment: Option<String>,
    pub declared: bool,
    pub inherited: Option<Inherited>,
    pub statistics: Option<i32>,
    pub storage: String,
    pub type_storage: String,
    pub compression: String,
    pub options: Vec<String>,
    pub fdw_options: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Inherited {
    pub replay_order: i64,
    pub default: Option<String>,
    /// A parent passes its `NOT NULL` on, which a `NO INHERIT` one does not.
    pub not_null: bool,
    pub storage: String,
    pub compression: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Generated {
    Stored,
    Virtual,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub always: bool,
    pub sequence: Sequence,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sequence {
    pub schema: String,
    pub name: String,
    pub column: String,
    pub type_name: String,
    pub start: i64,
    pub increment: i64,
    pub min: i64,
    pub max: i64,
    pub cache: i64,
    pub cycle: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Constraint {
    pub name: String,
    pub definition: String,
    pub comment: Option<String>,
    pub after_indexes: bool,
    /// The storage parameters and tablespace of the constraint's index, which `pg_get_constraintdef` leaves out.
    pub index_options: Vec<String>,
    pub index_tablespace: Option<String>,
    /// Inherited or cloned from a parent, so the replay creates it and the script keeps only its comment.
    pub inherited: bool,
    /// A foreign key to another table of the same partition tree, which may not exist yet where the
    /// constraint's own table is created.
    pub after_tree: bool,
    /// The column of a `NOT NULL` constraint, which takes the place of the column's own `NOT NULL`.
    pub not_null_column: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Index {
    pub name: String,
    pub definition: Option<String>,
    pub comment: Option<String>,
    pub clustered: bool,
    pub replica_identity: bool,
    pub tablespace: Option<String>,
    pub statistics: Vec<ColumnStatistics>,
    pub invalid: bool,
    pub partitioned: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Trigger {
    pub name: String,
    pub definition: String,
    pub enabled: char,
    pub comment: Option<String>,
    pub cloned: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rule {
    pub name: String,
    pub definition: String,
    pub enabled: char,
    pub comment: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Policy {
    pub name: String,
    pub permissive: bool,
    pub command: char,
    pub roles: Vec<String>,
    pub using: Option<String>,
    pub check: Option<String>,
    pub comment: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Statistics {
    pub schema: String,
    pub name: String,
    pub definition: String,
    pub target: Option<i32>,
    pub comment: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct View {
    pub schema: String,
    pub name: String,
    pub materialized: bool,
    pub populated: bool,
    pub access_method: Option<String>,
    pub options: Vec<String>,
    pub tablespace: Option<String>,
    /// `pg_get_viewdef`, which ends in `;`.
    pub query: String,
    pub comment: Option<String>,
    pub columns: Vec<Column>,
    pub indexes: Vec<Index>,
    pub triggers: Vec<Trigger>,
    pub rules: Vec<Rule>,
    pub unscripted: Unscripted,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Routine {
    pub definition: String,
    pub procedure: bool,
    pub signature: String,
    pub comment: Option<String>,
    pub privileges: Vec<Privilege>,
    pub owner: Owner,
    pub security_definer: bool,
    pub extensions: Vec<String>,
    pub security_labels: bool,
}

pub fn qualified(schema: &str, name: &str) -> String {
    format!("{}.{}", ident(schema), ident(name))
}

pub fn table(t: &Table) -> Ddl {
    let mut statements = vec![];
    let mut warnings = vec![];
    table_statements(t, &mut statements, &mut warnings);
    tree_foreign_keys(t, &mut statements);
    Ddl { text: script(statements), warnings }
}

fn tree_foreign_keys(t: &Table, out: &mut Vec<String>) {
    let name = qualified(&t.schema, &t.name);
    for k in t.constraints.iter().filter(|k| k.after_tree && !k.inherited) {
        out.push(format!("ALTER TABLE {name} ADD CONSTRAINT {} {}", ident(&k.name), constraint_definition(k)));
        out.extend(comment(&format!("CONSTRAINT {} ON {name}", ident(&k.name)), &k.comment));
    }
    for p in &t.partitions {
        tree_foreign_keys(p, out);
    }
}

fn makes_index(k: &Constraint) -> bool {
    ["PRIMARY KEY", "UNIQUE", "EXCLUDE"].iter().any(|kind| k.definition.starts_with(kind))
}

/// A partition's constraint that backs an index is added after the partition's attached indexes
/// have their names back, because `CREATE TABLE ... PARTITION OF` creates the copies of the
/// parent's indexes first, and one of them may take the constraint's name.
fn table_statements(t: &Table, out: &mut Vec<String>, warnings: &mut Vec<String>) {
    let name = qualified(&t.schema, &t.name);
    let keyword = if t.foreign.is_some() { "FOREIGN TABLE" } else { "TABLE" };
    let partition = matches!(t.parent, Parent::Partition { .. });
    let (late, early): (Vec<_>, Vec<_>) =
        t.constraints.iter().filter(|k| !k.inherited && !k.after_tree).partition(|k| k.after_indexes);
    let (deferred, inline): (Vec<_>, Vec<_>) =
        early.into_iter().partition(|k| k.definition.ends_with(" NOT VALID") || partition && makes_index(k));
    let constraints = inline.iter().map(|k| format!("CONSTRAINT {} {}", ident(&k.name), constraint_definition(k)));
    out.extend(t.owned_sequences.iter().map(create_sequence));

    let mut head = format!("CREATE {}{keyword} {name}", if t.unlogged { "UNLOGGED " } else { "" });
    match (&t.parent, &t.of_type) {
        (Parent::Partition { schema, name: parent, bound }, _) => {
            head.push_str(&format!(" PARTITION OF {}", qualified(schema, parent)));
            let overrides = t
                .columns
                .iter()
                .filter_map(|c| generation_override(c).map(|g| format!("{} WITH OPTIONS{g}", ident(&c.name))));
            let body: Vec<_> = overrides.chain(constraints).collect();
            if !body.is_empty() {
                head.push_str(&format!(" (\n    {}\n)", body.join(",\n    ")));
            }
            head.push_str(&format!("\n    {bound}"));
        }
        (Parent::None, Some(of_type)) => {
            head.push_str(&format!(" OF {of_type}"));
            let options = t.columns.iter().map(|c| (c, column_options(c, t))).filter(|(_, o)| !o.is_empty());
            let body: Vec<_> =
                options.map(|(c, o)| format!("{} WITH OPTIONS{o}", ident(&c.name))).chain(constraints).collect();
            if !body.is_empty() {
                head.push_str(&format!(" (\n    {}\n)", body.join(",\n    ")));
            }
        }
        (parent, _) => {
            let columns = t.columns.iter().filter(|c| c.declared).map(|c| {
                let options = match &c.inherited {
                    Some(_) => c
                        .identity
                        .as_ref()
                        .filter(|_| !identity_waits(t, c))
                        .map(|i| format!(" {}", identity_clause(i, &t.schema, &t.name)))
                        .or_else(|| generation_override(c))
                        .or_else(|| merged_default(c))
                        .unwrap_or_default(),
                    None => column_options(c, t),
                };
                format!("{}{options}", column_head(c))
            });
            let body: Vec<_> = columns.chain(constraints).collect();
            head.push_str(&format!(" (\n    {}\n)", body.join(",\n    ")));
            if let Parent::Inherits(parents) = parent {
                head.push_str(&format!(
                    " INHERITS ({})",
                    parents.iter().map(|(s, n)| qualified(s, n)).collect::<Vec<_>>().join(", ")
                ));
            }
        }
    }
    if let Some(key) = &t.partition_key {
        head.push_str(&format!(" PARTITION BY {key}"));
    }
    if let Some(method) = &t.access_method {
        head.push_str(&format!(" USING {}", ident(method)));
    }
    if let Some(foreign) = &t.foreign {
        head.push_str(&format!(" SERVER {}{}", ident(&foreign.server), fdw_options("", &foreign.options)));
    }
    if !t.options.is_empty() {
        head.push_str(&format!(" WITH ({})", t.options.join(", ")));
    }
    if let Some(space) = &t.tablespace {
        head.push_str(&format!(" TABLESPACE {}", ident(space)));
    }
    out.push(head);
    out.extend(restore_attached_names(t));
    for c in t.columns.iter().filter(|c| identity_waits(t, c)) {
        out.extend(c.identity.as_ref().map(|i| {
            format!("ALTER TABLE {name} ALTER COLUMN {} ADD {}", ident(&c.name), identity_clause(i, &t.schema, &t.name))
        }));
    }

    out.extend(
        t.owned_sequences
            .iter()
            .map(|s| format!("ALTER SEQUENCE {} OWNED BY {name}.{}", qualified(&s.schema, &s.name), ident(&s.column))),
    );
    out.extend(t.sequence_states.iter().filter_map(|s| {
        s.unlogged.map(|unlogged| {
            format!(
                "ALTER SEQUENCE {} SET {}",
                qualified(&s.schema, &s.name),
                if unlogged { "UNLOGGED" } else { "LOGGED" }
            )
        })
    }));
    let add =
        |k: &&Constraint| format!("ALTER TABLE {name} ADD CONSTRAINT {} {}", ident(&k.name), constraint_definition(k));
    out.extend(deferred.iter().map(add));
    for c in &t.columns {
        out.extend(column_changes(
            c,
            keyword,
            &name,
            !partition && c.declared && c.inherited.is_none(),
            !partition && merged_default(c).is_some(),
            not_null_constraint(t, c).is_some(),
        ));
    }
    indexes(&t.indexes, keyword, &t.schema, &name, out, warnings);
    out.extend(late.iter().map(add));
    match &t.replica_identity {
        _ if t.foreign.is_some() => {}
        ReplicaIdentity::Nothing => out.push(format!("ALTER TABLE {name} REPLICA IDENTITY NOTHING")),
        ReplicaIdentity::Full => out.push(format!("ALTER TABLE {name} REPLICA IDENTITY FULL")),
        ReplicaIdentity::Default | ReplicaIdentity::Index => {}
    }
    for tr in &t.triggers {
        out.extend(trigger(tr, keyword, &name));
    }
    for r in &t.rules {
        out.extend(rule(r, keyword, &name));
    }
    if t.row_security {
        out.push(format!("ALTER TABLE {name} ENABLE ROW LEVEL SECURITY"));
    }
    if t.force_row_security {
        out.push(format!("ALTER TABLE {name} FORCE ROW LEVEL SECURITY"));
    }
    for p in &t.policies {
        out.push(policy(p, &name));
        out.extend(comment(&format!("POLICY {} ON {name}", ident(&p.name)), &p.comment));
    }
    for s in &t.statistics {
        let stat = qualified(&s.schema, &s.name);
        out.push(s.definition.clone());
        if let Some(target) = s.target {
            out.push(format!("ALTER STATISTICS {stat} SET STATISTICS {target}"));
        }
        out.extend(comment(&format!("STATISTICS {stat}"), &s.comment));
    }
    out.extend(comment(&format!("{keyword} {name}"), &t.comment));
    for k in t.constraints.iter().filter(|k| backing_index(t, k).is_none() && (!k.after_tree || k.inherited)) {
        out.extend(comment(&format!("CONSTRAINT {} ON {name}", ident(&k.name)), &k.comment));
    }
    for c in &t.columns {
        out.extend(comment(&format!("COLUMN {name}.{}", ident(&c.name)), &c.comment));
    }
    for s in &t.sequence_states {
        out.extend(comment(&format!("SEQUENCE {}", qualified(&s.schema, &s.name)), &s.comment));
    }
    warnings.extend(unscripted(&t.unscripted, &t.schema, &name, None));
    for c in t.columns.iter().filter(|c| !partition && !c.declared && generation_override(c).is_some()) {
        warnings.push(format!(
            "the generation expression of {name}.{}, which a replay takes from its parent",
            ident(&c.name)
        ));
    }
    if !in_replay_order(&t.columns) {
        warnings.push(format!("the column order of {name}, because a replay puts inherited columns first"));
    }
    for p in &t.partitions {
        table_statements(p, out, warnings);
    }
}

fn in_replay_order(columns: &[Column]) -> bool {
    let positions: Vec<i64> =
        columns.iter().map(|c| c.inherited.as_ref().map_or(i64::MAX, |p| p.replay_order)).collect();
    positions.windows(2).all(|w| w[0] <= w[1])
}

/// `decides` says why the owner matters even when it is the connecting role.
fn unscripted(u: &Unscripted, schema: &str, name: &str, decides: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = owner(&u.owner, name, decides).into_iter().collect();
    out.extend(privileges(name, &u.privileges));
    out.extend(u.sequence_privileges.iter().filter_map(|s| privileges(&qualified(&s.schema, &s.name), &s.privileges)));
    out.extend(u.security_labels.iter().map(|labeled| format!("security labels on {labeled}")));
    let membership = |publications: &[String], through: &str| {
        let list: Vec<String> = publications.iter().map(|p| ident(p)).collect();
        (!list.is_empty()).then(|| {
            format!(
                "membership of {name} in {} {}{through}",
                if list.len() == 1 { "publication" } else { "publications" },
                list.join(", ")
            )
        })
    };
    out.extend(membership(&u.publications, ""));
    out.extend(membership(&u.schema_publications, &format!(" through schema {}", ident(schema))));
    if !u.inherited_not_nulls.is_empty() {
        let list: Vec<String> = u.inherited_not_nulls.iter().map(|n| ident(n)).collect();
        let noun = if list.len() == 1 { "not-null constraint" } else { "not-null constraints" };
        out.push(format!(
            "{noun} {} of {name}, which a replay takes from its parent, with the parent's name and validated",
            list.join(", ")
        ));
    }
    out
}

fn owner(o: &Owner, object: &str, decides: Option<&str>) -> Option<String> {
    match decides {
        Some(why) => Some(format!("owner {} of {object}, {why}", ident(&o.name))),
        None => (!o.connecting_role).then(|| format!("owner {} of {object}", ident(&o.name))),
    }
}

/// The index parameters go before an exclusion constraint's `WHERE` and before `DEFERRABLE`.
fn constraint_definition(k: &Constraint) -> String {
    let mut parameters = String::new();
    if !k.index_options.is_empty() {
        parameters.push_str(&format!(" WITH ({})", k.index_options.join(", ")));
    }
    if let Some(space) = &k.index_tablespace {
        parameters.push_str(&format!(" USING INDEX TABLESPACE {}", ident(space)));
    }
    let at = [" WHERE ", " DEFERRABLE"]
        .iter()
        .filter_map(|needle| top_level(&k.definition, needle))
        .min()
        .unwrap_or(k.definition.len());
    format!("{}{parameters}{}", &k.definition[..at], &k.definition[at..])
}

fn not_null_constraint<'a>(t: &'a Table, c: &Column) -> Option<&'a Constraint> {
    t.constraints.iter().find(|k| !k.inherited && k.not_null_column.as_deref() == Some(c.name.as_str()))
}

/// In one `CREATE TABLE` a column's identity conflicts with a `NO INHERIT` not-null constraint on it,
/// so the identity is added once the constraint exists.
fn identity_waits(t: &Table, c: &Column) -> bool {
    not_null_constraint(t, c).is_some_and(|k| k.definition.ends_with(" NO INHERIT"))
}

fn column_options(c: &Column, t: &Table) -> String {
    let mut out = String::new();
    if let Some(identity) = c.identity.as_ref().filter(|_| !identity_waits(t, c)) {
        out.push_str(&format!(" {}", identity_clause(identity, &t.schema, &t.name)));
    }
    match (c.generated, &c.default) {
        (Some(kind), Some(expr)) => out.push_str(&generation(kind, expr)),
        (None, Some(expr)) => out.push_str(&format!(" DEFAULT {expr}")),
        _ => {}
    }
    if c.not_null && not_null_constraint(t, c).is_none() {
        out.push_str(" NOT NULL");
    }
    out
}

/// The inherited constraint that an attached index of `t` backs, which has the index's name.
fn backing_index<'a>(t: &'a Table, k: &Constraint) -> Option<&'a AttachedIndex> {
    t.attached_indexes.iter().find(|a| k.inherited && makes_index(k) && a.name == k.name)
}

/// Runs right after the partition is created. Every index of the tree that exists then holds its
/// source name, which no other index wants, or is one of this partition's new copies. So a copy
/// that holds a wanted name moves aside under a name made from its OID until its own entry. When
/// something the script did not create holds a wanted name, it keeps it, and the copy takes the
/// OID name with a warning, because the name PostgreSQL chose may be one the script creates next.
/// The statements that name a copy run on the copy the block found, so they never reach the other.
fn restore_attached_names(t: &Table) -> Option<String> {
    if t.attached_indexes.is_empty() {
        return None;
    }
    let relation = qualified(&t.schema, &t.name);
    let row = |a: &AttachedIndex, last: &str| {
        format!("        [{}, {}, {}]", quote_literal(&relation), quote_literal(&a.parent), quote_literal(last))
    };
    let names: Vec<String> = t.attached_indexes.iter().map(|a| row(a, &a.name)).collect();
    let then: Vec<String> = t
        .attached_indexes
        .iter()
        .flat_map(|a| attached_changes(t, a, &relation).into_iter().map(move |s| (a, s)))
        .map(|(a, s)| row(a, &s))
        .collect();
    let find = "        attached := (SELECT h.inhrelid FROM pg_catalog.pg_inherits h JOIN pg_catalog.pg_index i ON i.indexrelid = h.inhrelid
                     WHERE h.inhparent = pg_catalog.to_regclass(entry[2]) AND i.indrelid = entry[1]::regclass);";
    let mut body = format!(
        "DECLARE
    entry text[];
    attached regclass;
    occupant regclass;
BEGIN
    FOREACH entry SLICE 1 IN ARRAY ARRAY[
{}
    ] LOOP
{find}
        occupant := (SELECT c.oid FROM pg_catalog.pg_class c WHERE c.relname = entry[3]
                     AND c.relnamespace = (SELECT a.relnamespace FROM pg_catalog.pg_class a WHERE a.oid = attached));
        IF attached IS NULL THEN
            RAISE WARNING 'L1 Data found no index on % attached to %, so it could not name it %', entry[1], entry[2], entry[3];
        ELSIF occupant IS NULL THEN
            EXECUTE pg_catalog.format('ALTER INDEX %s RENAME TO %I', attached, entry[3]);
        ELSIF occupant <> attached AND EXISTS (SELECT FROM pg_catalog.pg_inherits h JOIN pg_catalog.pg_index i ON i.indexrelid = h.inhrelid
                                               WHERE h.inhrelid = occupant AND i.indrelid = entry[1]::regclass) THEN
            EXECUTE pg_catalog.format('ALTER INDEX %s RENAME TO %I', occupant, 'l1_' || occupant::oid);
            EXECUTE pg_catalog.format('ALTER INDEX %s RENAME TO %I', attached, entry[3]);
        ELSIF occupant <> attached THEN
            IF (SELECT c.relname FROM pg_catalog.pg_class c WHERE c.oid = attached) <> 'l1_' || attached::oid THEN
                EXECUTE pg_catalog.format('ALTER INDEX %s RENAME TO %I', attached, 'l1_' || attached::oid);
            END IF;
            RAISE WARNING 'L1 Data named index % instead of %, because %, which this script did not create, has that name', attached, entry[3], occupant;
        END IF;
    END LOOP;
",
        names.join(",\n")
    );
    if !then.is_empty() {
        body.push_str(&format!(
            "    FOREACH entry SLICE 1 IN ARRAY ARRAY[
{}
    ] LOOP
{find}
        IF attached IS NOT NULL THEN
            EXECUTE pg_catalog.format(entry[3], (SELECT c.relname FROM pg_catalog.pg_class c WHERE c.oid = attached), attached);
        END IF;
    END LOOP;
",
            then.join(",\n")
        ));
    }
    body.push_str("END");
    let tag = std::iter::once("$l1$".to_owned())
        .chain((1..).map(|n| format!("$l1_{n}$")))
        .find(|tag| !body.contains(tag.as_str()))
        .unwrap_or_default();
    Some(format!("DO {tag}\n{body}\n{tag}"))
}

/// `format` templates the restore block runs on a copy, with `%1$I` its name and `%2$s` the copy.
fn attached_changes(t: &Table, a: &AttachedIndex, relation: &str) -> Vec<String> {
    let escape = |s: &str| s.replace('%', "%%");
    let key = |o: &str| o.split_once('=').map_or(o, |(k, _)| k).to_owned();
    let mut out = vec![];
    let reset: Vec<String> =
        a.parent_options.iter().map(|o| key(o)).filter(|k| !a.options.iter().any(|o| key(o) == *k)).collect();
    if !reset.is_empty() {
        out.push(format!("ALTER INDEX %2$s RESET ({})", escape(&reset.join(", "))));
    }
    let set: Vec<&str> = a.options.iter().filter(|o| !a.parent_options.contains(o)).map(String::as_str).collect();
    if !set.is_empty() {
        out.push(format!("ALTER INDEX %2$s SET ({})", escape(&set.join(", "))));
    }
    if let Some(space) = &a.tablespace {
        out.push(format!("ALTER INDEX %2$s SET TABLESPACE {}", escape(&ident(space))));
    }
    let relation = escape(relation);
    if a.clustered {
        out.push(format!("ALTER TABLE {relation} CLUSTER ON %1$I"));
    }
    if a.replica_identity {
        out.push(format!("ALTER TABLE {relation} REPLICA IDENTITY USING INDEX %1$I"));
    }
    out.extend(a.comment.as_ref().map(|c| format!("COMMENT ON INDEX %2$s IS {}", escape(&quote_literal(c)))));
    for k in t.constraints.iter().filter(|k| backing_index(t, k) == Some(a)) {
        out.extend(
            k.comment
                .as_ref()
                .map(|c| format!("COMMENT ON CONSTRAINT %1$I ON {relation} IS {}", escape(&quote_literal(c)))),
        );
    }
    out
}

fn generation(kind: Generated, expr: &str) -> String {
    format!(" GENERATED ALWAYS AS ({expr}) {}", if kind == Generated::Stored { "STORED" } else { "VIRTUAL" })
}

/// `PARTITION OF` and `INHERITS` copy the parent's generation expression, which a child may override.
fn generation_override(c: &Column) -> Option<String> {
    let (kind, expr, parent) = (c.generated?, c.default.as_ref()?, c.inherited.as_ref()?);
    (parent.default.as_ref() != Some(expr)).then(|| generation(kind, expr))
}

/// A column the child declares and also inherits keeps its own default in `CREATE TABLE`,
/// because parents whose defaults conflict make the child's declaration fail without it.
fn merged_default(c: &Column) -> Option<String> {
    if !c.declared || c.inherited.is_none() || c.generated.is_some() || c.identity.is_some() {
        return None;
    }
    c.default.as_ref().map(|d| format!(" DEFAULT {d}"))
}

fn column_head(c: &Column) -> String {
    let mut out = format!("{} {}", ident(&c.name), c.type_name);
    if let Some((schema, name)) = &c.collation {
        out.push_str(&format!(
            " COLLATE {}",
            if schema == "pg_catalog" { ident(name) } else { qualified(schema, name) }
        ));
    }
    out
}

fn column_changes(
    c: &Column,
    keyword: &str,
    relation: &str,
    declared: bool,
    declared_default: bool,
    has_not_null_constraint: bool,
) -> Vec<String> {
    let (default, not_null, storage, compression) = match &c.inherited {
        _ if declared => (c.default.as_ref(), c.not_null, &c.type_storage, ""),
        Some(p) => (
            if declared_default { c.default.as_ref() } else { p.default.as_ref() },
            p.not_null,
            &p.storage,
            p.compression.as_str(),
        ),
        None => (None, false, &c.type_storage, ""),
    };
    let mut changes = vec![];
    if c.generated.is_none() && c.identity.is_none() && c.default.as_ref() != default {
        changes.push(c.default.as_ref().map_or("DROP DEFAULT".into(), |d| format!("SET DEFAULT {d}")));
    }
    if c.not_null != not_null && !has_not_null_constraint {
        changes.push(if c.not_null { "SET NOT NULL" } else { "DROP NOT NULL" }.into());
    }
    if let Some(target) = c.statistics {
        changes.push(format!("SET STATISTICS {target}"));
    }
    if &c.storage != storage {
        let kind = match c.storage.as_str() {
            "p" => "PLAIN",
            "e" => "EXTERNAL",
            "m" => "MAIN",
            _ => "EXTENDED",
        };
        changes.push(format!("SET STORAGE {kind}"));
    }
    if c.compression != compression {
        changes.push(format!(
            "SET COMPRESSION {}",
            match c.compression.as_str() {
                "p" => "pglz",
                "l" => "lz4",
                _ => "default",
            }
        ));
    }
    if !c.options.is_empty() {
        changes.push(format!("SET ({})", c.options.join(", ")));
    }
    if !c.fdw_options.is_empty() {
        changes.push(fdw_options("ADD ", &c.fdw_options).trim_start().to_owned());
    }
    changes
        .into_iter()
        .map(|change| format!("ALTER {keyword} {relation} ALTER COLUMN {} {change}", ident(&c.name)))
        .collect()
}

fn fdw_options(lead: &str, options: &[String]) -> String {
    if options.is_empty() {
        return String::new();
    }
    let pairs: Vec<String> = options
        .iter()
        .map(|o| {
            let (k, v) = o.split_once('=').unwrap_or((o, ""));
            format!("{lead}{} {}", ident(k), quote_literal(v))
        })
        .collect();
    format!(" OPTIONS ({})", pairs.join(", "))
}

fn sequence_options(s: &Sequence) -> Vec<String> {
    let (type_min, type_max) = match s.type_name.as_str() {
        "smallint" => (i64::from(i16::MIN), i64::from(i16::MAX)),
        "integer" => (i64::from(i32::MIN), i64::from(i32::MAX)),
        _ => (i64::MIN, i64::MAX),
    };
    let (min, max) = if s.increment > 0 { (1, type_max) } else { (type_min, -1) };
    let start = if s.increment > 0 { s.min } else { s.max };
    let mut options = vec![];
    if s.start != start {
        options.push(format!("START WITH {}", s.start));
    }
    if s.increment != 1 {
        options.push(format!("INCREMENT BY {}", s.increment));
    }
    if s.min != min {
        options.push(format!("MINVALUE {}", s.min));
    }
    if s.max != max {
        options.push(format!("MAXVALUE {}", s.max));
    }
    if s.cache != 1 {
        options.push(format!("CACHE {}", s.cache));
    }
    if s.cycle {
        options.push("CYCLE".into());
    }
    options
}

fn create_sequence(s: &Sequence) -> String {
    let options = sequence_options(s);
    let mut out = format!("CREATE SEQUENCE {} AS {}", qualified(&s.schema, &s.name), s.type_name);
    if !options.is_empty() {
        out.push_str(&format!(" {}", options.join(" ")));
    }
    out
}

/// A sequence scripted from its own row. `owned_column` empty means the sequence has no owner.
/// `sequence.column` stays empty here. The table path fills that field for an identity clause.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StandaloneSequence {
    pub sequence: Sequence,
    pub unlogged: bool,
    pub owned_schema: String,
    pub owned_table: String,
    pub owned_column: String,
    pub comment: Option<String>,
    pub privileges: Vec<Privilege>,
    pub owner: Owner,
    pub security_label: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptedObject {
    pub schema: String,
    pub name: String,
    pub comment: Option<String>,
    pub privileges: Vec<Privilege>,
    pub owner: Owner,
    pub security_label: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompositeField {
    pub name: String,
    pub type_name: String,
    pub collation: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeScript {
    pub subtype: String,
    pub opclass: Option<String>,
    pub collation: Option<String>,
    pub canonical: Option<String>,
    pub subtype_diff: Option<String>,
}

fn object_warnings(
    schema: &str,
    name: &str,
    privileges: &[Privilege],
    owner: &Owner,
    security_label: bool,
) -> Vec<String> {
    unscripted(
        &Unscripted {
            privileges: privileges.to_vec(),
            owner: owner.clone(),
            security_labels: if security_label { vec![name.to_owned()] } else { vec![] },
            ..Default::default()
        },
        schema,
        name,
        None,
    )
}

pub fn standalone_sequence(s: &StandaloneSequence) -> Ddl {
    let name = qualified(&s.sequence.schema, &s.sequence.name);
    let mut statements = vec![create_sequence(&s.sequence)];
    if !s.owned_column.is_empty() {
        statements.push(format!(
            "ALTER SEQUENCE {name} OWNED BY {}.{}",
            qualified(&s.owned_schema, &s.owned_table),
            ident(&s.owned_column)
        ));
    }
    if s.unlogged {
        statements.push(format!("ALTER SEQUENCE {name} SET UNLOGGED"));
    }
    statements.extend(comment(&format!("SEQUENCE {name}"), &s.comment));
    Ddl {
        text: script(statements),
        warnings: object_warnings(&s.sequence.schema, &name, &s.privileges, &s.owner, s.security_label),
    }
}

pub fn enum_type(o: &ScriptedObject, labels: &[String]) -> Ddl {
    let name = qualified(&o.schema, &o.name);
    let labels = labels.iter().map(|label| quote_literal(label)).collect::<Vec<_>>().join(", ");
    let mut statements = vec![format!("CREATE TYPE {name} AS ENUM ({labels})")];
    statements.extend(comment(&format!("TYPE {name}"), &o.comment));
    Ddl {
        text: script(statements),
        warnings: object_warnings(&o.schema, &name, &o.privileges, &o.owner, o.security_label),
    }
}

pub fn domain_type(
    o: &ScriptedObject,
    base: &str,
    collation: Option<&str>,
    default: Option<&str>,
    not_null: bool,
    checks: &[(String, String)],
) -> Ddl {
    let name = qualified(&o.schema, &o.name);
    let mut lines = vec![format!("CREATE DOMAIN {name} AS {base}")];
    if let Some(collation) = collation {
        lines.push(format!("    COLLATE {}", ident(collation)));
    }
    if let Some(default) = default {
        lines.push(format!("    DEFAULT {default}"));
    }
    if not_null {
        lines.push("    NOT NULL".into());
    }
    for (check_name, definition) in checks {
        lines.push(format!("    CONSTRAINT {} {definition}", ident(check_name)));
    }
    let mut statements = vec![lines.join("\n")];
    statements.extend(comment(&format!("DOMAIN {name}"), &o.comment));
    Ddl {
        text: script(statements),
        warnings: object_warnings(&o.schema, &name, &o.privileges, &o.owner, o.security_label),
    }
}

pub fn composite_type(o: &ScriptedObject, fields: &[CompositeField]) -> Ddl {
    let name = qualified(&o.schema, &o.name);
    let body = fields
        .iter()
        .map(|field| {
            let mut line = format!("    {} {}", ident(&field.name), field.type_name);
            if let Some(collation) = &field.collation {
                line.push_str(&format!(" COLLATE {}", ident(collation)));
            }
            line
        })
        .collect::<Vec<_>>()
        .join(",\n");
    let mut statements = vec![format!("CREATE TYPE {name} AS (\n{body}\n)")];
    statements.extend(comment(&format!("TYPE {name}"), &o.comment));
    Ddl {
        text: script(statements),
        warnings: object_warnings(&o.schema, &name, &o.privileges, &o.owner, o.security_label),
    }
}

pub fn range_type(o: &ScriptedObject, range: &RangeScript) -> Ddl {
    let name = qualified(&o.schema, &o.name);
    let mut lines = vec![format!("    SUBTYPE = {}", range.subtype)];
    if let Some(opclass) = &range.opclass {
        lines.push(format!("    SUBTYPE_OPCLASS = {opclass}"));
    }
    if let Some(collation) = &range.collation {
        lines.push(format!("    COLLATION = {}", ident(collation)));
    }
    if let Some(canonical) = &range.canonical {
        lines.push(format!("    CANONICAL = {canonical}"));
    }
    if let Some(diff) = &range.subtype_diff {
        lines.push(format!("    SUBTYPE_DIFF = {diff}"));
    }
    let mut statements = vec![format!("CREATE TYPE {name} AS RANGE (\n{}\n)", lines.join(",\n"))];
    statements.extend(comment(&format!("TYPE {name}"), &o.comment));
    Ddl {
        text: script(statements),
        warnings: object_warnings(&o.schema, &name, &o.privileges, &o.owner, o.security_label),
    }
}

pub fn extension_script(
    name: &str,
    schema: &str,
    version: &str,
    comment_text: Option<&str>,
    owner: &Owner,
    security_label: bool,
) -> Ddl {
    let quoted = ident(name);
    let stored = comment_text.map(str::to_owned);
    let mut statements =
        vec![format!("CREATE EXTENSION {quoted} WITH SCHEMA {} VERSION {}", ident(schema), quote_literal(version))];
    statements.extend(comment(&format!("EXTENSION {quoted}"), &stored));
    Ddl { text: script(statements), warnings: object_warnings(schema, &quoted, &[], owner, security_label) }
}

fn identity_clause(i: &Identity, schema: &str, table: &str) -> String {
    let s = &i.sequence;
    let mut options = sequence_options(s);
    if s.schema != schema || s.name != format!("{table}_{}_seq", s.column) {
        options.insert(0, format!("SEQUENCE NAME {}", qualified(&s.schema, &s.name)));
    }
    let head = format!("GENERATED {} AS IDENTITY", if i.always { "ALWAYS" } else { "BY DEFAULT" });
    if options.is_empty() { head } else { format!("{head} ({})", options.join(" ")) }
}

fn indexes(
    list: &[Index],
    keyword: &str,
    schema: &str,
    relation: &str,
    out: &mut Vec<String>,
    warnings: &mut Vec<String>,
) {
    for i in list {
        let name = ident(&i.name);
        let qualified_name = qualified(schema, &i.name);
        out.extend(i.definition.as_deref().map(index));
        out.extend(
            i.statistics
                .iter()
                .map(|s| format!("ALTER INDEX {qualified_name} ALTER COLUMN {} SET STATISTICS {}", s.column, s.target)),
        );
        match (i.invalid, i.partitioned) {
            (false, _) => {}
            (true, false) => {
                warnings.push(format!("the invalid state of index {qualified_name}, which a replay builds valid"))
            }
            (true, true) => warnings.push(format!(
                "the partial attachment of index {qualified_name}, which a replay attaches to every partition"
            )),
        }
        if let (Some(space), Some(_)) = (&i.tablespace, &i.definition) {
            out.push(format!("ALTER INDEX {} SET TABLESPACE {}", qualified(schema, &i.name), ident(space)));
        }
        if i.clustered {
            out.push(format!("ALTER {keyword} {relation} CLUSTER ON {name}"));
        }
        if i.replica_identity {
            out.push(format!("ALTER TABLE {relation} REPLICA IDENTITY USING INDEX {name}"));
        }
        out.extend(comment(&format!("INDEX {}", qualified(schema, &i.name)), &i.comment));
    }
}

pub fn view(v: &View) -> Ddl {
    let name = qualified(&v.schema, &v.name);
    let keyword = if v.materialized { "MATERIALIZED VIEW" } else { "VIEW" };
    let check = v.options.iter().find_map(|o| o.strip_prefix("check_option=")).map(|c| c.to_uppercase());
    let options: Vec<_> = v.options.iter().filter(|o| !o.starts_with("check_option=")).cloned().collect();
    let mut head = format!("CREATE {keyword} {name}");
    if let Some(method) = &v.access_method {
        head.push_str(&format!(" USING {}", ident(method)));
    }
    if !options.is_empty() {
        head.push_str(&format!(" WITH ({})", options.join(", ")));
    }
    if let Some(space) = &v.tablespace {
        head.push_str(&format!(" TABLESPACE {}", ident(space)));
    }
    head.push_str(&format!(" AS\n{}", v.query.trim_end().trim_end_matches(';')));
    if let Some(check) = check {
        head.push_str(&format!("\n  WITH {check} CHECK OPTION"));
    }
    if v.materialized {
        head.push_str(if v.populated { "\nWITH DATA" } else { "\nWITH NO DATA" });
    }
    let mut statements = vec![head];
    for c in &v.columns {
        statements.extend(column_changes(c, keyword, &name, false, false, false));
    }
    let decides = if v.materialized {
        "whose privileges a refresh runs the query with"
    } else {
        "whose privileges the view's query is checked against"
    };
    let mut warnings = unscripted(&v.unscripted, &v.schema, &name, Some(decides));
    indexes(&v.indexes, keyword, &v.schema, &name, &mut statements, &mut warnings);
    for tr in &v.triggers {
        statements.extend(trigger(tr, keyword, &name));
    }
    for r in &v.rules {
        statements.extend(rule(r, keyword, &name));
    }
    statements.extend(comment(&format!("{keyword} {name}"), &v.comment));
    for c in &v.columns {
        statements.extend(comment(&format!("COLUMN {name}.{}", ident(&c.name)), &c.comment));
    }
    Ddl { text: script(statements), warnings }
}

pub fn routine(r: &Routine) -> Ddl {
    let target = format!("{} {}", if r.procedure { "PROCEDURE" } else { "FUNCTION" }, r.signature);
    let mut statements = vec![r.definition.trim_end().to_owned()];
    statements.extend(r.extensions.iter().map(|e| format!("ALTER {target} DEPENDS ON EXTENSION {}", ident(e))));
    statements.extend(comment(&target, &r.comment));
    let mut warnings: Vec<String> = owner(
        &r.owner,
        &r.signature,
        r.security_definer.then_some("whose privileges the SECURITY DEFINER function runs with"),
    )
    .into_iter()
    .collect();
    warnings.extend(privileges(&r.signature, &r.privileges));
    if r.security_labels {
        warnings.push(format!("security labels on {}", r.signature));
    }
    Ddl { text: script(statements), warnings }
}

/// An index attached to `attached_to`, its parent table's index, is attached again, as `pg_dump` does.
pub fn index_script(
    i: &Index,
    constraint: Option<&Constraint>,
    attached_to: Option<&str>,
    keyword: &str,
    schema: &str,
    relation: &str,
) -> Ddl {
    let mut statements: Vec<String> = constraint
        .map(|k| format!("ALTER {keyword} {relation} ADD CONSTRAINT {} {}", ident(&k.name), constraint_definition(k)))
        .into_iter()
        .collect();
    let mut warnings = vec![];
    indexes(std::slice::from_ref(i), keyword, schema, relation, &mut statements, &mut warnings);
    statements.extend(
        attached_to.map(|parent| format!("ALTER INDEX {parent} ATTACH PARTITION {}", qualified(schema, &i.name))),
    );
    if let Some(k) = constraint {
        statements.extend(comment(&format!("CONSTRAINT {} ON {relation}", ident(&k.name)), &k.comment));
    }
    Ddl { text: script(statements), warnings }
}

/// `pg_get_indexdef` prints `ON ONLY` for a partitioned table's index, which would create the
/// index without its partitions' copies.
pub fn index(definition: &str) -> String {
    match top_level(definition, " ON ONLY ") {
        Some(at) => format!("{} ON {}", &definition[..at], &definition[at + " ON ONLY ".len()..]),
        None => definition.to_owned(),
    }
}

pub fn trigger(t: &Trigger, keyword: &str, table: &str) -> Vec<String> {
    let state = match t.enabled {
        'D' => Some("DISABLE"),
        'R' => Some("ENABLE REPLICA"),
        'A' => Some("ENABLE ALWAYS"),
        _ if t.cloned => Some("ENABLE"),
        _ => None,
    };
    (!t.cloned)
        .then(|| t.definition.clone())
        .into_iter()
        .chain(state.map(|s| {
            format!(
                "ALTER {} {table} {s} TRIGGER {}",
                if keyword == "FOREIGN TABLE" { keyword } else { "TABLE" },
                ident(&t.name)
            )
        }))
        .chain(comment(&format!("TRIGGER {} ON {table}", ident(&t.name)), &t.comment))
        .collect()
}

fn rule(r: &Rule, keyword: &str, relation: &str) -> Vec<String> {
    let state = match r.enabled {
        'D' => Some("DISABLE"),
        'R' => Some("ENABLE REPLICA"),
        'A' => Some("ENABLE ALWAYS"),
        _ => None,
    };
    let keyword = if keyword == "VIEW" { "VIEW" } else { "TABLE" };
    std::iter::once(r.definition.trim_end().trim_end_matches(';').to_owned())
        .chain(state.map(|s| format!("ALTER {keyword} {relation} {s} RULE {}", ident(&r.name))))
        .chain(comment(&format!("RULE {} ON {relation}", ident(&r.name)), &r.comment))
        .collect()
}

fn policy(p: &Policy, table: &str) -> String {
    let mut out = format!("CREATE POLICY {} ON {table}", ident(&p.name));
    if !p.permissive {
        out.push_str(" AS RESTRICTIVE");
    }
    let command = match p.command {
        'r' => Some("SELECT"),
        'a' => Some("INSERT"),
        'w' => Some("UPDATE"),
        'd' => Some("DELETE"),
        _ => None,
    };
    if let Some(command) = command {
        out.push_str(&format!(" FOR {command}"));
    }
    if !p.roles.is_empty() {
        out.push_str(&format!(" TO {}", p.roles.iter().map(|r| ident(r)).collect::<Vec<_>>().join(", ")));
    }
    if let Some(using) = &p.using {
        out.push_str(&format!(" USING ({using})"));
    }
    if let Some(check) = &p.check {
        out.push_str(&format!(" WITH CHECK ({check})"));
    }
    out
}

fn comment(target: &str, text: &Option<String>) -> Option<String> {
    text.as_ref().map(|t| format!("COMMENT ON {target} IS {}", quote_literal(t)))
}

pub fn script(statements: Vec<String>) -> String {
    statements.iter().map(|s| format!("{s};\n")).collect::<Vec<_>>().join("\n")
}

/// Byte offset of the first `needle` outside quotes and parentheses.
pub fn top_level(s: &str, needle: &str) -> Option<usize> {
    let mut walk = Walk::default();
    s.char_indices().find_map(|(i, c)| (walk.step(c) == 0 && !walk.quoted() && s[i..].starts_with(needle)).then_some(i))
}

/// Byte offset just past the `)` that closes the `(` at `open`.
pub fn group_end(s: &str, open: usize) -> Option<usize> {
    let mut walk = Walk::default();
    for (i, c) in s[open..].char_indices() {
        let closes = c == ')' && !walk.quoted();
        if walk.step(c) == 0 && closes {
            return Some(open + i + 1);
        }
    }
    None
}

/// Parenthesis depth outside single- and double-quoted text. A doubled quote inside a quoted
/// run closes and reopens it, which leaves the state right.
#[derive(Default)]
struct Walk {
    depth: u32,
    quote: Option<char>,
}

impl Walk {
    fn quoted(&self) -> bool {
        self.quote.is_some()
    }

    fn step(&mut self, c: char) -> u32 {
        match (self.quote, c) {
            (Some(q), c) if c == q => self.quote = None,
            (Some(_), _) => {}
            (None, '\'' | '"') => self.quote = Some(c),
            (None, '(') => self.depth += 1,
            (None, ')') => self.depth = self.depth.saturating_sub(1),
            _ => {}
        }
        self.depth
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script::Change;

    fn grant(privilege: &str, column: Option<&str>, grantee: &str, change: Change) -> Privilege {
        Privilege { name: privilege.into(), column: column.map(Into::into), grantee: grantee.into(), change }
    }

    fn sequence(start: i64, increment: i64, min: i64, max: i64) -> Sequence {
        Sequence {
            schema: "public".into(),
            name: "t_id_seq".into(),
            column: "id".into(),
            type_name: "bigint".into(),
            start,
            increment,
            min,
            max,
            cache: 1,
            cycle: false,
        }
    }

    fn col(name: &str, type_name: &str) -> Column {
        Column {
            name: name.into(),
            type_name: type_name.into(),
            declared: true,
            storage: "x".into(),
            type_storage: "x".into(),
            ..Default::default()
        }
    }

    #[test]
    fn a_table_carries_its_columns_constraints_indexes_triggers_rules_policies_and_comments() {
        let t = Table {
            schema: "public".into(),
            name: "t".into(),
            comment: Some("it's".into()),
            columns: vec![
                Column {
                    not_null: true,
                    identity: Some(Identity { always: false, sequence: sequence(1000, 10, 1, i64::MAX) }),
                    ..col("id", "bigint")
                },
                Column {
                    collation: Some(("pg_catalog".into(), "C".into())),
                    not_null: true,
                    storage: "e".into(),
                    statistics: Some(500),
                    ..col("code", "text")
                },
                Column {
                    default: Some("(price * qty)".into()),
                    generated: Some(Generated::Stored),
                    ..col("Total", "numeric(12,2)")
                },
                Column {
                    default: Some("'new'::text".into()),
                    comment: Some("lifecycle".into()),
                    compression: "l".into(),
                    options: vec!["n_distinct=5".into()],
                    ..col("status", "text")
                },
                Column { not_null: true, ..col("seq", "bigserial") },
            ],
            owned_sequences: vec![Sequence { name: "old_seq".into(), start: 777, ..sequence(1, 1, 1, i64::MAX) }],
            constraints: vec![
                Constraint { name: "t_pkey".into(), definition: "PRIMARY KEY (id)".into(), ..Default::default() },
                Constraint {
                    name: "positive".into(),
                    definition: "CHECK ((id > 0)) NOT VALID".into(),
                    comment: Some("checked later".into()),
                    ..Default::default()
                },
            ],
            indexes: vec![
                Index {
                    name: "t_pkey".into(),
                    comment: Some("pk".into()),
                    replica_identity: true,
                    ..Default::default()
                },
                Index {
                    name: "t_code".into(),
                    definition: Some("CREATE INDEX t_code ON public.t USING btree (code)".into()),
                    clustered: true,
                    ..Default::default()
                },
            ],
            replica_identity: ReplicaIdentity::Index,
            triggers: vec![Trigger {
                name: "audit".into(),
                definition:
                    "CREATE TRIGGER audit AFTER INSERT ON public.t FOR EACH ROW EXECUTE FUNCTION public.audit()".into(),
                enabled: 'D',
                comment: Some("tr".into()),
                cloned: false,
            }],
            rules: vec![Rule {
                name: "r".into(),
                definition: "CREATE RULE r AS\n    ON UPDATE TO public.t DO INSTEAD NOTHING;".into(),
                enabled: 'D',
                comment: None,
            }],
            row_security: true,
            force_row_security: true,
            policies: vec![
                Policy {
                    name: "own".into(),
                    permissive: false,
                    command: 'w',
                    roles: vec!["Admins".into(), "app".into()],
                    using: Some("(id > 0)".into()),
                    check: Some("true".into()),
                    comment: Some("p".into()),
                },
                Policy { name: "all".into(), permissive: true, command: '*', ..Default::default() },
            ],
            statistics: vec![Statistics {
                schema: "public".into(),
                name: "s".into(),
                definition: "CREATE STATISTICS public.s ON id, code FROM public.t".into(),
                target: Some(50),
                comment: None,
            }],
            ..Default::default()
        };
        assert_eq!(
            table(&t),
            Ddl::complete(
                "CREATE SEQUENCE public.old_seq AS bigint START WITH 777;

CREATE TABLE public.t (
    id bigint GENERATED BY DEFAULT AS IDENTITY (START WITH 1000 INCREMENT BY 10) NOT NULL,
    code text COLLATE \"C\" NOT NULL,
    \"Total\" numeric(12,2) GENERATED ALWAYS AS ((price * qty)) STORED,
    status text DEFAULT 'new'::text,
    seq bigserial NOT NULL,
    CONSTRAINT t_pkey PRIMARY KEY (id)
);

ALTER SEQUENCE public.old_seq OWNED BY public.t.id;

ALTER TABLE public.t ADD CONSTRAINT positive CHECK ((id > 0)) NOT VALID;

ALTER TABLE public.t ALTER COLUMN code SET STATISTICS 500;

ALTER TABLE public.t ALTER COLUMN code SET STORAGE EXTERNAL;

ALTER TABLE public.t ALTER COLUMN status SET COMPRESSION lz4;

ALTER TABLE public.t ALTER COLUMN status SET (n_distinct=5);

ALTER TABLE public.t REPLICA IDENTITY USING INDEX t_pkey;

COMMENT ON INDEX public.t_pkey IS 'pk';

CREATE INDEX t_code ON public.t USING btree (code);

ALTER TABLE public.t CLUSTER ON t_code;

CREATE TRIGGER audit AFTER INSERT ON public.t FOR EACH ROW EXECUTE FUNCTION public.audit();

ALTER TABLE public.t DISABLE TRIGGER audit;

COMMENT ON TRIGGER audit ON public.t IS 'tr';

CREATE RULE r AS
    ON UPDATE TO public.t DO INSTEAD NOTHING;

ALTER TABLE public.t DISABLE RULE r;

ALTER TABLE public.t ENABLE ROW LEVEL SECURITY;

ALTER TABLE public.t FORCE ROW LEVEL SECURITY;

CREATE POLICY own ON public.t AS RESTRICTIVE FOR UPDATE TO \"Admins\", app USING ((id > 0)) WITH CHECK (true);

COMMENT ON POLICY own ON public.t IS 'p';

CREATE POLICY \"all\" ON public.t;

CREATE STATISTICS public.s ON id, code FROM public.t;

ALTER STATISTICS public.s SET STATISTICS 50;

COMMENT ON TABLE public.t IS 'it''s';

COMMENT ON CONSTRAINT positive ON public.t IS 'checked later';

COMMENT ON COLUMN public.t.status IS 'lifecycle';
"
                .into()
            )
        );
    }

    #[test]
    fn a_foreign_key_to_its_own_tables_unique_index_follows_the_index() {
        let t = Table {
            schema: "app".into(),
            name: "tree".into(),
            columns: vec![Column { not_null: true, ..col("id", "integer") }, col("parent", "integer")],
            constraints: vec![Constraint {
                name: "tree_parent".into(),
                definition: "FOREIGN KEY (parent) REFERENCES app.tree(id)".into(),
                after_indexes: true,
                ..Default::default()
            }],
            indexes: vec![Index {
                name: "tree_id".into(),
                definition: Some("CREATE UNIQUE INDEX tree_id ON app.tree USING btree (id)".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(
            table(&t).text,
            "CREATE TABLE app.tree (\n    id integer NOT NULL,\n    parent integer\n);\n\nCREATE UNIQUE INDEX tree_id ON app.tree USING btree (id);\n\nALTER TABLE app.tree ADD CONSTRAINT tree_parent FOREIGN KEY (parent) REFERENCES app.tree(id);\n"
        );
    }

    #[test]
    fn a_child_declares_its_own_identity_on_a_column_it_also_inherits() {
        let identity = Identity {
            always: false,
            sequence: Sequence {
                schema: "app".into(),
                name: "kid_id_seq".into(),
                column: "id".into(),
                type_name: "integer".into(),
                start: 1,
                increment: 1,
                min: 1,
                max: 2147483647,
                cache: 1,
                cycle: false,
            },
        };
        let id = Column {
            not_null: true,
            identity: Some(identity),
            inherited: Some(Inherited { storage: "x".into(), ..Inherited::default() }),
            ..col("id", "integer")
        };
        let t = Table {
            schema: "app".into(),
            name: "kid".into(),
            parent: Parent::Inherits(vec![("app".into(), "plain".into())]),
            columns: vec![id],
            ..Default::default()
        };
        assert!(
            table(&t).text.starts_with(
                "CREATE TABLE app.kid (\n    id integer GENERATED BY DEFAULT AS IDENTITY\n) INHERITS (app.plain);\n"
            ),
            "{}",
            table(&t).text
        );
    }

    #[test]
    fn a_foreign_key_between_partitions_of_one_tree_follows_the_whole_tree() {
        let partition = |name: &str, constraints: Vec<Constraint>| Table {
            schema: "app".into(),
            name: name.into(),
            parent: Parent::Partition {
                schema: "app".into(),
                name: "r".into(),
                bound: format!("FOR VALUES IN ('{name}')"),
            },
            constraints,
            ..Default::default()
        };
        let fk = Constraint {
            name: "a_ref".into(),
            definition: "FOREIGN KEY (ref_id) REFERENCES app.b(id)".into(),
            comment: Some("to b".into()),
            after_tree: true,
            ..Default::default()
        };
        let root = Table {
            schema: "app".into(),
            name: "r".into(),
            partition_key: Some("LIST (k)".into()),
            partitions: vec![partition("a", vec![fk]), partition("b", vec![])],
            ..Default::default()
        };
        let text = table(&root).text;
        assert!(
            text.ends_with("CREATE TABLE app.b PARTITION OF app.r\n    FOR VALUES IN ('b');\n\nALTER TABLE app.a ADD CONSTRAINT a_ref FOREIGN KEY (ref_id) REFERENCES app.b(id);\n\nCOMMENT ON CONSTRAINT a_ref ON app.a IS 'to b';\n"),
            "{text}"
        );
    }

    #[test]
    fn a_child_declares_the_default_of_a_column_it_merges_from_parents_whose_defaults_conflict() {
        let merged = Column {
            default: Some("3".into()),
            inherited: Some(Inherited { default: Some("1".into()), storage: "x".into(), ..Inherited::default() }),
            ..col("a", "integer")
        };
        let t = Table {
            schema: "app".into(),
            name: "pab".into(),
            parent: Parent::Inherits(vec![("app".into(), "pa".into()), ("app".into(), "pb".into())]),
            columns: vec![merged],
            ..Default::default()
        };
        assert_eq!(table(&t).text, "CREATE TABLE app.pab (\n    a integer DEFAULT 3\n) INHERITS (app.pa, app.pb);\n");
    }

    #[test]
    fn a_partition_or_child_keeps_a_generation_expression_that_differs_from_its_parents() {
        let generated = |expr: &str, parent: &str, declared: bool| Column {
            generated: Some(Generated::Stored),
            default: Some(expr.into()),
            declared,
            inherited: Some(Inherited { default: Some(parent.into()), storage: "x".into(), ..Inherited::default() }),
            ..col("b", "integer")
        };
        let partition = |columns: Vec<Column>| Table {
            schema: "app".into(),
            name: "g_1".into(),
            parent: Parent::Partition { schema: "app".into(), name: "g".into(), bound: "FOR VALUES IN (1)".into() },
            columns,
            ..Default::default()
        };
        assert_eq!(
            table(&partition(vec![generated("(a * 3)", "(a * 2)", false)])).text,
            "CREATE TABLE app.g_1 PARTITION OF app.g (\n    b WITH OPTIONS GENERATED ALWAYS AS ((a * 3)) STORED\n)\n    FOR VALUES IN (1);\n"
        );
        assert_eq!(
            table(&partition(vec![generated("(a * 2)", "(a * 2)", false)])).text,
            "CREATE TABLE app.g_1 PARTITION OF app.g\n    FOR VALUES IN (1);\n"
        );
        let child = |declared: bool| Table {
            schema: "app".into(),
            name: "c".into(),
            parent: Parent::Inherits(vec![("app".into(), "p".into())]),
            columns: vec![generated("(a * 3)", "(a * 2)", declared)],
            ..Default::default()
        };
        assert_eq!(
            table(&child(true)).text,
            "CREATE TABLE app.c (\n    b integer GENERATED ALWAYS AS ((a * 3)) STORED\n) INHERITS (app.p);\n"
        );
        assert_eq!(
            table(&child(false)).warnings,
            ["the generation expression of app.c.b, which a replay takes from its parent"]
        );
    }

    const RESTORE_HEAD: &str = "DECLARE
    entry text[];
    attached regclass;
    occupant regclass;
BEGIN
    FOREACH entry SLICE 1 IN ARRAY ARRAY[
";

    const RESTORE_NAMES: &str = "    ] LOOP
        attached := (SELECT h.inhrelid FROM pg_catalog.pg_inherits h JOIN pg_catalog.pg_index i ON i.indexrelid = h.inhrelid
                     WHERE h.inhparent = pg_catalog.to_regclass(entry[2]) AND i.indrelid = entry[1]::regclass);
        occupant := (SELECT c.oid FROM pg_catalog.pg_class c WHERE c.relname = entry[3]
                     AND c.relnamespace = (SELECT a.relnamespace FROM pg_catalog.pg_class a WHERE a.oid = attached));
        IF attached IS NULL THEN
            RAISE WARNING 'L1 Data found no index on % attached to %, so it could not name it %', entry[1], entry[2], entry[3];
        ELSIF occupant IS NULL THEN
            EXECUTE pg_catalog.format('ALTER INDEX %s RENAME TO %I', attached, entry[3]);
        ELSIF occupant <> attached AND EXISTS (SELECT FROM pg_catalog.pg_inherits h JOIN pg_catalog.pg_index i ON i.indexrelid = h.inhrelid
                                               WHERE h.inhrelid = occupant AND i.indrelid = entry[1]::regclass) THEN
            EXECUTE pg_catalog.format('ALTER INDEX %s RENAME TO %I', occupant, 'l1_' || occupant::oid);
            EXECUTE pg_catalog.format('ALTER INDEX %s RENAME TO %I', attached, entry[3]);
        ELSIF occupant <> attached THEN
            IF (SELECT c.relname FROM pg_catalog.pg_class c WHERE c.oid = attached) <> 'l1_' || attached::oid THEN
                EXECUTE pg_catalog.format('ALTER INDEX %s RENAME TO %I', attached, 'l1_' || attached::oid);
            END IF;
            RAISE WARNING 'L1 Data named index % instead of %, because %, which this script did not create, has that name', attached, entry[3], occupant;
        END IF;
    END LOOP;
";

    const RESTORE_THEN: &str = "    ] LOOP
        attached := (SELECT h.inhrelid FROM pg_catalog.pg_inherits h JOIN pg_catalog.pg_index i ON i.indexrelid = h.inhrelid
                     WHERE h.inhparent = pg_catalog.to_regclass(entry[2]) AND i.indrelid = entry[1]::regclass);
        IF attached IS NOT NULL THEN
            EXECUTE pg_catalog.format(entry[3], (SELECT c.relname FROM pg_catalog.pg_class c WHERE c.oid = attached), attached);
        END IF;
    END LOOP;
";

    fn restore(names: &str, then: Option<&str>) -> String {
        let then =
            then.map(|t| format!("    FOREACH entry SLICE 1 IN ARRAY ARRAY[\n{t}\n{RESTORE_THEN}")).unwrap_or_default();
        format!("DO $l1$\n{RESTORE_HEAD}{names}\n{RESTORE_NAMES}{then}END\n$l1$;\n")
    }

    #[test]
    fn each_partition_restores_its_attached_index_names_right_after_it_is_created() {
        let clone = |name: &str, enabled: char, comment: Option<&str>| Trigger {
            name: name.into(),
            definition: "CREATE TRIGGER ignored".into(),
            enabled,
            comment: comment.map(Into::into),
            cloned: true,
        };
        let partition = |name: &str| Table {
            schema: "app".into(),
            name: name.into(),
            parent: Parent::Partition {
                schema: "app".into(),
                name: "ri".into(),
                bound: format!("FOR VALUES IN ({})", &name[3..]),
            },
            ..Default::default()
        };
        let t = Table {
            schema: "app".into(),
            name: "ri".into(),
            columns: vec![col("k", "integer")],
            partition_key: Some("LIST (k)".into()),
            partitions: vec![
                Table {
                    replica_identity: ReplicaIdentity::Index,
                    attached_indexes: vec![
                        AttachedIndex {
                            name: "ri_1_custom".into(),
                            parent: "app.ri_id_k".into(),
                            clustered: true,
                            replica_identity: true,
                            comment: Some("leaf 100%".into()),
                            ..Default::default()
                        },
                        AttachedIndex { name: "it's".into(), parent: "app.\"Odd\"".into(), ..Default::default() },
                    ],
                    triggers: vec![clone("off", 'D', None), clone("on", 'O', Some("leaf note"))],
                    indexes: vec![Index {
                        name: "ri_1_k_idx".into(),
                        definition: Some("CREATE INDEX ri_1_k_idx ON app.ri_1 USING btree (k) WHERE (k > 0)".into()),
                        ..Default::default()
                    }],
                    constraints: vec![
                        Constraint { name: "ri_1_own".into(), definition: "UNIQUE (k)".into(), ..Default::default() },
                        Constraint {
                            name: "ri_1_chk".into(),
                            definition: "CHECK ((k > 0))".into(),
                            ..Default::default()
                        },
                    ],
                    ..partition("ri_1")
                },
                Table {
                    attached_indexes: vec![AttachedIndex {
                        name: "ri_2 key".into(),
                        parent: "app.ri_id_k".into(),
                        ..Default::default()
                    }],
                    constraints: vec![
                        Constraint {
                            name: "ri_2 key".into(),
                            definition: "UNIQUE (id, k)".into(),
                            comment: Some("cloned".into()),
                            inherited: true,
                            ..Default::default()
                        },
                        Constraint {
                            name: "ri_chk".into(),
                            definition: "CHECK ((k > 0))".into(),
                            comment: Some("inherited check".into()),
                            inherited: true,
                            ..Default::default()
                        },
                    ],
                    ..partition("ri_2")
                },
            ],
            ..Default::default()
        };
        let ri_1 = restore(
            "        ['app.ri_1', 'app.ri_id_k', 'ri_1_custom'],\n        ['app.ri_1', 'app.\"Odd\"', 'it''s']",
            Some(
                "        ['app.ri_1', 'app.ri_id_k', 'ALTER TABLE app.ri_1 CLUSTER ON %1$I'],
        ['app.ri_1', 'app.ri_id_k', 'ALTER TABLE app.ri_1 REPLICA IDENTITY USING INDEX %1$I'],
        ['app.ri_1', 'app.ri_id_k', 'COMMENT ON INDEX %2$s IS ''leaf 100%%''']",
            ),
        );
        let ri_2 = restore(
            "        ['app.ri_2', 'app.ri_id_k', 'ri_2 key']",
            Some("        ['app.ri_2', 'app.ri_id_k', 'COMMENT ON CONSTRAINT %1$I ON app.ri_2 IS ''cloned''']"),
        );
        assert_eq!(
            table(&t).text,
            format!(
                "CREATE TABLE app.ri (
    k integer
) PARTITION BY LIST (k);

CREATE TABLE app.ri_1 PARTITION OF app.ri (
    CONSTRAINT ri_1_chk CHECK ((k > 0))
)
    FOR VALUES IN (1);

{ri_1}
ALTER TABLE app.ri_1 ADD CONSTRAINT ri_1_own UNIQUE (k);

CREATE INDEX ri_1_k_idx ON app.ri_1 USING btree (k) WHERE (k > 0);

ALTER TABLE app.ri_1 DISABLE TRIGGER off;

ALTER TABLE app.ri_1 ENABLE TRIGGER \"on\";

COMMENT ON TRIGGER \"on\" ON app.ri_1 IS 'leaf note';

CREATE TABLE app.ri_2 PARTITION OF app.ri
    FOR VALUES IN (2);

{ri_2}
COMMENT ON CONSTRAINT ri_chk ON app.ri_2 IS 'inherited check';
"
            )
        );
    }

    #[test]
    fn an_attached_index_sets_the_options_and_tablespace_that_differ_from_its_parent_index() {
        let index = AttachedIndex {
            name: "p_1_k_idx".into(),
            parent: "app.p_k".into(),
            options: vec!["fillfactor=33".into(), "deduplicate_items=off".into()],
            parent_options: vec!["fillfactor=50".into(), "deduplicate_items=off".into(), "autosummarize=on".into()],
            tablespace: Some("Fast".into()),
            ..Default::default()
        };
        let t = Table {
            schema: "app".into(),
            name: "p_1".into(),
            attached_indexes: vec![index],
            parent: Parent::Partition { schema: "app".into(), name: "p".into(), bound: "FOR VALUES IN (1)".into() },
            ..Default::default()
        };
        let then = "        ['app.p_1', 'app.p_k', 'ALTER INDEX %2$s RESET (autosummarize)'],
        ['app.p_1', 'app.p_k', 'ALTER INDEX %2$s SET (fillfactor=33)'],
        ['app.p_1', 'app.p_k', 'ALTER INDEX %2$s SET TABLESPACE \"Fast\"']";
        assert_eq!(
            table(&t).text,
            format!(
                "CREATE TABLE app.p_1 PARTITION OF app.p\n    FOR VALUES IN (1);\n\n{}",
                restore("        ['app.p_1', 'app.p_k', 'p_1_k_idx']", Some(then))
            )
        );
    }

    #[test]
    fn an_attached_index_restore_quotes_its_block_with_a_tag_no_name_contains() {
        let index = AttachedIndex { name: "x$l1$y$l1_1$".into(), parent: "app.p".into(), ..Default::default() };
        let t = Table { schema: "app".into(), name: "t".into(), attached_indexes: vec![index], ..Default::default() };
        let block = restore_attached_names(&t).unwrap();
        assert!(block.starts_with("DO $l1_2$\n") && block.ends_with("\n$l1_2$"), "{block}");
        assert_eq!(block.matches("$l1_2$").count(), 2, "{block}");
        assert!(block.contains("['app.t', 'app.p', 'x$l1$y$l1_1$']"), "{block}");
    }

    #[test]
    fn a_partition_tree_scripts_each_partition_after_its_parent_with_what_differs_from_it() {
        let inherited = |default: Option<&str>| {
            Some(Inherited {
                replay_order: 1,
                default: default.map(Into::into),
                not_null: false,
                storage: "p".into(),
                compression: String::new(),
            })
        };
        let partition = |name: &str, columns: Vec<Column>| Table {
            schema: "public".into(),
            name: name.into(),
            columns,
            parent: Parent::Partition { schema: "public".into(), name: "e".into(), bound: "FOR VALUES IN (1)".into() },
            ..Default::default()
        };
        let v = Column { declared: false, storage: "p".into(), type_storage: "p".into(), ..col("v", "integer") };
        let leaf = Table {
            parent: Parent::Partition { schema: "public".into(), name: "e1".into(), bound: "DEFAULT".into() },
            ..partition(
                "e1_rest",
                vec![Column { default: Some("7".into()), inherited: inherited(Some("7")), ..v.clone() }],
            )
        };
        let t = Table {
            schema: "public".into(),
            name: "e".into(),
            columns: vec![Column {
                default: Some("7".into()),
                storage: "p".into(),
                type_storage: "p".into(),
                ..col("v", "integer")
            }],
            partition_key: Some("LIST (v)".into()),
            partitions: vec![
                Table {
                    unlogged: true,
                    options: vec!["fillfactor=70".into()],
                    constraints: vec![Constraint {
                        name: "e1_note".into(),
                        definition: "CHECK ((v < 10))".into(),
                        ..Default::default()
                    }],
                    partition_key: Some("RANGE (v)".into()),
                    partitions: vec![leaf],
                    ..partition(
                        "e1",
                        vec![Column {
                            default: Some("9".into()),
                            not_null: true,
                            inherited: inherited(Some("7")),
                            ..v.clone()
                        }],
                    )
                },
                Table {
                    foreign: Some(Foreign {
                        server: "remote".into(),
                        options: vec!["table_name=a, b".into(), "Odd=it's".into()],
                    }),
                    parent: Parent::Partition {
                        schema: "public".into(),
                        name: "e".into(),
                        bound: "FOR VALUES IN (2)".into(),
                    },
                    ..partition(
                        "e2",
                        vec![Column {
                            inherited: inherited(Some("7")),
                            fdw_options: vec!["column_name=vv".into()],
                            ..v.clone()
                        }],
                    )
                },
            ],
            ..Default::default()
        };
        assert_eq!(
            table(&t).text,
            "CREATE TABLE public.e (
    v integer DEFAULT 7
) PARTITION BY LIST (v);

CREATE UNLOGGED TABLE public.e1 PARTITION OF public.e (
    CONSTRAINT e1_note CHECK ((v < 10))
)
    FOR VALUES IN (1) PARTITION BY RANGE (v) WITH (fillfactor=70);

ALTER TABLE public.e1 ALTER COLUMN v SET DEFAULT 9;

ALTER TABLE public.e1 ALTER COLUMN v SET NOT NULL;

CREATE TABLE public.e1_rest PARTITION OF public.e1
    DEFAULT;

CREATE FOREIGN TABLE public.e2 PARTITION OF public.e
    FOR VALUES IN (2) SERVER remote OPTIONS (table_name 'a, b', \"Odd\" 'it''s');

ALTER FOREIGN TABLE public.e2 ALTER COLUMN v DROP DEFAULT;

ALTER FOREIGN TABLE public.e2 ALTER COLUMN v OPTIONS (ADD column_name 'vv');
"
        );
    }

    #[test]
    fn an_inheritance_child_declares_its_own_columns_and_warns_when_a_replay_reorders_them() {
        let from_base = |position: i64| {
            Some(Inherited {
                replay_order: position,
                default: None,
                not_null: false,
                storage: "x".into(),
                compression: String::new(),
            })
        };
        let t = Table {
            schema: "app".into(),
            name: "child".into(),
            columns: vec![
                Column { declared: false, inherited: from_base(1), default: Some("2".into()), ..col("id", "integer") },
                Column { inherited: from_base(2), ..col("note", "text") },
                col("extra", "text"),
                Column { declared: false, inherited: from_base(3), ..col("late", "integer") },
            ],
            parent: Parent::Inherits(vec![("app".into(), "base".into()), ("app".into(), "Other".into())]),
            unscripted: Unscripted {
                privileges: vec![
                    grant("SELECT", None, "app", Change::Granted),
                    grant("UPDATE", Some("\"Mixed\""), "PUBLIC", Change::GrantedWithGrantOption),
                ],
                sequence_privileges: vec![SequencePrivileges {
                    schema: "app".into(),
                    name: "child_id_seq".into(),
                    privileges: vec![grant("USAGE", None, "app", Change::Granted)],
                }],
                security_labels: vec!["app.child".into(), "app.child.\"Mixed\"".into(), "app.child_id_seq".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        let ddl = table(&t);
        assert_eq!(
            ddl.text,
            "CREATE TABLE app.child (\n    note text,\n    extra text\n) INHERITS (app.base, app.\"Other\");\n\nALTER TABLE app.child ALTER COLUMN id SET DEFAULT 2;\n"
        );
        assert_eq!(
            ddl.warnings,
            [
                "privileges on app.child (SELECT granted to app, UPDATE (\"Mixed\") granted with grant option to PUBLIC)",
                "privileges on app.child_id_seq (USAGE granted to app)",
                "security labels on app.child",
                "security labels on app.child.\"Mixed\"",
                "security labels on app.child_id_seq",
                "the column order of app.child, because a replay puts inherited columns first",
            ]
        );
    }

    fn not_null(name: &str, column: &str, tail: &str) -> Constraint {
        Constraint {
            name: name.into(),
            definition: format!("NOT NULL {}{tail}", ident(column)),
            not_null_column: Some(column.into()),
            ..Default::default()
        }
    }

    #[test]
    fn a_not_null_constraint_takes_the_place_of_the_columns_own_not_null() {
        let required = |name: &str| Column { not_null: true, ..col(name, "integer") };
        let t = Table {
            schema: "app".into(),
            name: "t".into(),
            columns: vec![
                required("plain"),
                required("named"),
                required("Mi xed"),
                required("later"),
                required("local"),
            ],
            constraints: vec![
                Constraint { comment: Some("why".into()), ..not_null("t_named", "named", "") },
                not_null("Odd \"nn\"", "Mi xed", ""),
                not_null("t_later", "later", " NOT VALID"),
                not_null("t_local", "local", " NO INHERIT"),
            ],
            ..Default::default()
        };
        assert_eq!(
            table(&t).text,
            "CREATE TABLE app.t (
    plain integer NOT NULL,
    named integer,
    \"Mi xed\" integer,
    later integer,
    local integer,
    CONSTRAINT t_named NOT NULL named,
    CONSTRAINT \"Odd \"\"nn\"\"\" NOT NULL \"Mi xed\",
    CONSTRAINT t_local NOT NULL local NO INHERIT
);

ALTER TABLE app.t ADD CONSTRAINT t_later NOT NULL later NOT VALID;

COMMENT ON CONSTRAINT t_named ON app.t IS 'why';
"
        );
    }

    #[test]
    fn an_inherited_not_null_keeps_only_its_comment_and_leaves_the_column_to_its_parent() {
        let t = Table {
            schema: "app".into(),
            name: "kid".into(),
            columns: vec![Column {
                not_null: true,
                declared: false,
                inherited: Some(Inherited { not_null: true, storage: "x".into(), ..Default::default() }),
                ..col("id", "integer")
            }],
            constraints: vec![Constraint {
                inherited: true,
                comment: Some("from base".into()),
                ..not_null("base_id_not_null", "id", "")
            }],
            parent: Parent::Inherits(vec![("app".into(), "base".into())]),
            ..Default::default()
        };
        assert_eq!(
            table(&t).text,
            "CREATE TABLE app.kid (\n    \n) INHERITS (app.base);\n\nCOMMENT ON CONSTRAINT base_id_not_null ON app.kid IS 'from base';\n"
        );
    }

    #[test]
    fn a_partition_writes_its_own_not_null_constraint_instead_of_setting_not_null() {
        let from_parent = |not_null: bool| Some(Inherited { not_null, storage: "x".into(), ..Default::default() });
        let required = |name: &str, parent_requires: bool| Column {
            not_null: true,
            declared: false,
            inherited: from_parent(parent_requires),
            ..col(name, "integer")
        };
        let t = Table {
            schema: "app".into(),
            name: "p_1".into(),
            parent: Parent::Partition { schema: "app".into(), name: "p".into(), bound: "FOR VALUES IN (1)".into() },
            columns: vec![
                required("merged", true),
                required("own", false),
                required("later", false),
                required("set", false),
            ],
            constraints: vec![
                not_null("p_1_merged", "merged", ""),
                not_null("p_1_own", "own", ""),
                not_null("p_1_later", "later", " NOT VALID"),
            ],
            ..Default::default()
        };
        assert_eq!(
            table(&t).text,
            "CREATE TABLE app.p_1 PARTITION OF app.p (
    CONSTRAINT p_1_merged NOT NULL merged,
    CONSTRAINT p_1_own NOT NULL own
)
    FOR VALUES IN (1);

ALTER TABLE app.p_1 ADD CONSTRAINT p_1_later NOT NULL later NOT VALID;

ALTER TABLE app.p_1 ALTER COLUMN set SET NOT NULL;
"
        );
    }

    #[test]
    fn a_typed_table_writes_a_not_null_constraint_without_with_options() {
        let t = Table {
            schema: "app".into(),
            name: "pets".into(),
            of_type: Some("app.pet".into()),
            columns: vec![
                Column { not_null: true, ..col("id", "integer") },
                Column { not_null: true, ..col("kind", "text") },
            ],
            constraints: vec![not_null("pets_id", "id", "")],
            ..Default::default()
        };
        assert_eq!(
            table(&t).text,
            "CREATE TABLE app.pets OF app.pet (\n    kind WITH OPTIONS NOT NULL,\n    CONSTRAINT pets_id NOT NULL id\n);\n"
        );
    }

    #[test]
    fn an_identity_waits_for_a_no_inherit_not_null_constraint_it_would_conflict_with() {
        let sequence = Sequence {
            schema: "app".into(),
            name: "t_id_seq".into(),
            column: "id".into(),
            type_name: "integer".into(),
            start: 1,
            increment: 1,
            min: 1,
            max: i64::from(i32::MAX),
            cache: 1,
            cycle: false,
        };
        let column =
            Column { not_null: true, identity: Some(Identity { always: true, sequence }), ..col("id", "integer") };
        let t = |constraint: Constraint| Table {
            schema: "app".into(),
            name: "t".into(),
            columns: vec![column.clone()],
            constraints: vec![constraint],
            ..Default::default()
        };
        assert_eq!(
            table(&t(not_null("t_id_not_null", "id", " NO INHERIT"))).text,
            "CREATE TABLE app.t (\n    id integer,\n    CONSTRAINT t_id_not_null NOT NULL id NO INHERIT\n);\n\nALTER TABLE app.t ALTER COLUMN id ADD GENERATED ALWAYS AS IDENTITY;\n"
        );
        assert_eq!(
            table(&t(not_null("t_named", "id", ""))).text,
            "CREATE TABLE app.t (\n    id integer GENERATED ALWAYS AS IDENTITY,\n    CONSTRAINT t_named NOT NULL id\n);\n"
        );
    }

    #[test]
    fn inherited_not_nulls_a_replay_cannot_reproduce_are_warned_once_per_table() {
        let warnings = |names: &[&str]| {
            table(&Table {
                schema: "app".into(),
                name: "p_1".into(),
                unscripted: Unscripted {
                    inherited_not_nulls: names.iter().map(|n| n.to_string()).collect(),
                    ..Default::default()
                },
                ..Default::default()
            })
            .warnings
        };
        assert_eq!(warnings(&[]), Vec::<String>::new());
        assert_eq!(
            warnings(&["p_1_id"]),
            [
                "not-null constraint p_1_id of app.p_1, which a replay takes from its parent, with the parent's name and validated"
            ]
        );
        assert_eq!(
            warnings(&["Odd nn", "p_1_id"]),
            [
                "not-null constraints \"Odd nn\", p_1_id of app.p_1, which a replay takes from its parent, with the parent's name and validated"
            ]
        );
    }

    #[test]
    fn a_typed_table_takes_its_columns_from_its_type_and_declares_only_their_options() {
        let typed = |columns: Vec<Column>, constraints: Vec<Constraint>| Table {
            schema: "app".into(),
            name: "pets".into(),
            of_type: Some("app.pet".into()),
            columns,
            constraints,
            ..Default::default()
        };
        let t = typed(
            vec![
                col("id", "integer"),
                Column { default: Some("'cat'::text".into()), not_null: true, ..col("kind", "text") },
                Column { not_null: true, ..col("name", "text") },
            ],
            vec![Constraint { name: "pets_pkey".into(), definition: "PRIMARY KEY (id)".into(), ..Default::default() }],
        );
        assert_eq!(
            table(&t).text,
            "CREATE TABLE app.pets OF app.pet (\n    kind WITH OPTIONS DEFAULT 'cat'::text NOT NULL,\n    name WITH OPTIONS NOT NULL,\n    CONSTRAINT pets_pkey PRIMARY KEY (id)\n);\n"
        );
        assert_eq!(table(&typed(vec![col("id", "integer")], vec![])).text, "CREATE TABLE app.pets OF app.pet;\n");
    }

    #[test]
    fn identity_prints_only_the_options_that_differ_from_the_defaults() {
        let clause = |s: Sequence| identity_clause(&Identity { always: false, sequence: s }, "public", "t");
        assert_eq!(clause(sequence(1, 1, 1, i64::MAX)), "GENERATED BY DEFAULT AS IDENTITY");
        assert_eq!(
            identity_clause(
                &Identity {
                    always: true,
                    sequence: Sequence { type_name: "integer".into(), ..sequence(1, 1, 1, i64::from(i32::MAX)) }
                },
                "public",
                "t"
            ),
            "GENERATED ALWAYS AS IDENTITY"
        );
        assert_eq!(
            clause(Sequence { type_name: "smallint".into(), ..sequence(-1, -1, i64::from(i16::MIN), -1) }),
            "GENERATED BY DEFAULT AS IDENTITY (INCREMENT BY -1)"
        );
        assert_eq!(
            clause(Sequence { cache: 20, cycle: true, ..sequence(5, 1, 5, 100) }),
            "GENERATED BY DEFAULT AS IDENTITY (MINVALUE 5 MAXVALUE 100 CACHE 20 CYCLE)"
        );
        assert_eq!(clause(sequence(50, 1, 1, i64::MAX)), "GENERATED BY DEFAULT AS IDENTITY (START WITH 50)");
        assert_eq!(
            clause(Sequence { name: "custom".into(), ..sequence(1, 1, 1, i64::MAX) }),
            "GENERATED BY DEFAULT AS IDENTITY (SEQUENCE NAME public.custom)"
        );
    }

    #[test]
    fn views_keep_their_options_check_option_population_column_defaults_and_rules() {
        let v = View {
            schema: "public".into(),
            name: "paid".into(),
            options: vec!["security_barrier=true".into(), "check_option=cascaded".into()],
            query: " SELECT orders.id\n   FROM public.orders;".into(),
            comment: Some("paid only".into()),
            columns: vec![Column {
                default: Some("42".into()),
                comment: Some("the id".into()),
                ..col("id", "integer")
            }],
            rules: vec![Rule {
                name: "ins".into(),
                definition: "CREATE RULE ins AS\n    ON INSERT TO public.paid DO INSTEAD NOTHING;".into(),
                enabled: 'O',
                comment: None,
            }],
            unscripted: Unscripted {
                privileges: vec![grant("SELECT", None, "PUBLIC", Change::Revoked)],
                owner: Owner { name: "app".into(), connecting_role: true },
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(
            view(&v),
            Ddl {
                text: "CREATE VIEW public.paid WITH (security_barrier=true) AS\n SELECT orders.id\n   FROM public.orders\n  WITH CASCADED CHECK OPTION;\n\nALTER VIEW public.paid ALTER COLUMN id SET DEFAULT 42;\n\nCREATE RULE ins AS\n    ON INSERT TO public.paid DO INSTEAD NOTHING;\n\nCOMMENT ON VIEW public.paid IS 'paid only';\n\nCOMMENT ON COLUMN public.paid.id IS 'the id';\n".into(),
                warnings: vec!["owner app of public.paid, whose privileges the view's query is checked against".into(), "privileges on public.paid (SELECT revoked from PUBLIC)".into()],
            }
        );
        let m = View {
            schema: "public".into(),
            name: "m".into(),
            materialized: true,
            query: " SELECT 1 AS day;".into(),
            columns: vec![Column { statistics: Some(200), ..col("day", "integer") }],
            indexes: vec![Index {
                name: "m_day".into(),
                definition: Some("CREATE UNIQUE INDEX m_day ON public.m USING btree (day)".into()),
                clustered: true,
                ..Default::default()
            }],
            ..Default::default()
        };
        let owned = View {
            unscripted: Unscripted {
                owner: Owner { name: "Bob".into(), connecting_role: false },
                ..Default::default()
            },
            ..m.clone()
        };
        assert_eq!(
            view(&owned).warnings,
            ["owner \"Bob\" of public.m, whose privileges a refresh runs the query with"]
        );
        assert_eq!(
            view(&m).text,
            "CREATE MATERIALIZED VIEW public.m AS\n SELECT 1 AS day\nWITH NO DATA;\n\nALTER MATERIALIZED VIEW public.m ALTER COLUMN day SET STATISTICS 200;\n\nCREATE UNIQUE INDEX m_day ON public.m USING btree (day);\n\nALTER MATERIALIZED VIEW public.m CLUSTER ON m_day;\n"
        );
    }

    #[test]
    fn a_routine_ends_its_definition_and_keeps_its_comment_and_extension_dependencies() {
        let p = Routine {
            definition:
                "CREATE OR REPLACE PROCEDURE public.p(a integer)\n LANGUAGE sql\nAS $procedure$ SELECT 1 $procedure$\n"
                    .into(),
            procedure: true,
            signature: "public.p(a integer)".into(),
            comment: Some("x".into()),
            extensions: vec!["plpgsql".into(), "Odd ext".into()],
            ..Default::default()
        };
        assert_eq!(
            routine(&p),
            Ddl::complete(
                "CREATE OR REPLACE PROCEDURE public.p(a integer)\n LANGUAGE sql\nAS $procedure$ SELECT 1 $procedure$;\n\nALTER PROCEDURE public.p(a integer) DEPENDS ON EXTENSION plpgsql;\n\nALTER PROCEDURE public.p(a integer) DEPENDS ON EXTENSION \"Odd ext\";\n\nCOMMENT ON PROCEDURE public.p(a integer) IS 'x';\n".into()
            )
        );
        let revoked = Routine {
            privileges: vec![grant("EXECUTE", None, "PUBLIC", Change::Revoked)],
            signature: "public.p(integer)".into(),
            ..p.clone()
        };
        assert_eq!(routine(&revoked).warnings, ["privileges on public.p(integer) (EXECUTE revoked from PUBLIC)"]);
        let labeled = Routine { security_labels: true, ..revoked };
        assert_eq!(
            routine(&labeled).warnings,
            ["privileges on public.p(integer) (EXECUTE revoked from PUBLIC)", "security labels on public.p(integer)"]
        );
    }

    #[test]
    fn a_routine_warns_about_an_owner_other_than_the_connecting_role_and_always_for_security_definer() {
        let f = |owner: &str, connecting_role: bool, security_definer: bool| {
            routine(&Routine {
                definition: "CREATE FUNCTION app.f() ...".into(),
                signature: "app.f()".into(),
                owner: Owner { name: owner.into(), connecting_role },
                security_definer,
                ..Default::default()
            })
            .warnings
        };
        assert!(f("me", true, false).is_empty());
        assert_eq!(f("Bob", false, false), ["owner \"Bob\" of app.f()"]);
        assert_eq!(
            f("me", true, true),
            ["owner me of app.f(), whose privileges the SECURITY DEFINER function runs with"]
        );
    }

    #[test]
    fn a_table_warns_about_an_owner_other_than_the_connecting_role_on_itself_and_each_partition() {
        let leaf = Table {
            schema: "app".into(),
            name: "p_1".into(),
            parent: Parent::Partition { schema: "app".into(), name: "p".into(), bound: "FOR VALUES IN (1)".into() },
            unscripted: Unscripted {
                owner: Owner { name: "bob".into(), connecting_role: false },
                ..Default::default()
            },
            ..Default::default()
        };
        let t = Table {
            schema: "app".into(),
            name: "p".into(),
            columns: vec![col("k", "integer")],
            partition_key: Some("LIST (k)".into()),
            partitions: vec![leaf],
            ..Default::default()
        };
        assert_eq!(table(&t).warnings, ["owner bob of app.p_1"]);
    }

    #[test]
    fn a_constraint_keeps_its_index_storage_parameters_and_tablespace_before_where_and_deferrable() {
        let t = Table {
            schema: "app".into(),
            name: "t".into(),
            columns: vec![col("id", "integer"), col("b", "integer")],
            constraints: vec![
                Constraint {
                    name: "t_pkey".into(),
                    definition: "PRIMARY KEY (id) INCLUDE (b) DEFERRABLE INITIALLY DEFERRED".into(),
                    index_options: vec!["fillfactor=70".into(), "deduplicate_items=off".into()],
                    index_tablespace: Some("Fast".into()),
                    ..Default::default()
                },
                Constraint {
                    name: "t_b_key".into(),
                    definition: "UNIQUE (b)".into(),
                    index_options: vec!["fillfactor=80".into()],
                    ..Default::default()
                },
                Constraint {
                    name: "t_excl".into(),
                    definition: "EXCLUDE USING gist (b WITH =) WITH (fillfactor='60') WHERE ((b > 0))".into(),
                    index_tablespace: Some("fast".into()),
                    ..Default::default()
                },
            ],
            indexes: vec![Index { name: "t_pkey".into(), tablespace: Some("Fast".into()), ..Default::default() }],
            ..Default::default()
        };
        assert_eq!(
            table(&t).text,
            "CREATE TABLE app.t (\n    id integer,\n    b integer,\n    CONSTRAINT t_pkey PRIMARY KEY (id) INCLUDE (b) WITH (fillfactor=70, deduplicate_items=off) USING INDEX TABLESPACE \"Fast\" DEFERRABLE INITIALLY DEFERRED,\n    CONSTRAINT t_b_key UNIQUE (b) WITH (fillfactor=80),\n    CONSTRAINT t_excl EXCLUDE USING gist (b WITH =) WITH (fillfactor='60') USING INDEX TABLESPACE fast WHERE ((b > 0))\n);\n"
        );
    }

    #[test]
    fn a_table_sets_its_sequences_persistence_and_comments_and_comments_on_inherited_constraints() {
        let t = Table {
            schema: "app".into(),
            name: "t_1".into(),
            parent: Parent::Partition { schema: "app".into(), name: "t".into(), bound: "FOR VALUES IN (1)".into() },
            constraints: vec![Constraint {
                name: "t_chk".into(),
                definition: "CHECK ((k > 0))".into(),
                comment: Some("from the parent".into()),
                inherited: true,
                ..Default::default()
            }],
            sequence_states: vec![
                SequenceState {
                    schema: "app".into(),
                    name: "t_1_id_seq".into(),
                    unlogged: Some(true),
                    comment: Some("ids".into()),
                },
                SequenceState { schema: "app".into(), name: "Odd seq".into(), unlogged: Some(false), comment: None },
                SequenceState {
                    schema: "app".into(),
                    name: "noted".into(),
                    unlogged: None,
                    comment: Some("it's".into()),
                },
            ],
            ..Default::default()
        };
        assert_eq!(
            table(&t).text,
            "CREATE TABLE app.t_1 PARTITION OF app.t\n    FOR VALUES IN (1);\n\nALTER SEQUENCE app.t_1_id_seq SET UNLOGGED;\n\nALTER SEQUENCE app.\"Odd seq\" SET LOGGED;\n\nCOMMENT ON CONSTRAINT t_chk ON app.t_1 IS 'from the parent';\n\nCOMMENT ON SEQUENCE app.t_1_id_seq IS 'ids';\n\nCOMMENT ON SEQUENCE app.noted IS 'it''s';\n"
        );
    }

    #[test]
    fn an_index_a_constraint_creates_scripts_as_the_constraint() {
        let pkey = Index { name: "orders_pkey".into(), comment: Some("pk".into()), ..Default::default() };
        let constraint = Constraint {
            name: "orders_pkey".into(),
            definition: "PRIMARY KEY (id)".into(),
            comment: Some("the key".into()),
            ..Default::default()
        };
        assert_eq!(
            index_script(&pkey, Some(&constraint), None, "TABLE", "public", "public.orders").text,
            "ALTER TABLE public.orders ADD CONSTRAINT orders_pkey PRIMARY KEY (id);\n\nCOMMENT ON INDEX public.orders_pkey IS 'pk';\n\nCOMMENT ON CONSTRAINT orders_pkey ON public.orders IS 'the key';\n"
        );
        let plain = Index {
            name: "i".into(),
            definition: Some("CREATE INDEX i ON ONLY public.t USING btree (a)".into()),
            tablespace: Some("fast".into()),
            ..Default::default()
        };
        assert_eq!(
            index_script(&plain, None, None, "TABLE", "public", "public.t").text,
            "CREATE INDEX i ON public.t USING btree (a);\n\nALTER INDEX public.i SET TABLESPACE fast;\n"
        );
        let copy = Index {
            name: "t_1_a_idx".into(),
            definition: Some("CREATE INDEX t_1_a_idx ON public.t_1 USING btree (a)".into()),
            ..Default::default()
        };
        assert_eq!(
            index_script(&copy, None, Some("public.t_a"), "TABLE", "public", "public.t_1").text,
            "CREATE INDEX t_1_a_idx ON public.t_1 USING btree (a);\n\nALTER INDEX public.t_a ATTACH PARTITION public.t_1_a_idx;\n"
        );
    }

    #[test]
    fn an_index_keeps_its_column_statistics_and_warns_when_invalid_or_partly_attached() {
        let expr = Index {
            name: "Lower".into(),
            definition: Some("CREATE INDEX \"Lower\" ON app.t USING btree (lower(a))".into()),
            statistics: vec![ColumnStatistics { column: 1, target: 300 }],
            ..Default::default()
        };
        assert_eq!(index_script(&expr, None, None, "TABLE", "app", "app.t"), Ddl::complete("CREATE INDEX \"Lower\" ON app.t USING btree (lower(a));\n\nALTER INDEX app.\"Lower\" ALTER COLUMN 1 SET STATISTICS 300;\n".into()));
        let broken = Index {
            name: "dup".into(),
            definition: Some("CREATE UNIQUE INDEX dup ON app.t USING btree (a)".into()),
            invalid: true,
            ..Default::default()
        };
        assert_eq!(
            index_script(&broken, None, None, "TABLE", "app", "app.t").warnings,
            ["the invalid state of index app.dup, which a replay builds valid"]
        );
        let only = Index {
            name: "p_k".into(),
            definition: Some("CREATE INDEX p_k ON ONLY app.p USING btree (k)".into()),
            invalid: true,
            partitioned: true,
            ..Default::default()
        };
        let t = Table {
            schema: "app".into(),
            name: "p".into(),
            columns: vec![col("k", "integer")],
            partition_key: Some("LIST (k)".into()),
            indexes: vec![only],
            ..Default::default()
        };
        assert_eq!(
            table(&t).warnings,
            ["the partial attachment of index app.p_k, which a replay attaches to every partition"]
        );
    }

    #[test]
    fn publication_membership_warns_naming_each_publication() {
        let t = |publications: &[&str]| Table {
            schema: "app".into(),
            name: "t".into(),
            columns: vec![col("id", "integer")],
            unscripted: Unscripted {
                publications: publications.iter().map(|p| p.to_string()).collect(),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(table(&t(&["pub"])).warnings, ["membership of app.t in publication pub"]);
        assert_eq!(table(&t(&["a", "B b"])).warnings, ["membership of app.t in publications a, \"B b\""]);
        let through_schema = Table {
            unscripted: Unscripted { schema_publications: vec!["all app".into()], ..t(&["pub"]).unscripted },
            ..t(&[])
        };
        assert_eq!(
            table(&through_schema).warnings,
            [
                "membership of app.t in publication pub",
                "membership of app.t in publication \"all app\" through schema app"
            ]
        );
    }

    #[test]
    fn only_a_top_level_on_only_is_dropped() {
        assert_eq!(
            index(r#"CREATE INDEX "x ON ONLY y" ON ONLY public.t USING btree (a)"#),
            r#"CREATE INDEX "x ON ONLY y" ON public.t USING btree (a)"#
        );
        assert_eq!(index("CREATE INDEX i ON public.t USING btree (a)"), "CREATE INDEX i ON public.t USING btree (a)");
    }

    fn bare() -> ScriptedObject {
        ScriptedObject {
            schema: "lab".into(),
            name: "mood".into(),
            comment: None,
            privileges: vec![],
            owner: Owner::default(),
            security_label: false,
        }
    }

    #[test]
    fn a_standalone_sequence_writes_nondefault_options_then_owned_by_and_warns() {
        let sequence = Sequence {
            schema: "public".into(),
            name: "order_number".into(),
            column: String::new(),
            type_name: "bigint".into(),
            start: 1000,
            increment: 10,
            min: 1,
            max: i64::MAX,
            cache: 1,
            cycle: false,
        };
        let owned = StandaloneSequence {
            sequence: sequence.clone(),
            unlogged: true,
            owned_schema: "public".into(),
            owned_table: "orders".into(),
            owned_column: "id".into(),
            comment: Some("orders".into()),
            privileges: vec![grant("USAGE", None, "app", Change::Granted)],
            owner: Owner { name: "bob".into(), connecting_role: false },
            security_label: true,
        };
        let ddl = standalone_sequence(&owned);
        assert_eq!(
            ddl.text,
            "CREATE SEQUENCE public.order_number AS bigint START WITH 1000 INCREMENT BY 10;\n\nALTER SEQUENCE public.order_number OWNED BY public.orders.id;\n\nALTER SEQUENCE public.order_number SET UNLOGGED;\n\nCOMMENT ON SEQUENCE public.order_number IS 'orders';\n"
        );
        assert_eq!(
            ddl.warnings,
            [
                "owner bob of public.order_number",
                "privileges on public.order_number (USAGE granted to app)",
                "security labels on public.order_number"
            ]
        );
        let plain = StandaloneSequence {
            unlogged: false,
            owned_column: String::new(),
            comment: None,
            privileges: vec![],
            owner: Owner::default(),
            security_label: false,
            ..owned
        };
        assert_eq!(
            standalone_sequence(&plain).text,
            "CREATE SEQUENCE public.order_number AS bigint START WITH 1000 INCREMENT BY 10;\n"
        );
        assert!(standalone_sequence(&plain).warnings.is_empty());
    }

    #[test]
    fn types_and_extensions_script_as_create() {
        assert_eq!(
            enum_type(&bare(), &["sad".into(), "ok".into(), "happy".into()]).text,
            "CREATE TYPE lab.mood AS ENUM ('sad', 'ok', 'happy');\n"
        );
        let domain = ScriptedObject { name: "posint".into(), ..bare() };
        assert_eq!(
            domain_type(
                &domain,
                "integer",
                None,
                None,
                false,
                &[("posint_positive".into(), "CHECK ((VALUE > 0))".into())]
            )
            .text,
            "CREATE DOMAIN lab.posint AS integer\n    CONSTRAINT posint_positive CHECK ((VALUE > 0));\n"
        );
        let pet = ScriptedObject { name: "pet".into(), ..bare() };
        assert_eq!(
            composite_type(
                &pet,
                &[
                    CompositeField { name: "id".into(), type_name: "integer".into(), collation: None },
                    CompositeField { name: "name".into(), type_name: "text".into(), collation: Some("C".into()) }
                ]
            )
            .text,
            "CREATE TYPE lab.pet AS (\n    id integer,\n    name text COLLATE \"C\"\n);\n"
        );
        let span = ScriptedObject { name: "span".into(), ..bare() };
        assert_eq!(
            range_type(
                &span,
                &RangeScript {
                    subtype: "integer".into(),
                    opclass: None,
                    collation: None,
                    canonical: None,
                    subtype_diff: None
                }
            )
            .text,
            "CREATE TYPE lab.span AS RANGE (\n    SUBTYPE = integer\n);\n"
        );
        assert_eq!(
            extension_script("plpgsql", "pg_catalog", "1.0", None, &Owner::default(), false).text,
            "CREATE EXTENSION plpgsql WITH SCHEMA pg_catalog VERSION '1.0';\n"
        );
    }
}
