//! SQL Server staged edits. `docs/architecture.md` ("SQL Server staged grid edits") gives the reasons.
use super::batch::{self, Probed, Reply, Set};
use super::checks::{self, Children};
use super::*;
use crate::sql::mssql_value::{input_expression, input_sql};
use crate::sql::tsql::{quote_ident, quote_literal};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};

pub(crate) const LOCK_TIMEOUT: i32 = 5000;
/// RPC parameters per module, under SQL Server's 2,100.
const PARAMETERS: usize = 2000;
/// `@@TEXTSIZE` reads -1 after `SET TEXTSIZE -1` and on a fresh session, and 2147483647 after some clients' setting.
const UNLIMITED_TEXT: [i64; 2] = [-1, 2_147_483_647];
const IMPLICIT_TRANSACTIONS: u32 = 2;
const DOUBLE_BYTE_CODE_PAGES: [i32; 4] = [932, 936, 949, 950];
const LOCK_TIMED_OUT: u32 = 1222;
const DEADLOCK: u32 = 1205;
const CHANGED_DURING_CHECK: u32 = 50003;

const FENCE: &str = "IF @fence IS NOT NULL AND (@@TRANCOUNT <> 1 OR XACT_STATE() <> 1 OR CURRENT_TRANSACTION_ID() <> @fence) THROW 50001, N'L1 Data edit fence', 1;";

fn module(declare: &str, body: &str, lock_timeout: i32) -> String {
    format!(
        "DECLARE @fence bigint = @P1;{declare}
SET XACT_ABORT ON; SET NOCOUNT ON; SET ANSI_WARNINGS ON; SET ARITHABORT ON; SET NUMERIC_ROUNDABORT OFF; SET CONCAT_NULL_YIELDS_NULL ON;
SET ROWCOUNT 0; SET TEXTSIZE -1; SET LOCK_TIMEOUT {lock_timeout}; SET TRANSACTION ISOLATION LEVEL READ COMMITTED;
BEGIN TRY
{FENCE}
{body}
END TRY
BEGIN CATCH
{}
END CATCH",
        batch::fenced_catch("@fence")
    )
}

/// P2. On the metadata session, with no fence, it takes no locks, which would end with the statement anyway.
fn check_module(lock_timeout: i32) -> String {
    let violations = checks::violations("l.object_id", Children::Constraints);
    module(
        " DECLARE @target int = @P2, @name nvarchar(max) = @P3;",
        &format!(
            r#"DECLARE @gate TABLE (reason sysname);
IF ISNULL(HAS_PERMS_BY_NAME(DB_NAME(), 'DATABASE', 'VIEW DEFINITION'), 0) <> 1 INSERT @gate VALUES (N'VIEW DEFINITION');
IF ISNULL(IS_SRVROLEMEMBER('sysadmin'), 0) <> 1 AND USER_NAME() <> N'dbo'
BEGIN
    IF ISNULL(HAS_PERMS_BY_NAME(DB_NAME(), 'DATABASE', 'VIEW SECURITY DEFINITION'), 0) <> 1 INSERT @gate VALUES (N'VIEW SECURITY DEFINITION');
    IF EXISTS (SELECT 1 FROM sys.database_permissions WHERE state = 'D' AND permission_name IN ('VIEW DEFINITION', 'CONTROL')) INSERT @gate VALUES (N'deny');
END;
IF ISNULL(OBJECT_ID(@name, N'U'), 0) <> @target INSERT @gate VALUES (N'identity');
SELECT reason AS [l1:gate] FROM @gate;
IF EXISTS (SELECT 1 FROM @gate) RETURN;
DECLARE @locked TABLE (object_id int PRIMARY KEY, depth int NOT NULL);
DECLARE @wave TABLE (object_id int PRIMARY KEY);
DECLARE @id int, @depth int = 0, @lock nvarchar(max);
INSERT @wave VALUES (@target);
WHILE EXISTS (SELECT 1 FROM @wave) AND (SELECT COUNT(*) FROM @locked) < 64
BEGIN
    SELECT @id = MIN(object_id) FROM @wave;
    WHILE @id IS NOT NULL
    BEGIN
        SET @lock = N'DECLARE @x int; SELECT TOP (0) @x = 1 FROM ' + QUOTENAME(OBJECT_SCHEMA_NAME(@id)) + N'.' + QUOTENAME(OBJECT_NAME(@id)) + N' WITH (UPDLOCK, HOLDLOCK)';
        IF @lock IS NULL THROW {CHANGED_DURING_CHECK}, N'L1 Data edit: a table was dropped during the check', 1;
        IF @fence IS NOT NULL EXEC sys.sp_executesql @lock;
        IF @id = @target AND ISNULL(OBJECT_ID(@name, N'U'), 0) <> @target THROW {CHANGED_DURING_CHECK}, N'L1 Data edit: the table changed during the check', 1;
        INSERT @locked VALUES (@id, @depth);
        SELECT @id = MIN(object_id) FROM @wave WHERE object_id > @id;
    END;
    DELETE @wave;
    SET @depth += 1;
    INSERT @wave SELECT DISTINCT fk.parent_object_id FROM sys.foreign_keys fk JOIN @locked l ON l.object_id = fk.referenced_object_id
    WHERE fk.is_disabled = 0 AND (fk.delete_referential_action <> 0 OR fk.update_referential_action <> 0)
        AND NOT EXISTS (SELECT 1 FROM @locked x WHERE x.object_id = fk.parent_object_id);
END;
SELECT l.object_id AS [l1:closure], OBJECT_SCHEMA_NAME(l.object_id), OBJECT_NAME(l.object_id) FROM @locked l ORDER BY l.depth, l.object_id;
SELECT COUNT(*) AS [l1:unlocked] FROM @wave;
SELECT fk.name AS [l1:edge], fk.parent_object_id, fk.referenced_object_id, fk.delete_referential_action, fk.update_referential_action,
    (SELECT fc.referenced_column_id AS c FROM sys.foreign_key_columns fc WHERE fc.constraint_object_id = fk.object_id ORDER BY fc.constraint_column_id FOR JSON PATH)
FROM sys.foreign_keys fk JOIN @locked p ON p.object_id = fk.parent_object_id JOIN @locked r ON r.object_id = fk.referenced_object_id
WHERE fk.is_disabled = 0 AND (fk.delete_referential_action <> 0 OR fk.update_referential_action <> 0) ORDER BY fk.object_id;
SELECT l.object_id AS [l1:violation], v.reason, v.detail FROM @locked l CROSS APPLY ({violations}) AS v(reason, detail);
SELECT c.column_id AS [l1:column], c.name, COALESCE(t.name, declared.name, N'unknown'), c.max_length, c.precision, c.scale,
    c.is_identity, c.is_computed, c.generated_always_type, c.is_column_set, c.is_masked,
    CASE WHEN EXISTS (SELECT 1 FROM sys.indexes i JOIN sys.index_columns ic ON ic.object_id = i.object_id AND ic.index_id = i.index_id
        WHERE i.object_id = c.object_id AND i.is_primary_key = 1 AND ic.column_id = c.column_id) THEN 1 ELSE 0 END,
    c.collation_name, CONVERT(int, COLLATIONPROPERTY(c.collation_name, 'CodePage')),
    (SELECT d.referenced_minor_id AS c FROM sys.sql_expression_dependencies d WHERE d.referencing_id = @target
        AND d.referencing_minor_id = c.column_id AND d.referenced_id = @target AND d.referenced_minor_id <> 0 FOR JSON PATH)
FROM sys.columns c LEFT JOIN sys.types declared ON declared.user_type_id = c.user_type_id
LEFT JOIN sys.types t ON t.user_type_id = c.system_type_id AND t.system_type_id = t.user_type_id
WHERE c.object_id = @target ORDER BY c.column_id;"#
        ),
        lock_timeout,
    )
}

/// A column as P2 reads it. Admission and the in-transaction check must read every field alike.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub(crate) struct ShapeColumn {
    id: i32,
    pub name: String,
    type_name: String,
    max_length: i32,
    precision: u8,
    scale: u8,
    identity: bool,
    computed: bool,
    generated: bool,
    column_set: bool,
    masked: bool,
    pk: bool,
    collation: Option<String>,
    code_page: Option<i32>,
    depends: Vec<i32>,
}

impl ShapeColumn {
    pub fn type_display(&self) -> String {
        super::catalog::display(&self.type_name, self.max_length, self.precision, self.scale)
    }
    fn read_only(&self) -> Option<ReadOnlyReason> {
        Some(if self.identity {
            ReadOnlyReason::Identity
        } else if self.computed {
            ReadOnlyReason::Computed
        } else if self.type_name == "timestamp" {
            ReadOnlyReason::Rowversion
        } else if self.generated {
            ReadOnlyReason::GeneratedAlways
        } else if self.column_set {
            ReadOnlyReason::ColumnSet
        } else if self.masked {
            ReadOnlyReason::Masked
        } else if self.type_name == "sql_variant" {
            ReadOnlyReason::SqlVariant
        } else if matches!(self.type_name.as_str(), "text" | "ntext" | "image") {
            ReadOnlyReason::LegacyLob
        } else {
            return None;
        })
    }
    fn writable_type(&self) -> bool {
        matches!(
            self.type_name.as_str(),
            "tinyint"
                | "smallint"
                | "int"
                | "bigint"
                | "bit"
                | "decimal"
                | "numeric"
                | "money"
                | "smallmoney"
                | "float"
                | "real"
                | "date"
                | "time"
                | "smalldatetime"
                | "datetime"
                | "datetime2"
                | "datetimeoffset"
                | "uniqueidentifier"
                | "char"
                | "varchar"
                | "nchar"
                | "nvarchar"
                | "binary"
                | "varbinary"
                | "xml"
        )
    }
    fn double_byte(&self) -> bool {
        matches!(self.type_name.as_str(), "char" | "varchar")
            && self.code_page.is_some_and(|p| DOUBLE_BYTE_CODE_PAGES.contains(&p))
    }
}

#[derive(Debug, Clone)]
struct Table {
    id: i32,
    name: String,
}

#[derive(Debug, Clone)]
struct Edge {
    name: String,
    parent: i32,
    referenced: i32,
    deletes: bool,
    updates: bool,
    columns: Vec<i32>,
}

/// What P2 found, once its gate passed.
#[derive(Debug, Clone)]
pub(crate) struct EditCheck {
    target: i32,
    tables: Vec<Table>,
    unlocked: u32,
    edges: Vec<Edge>,
    violations: Vec<(i32, EditRefusal)>,
    pub columns: Vec<ShapeColumn>,
}

pub(crate) enum Answer {
    Checked(EditCheck),
    Refused(EditRefusal),
    /// `OBJECT_ID` of the name no longer gives the tab's relation.
    Changed,
}

#[derive(serde::Deserialize)]
struct Id {
    c: i32,
}

fn ids(cell: Option<&Option<String>>) -> Result<Vec<i32>, QueryError> {
    match cell.and_then(Option::as_deref) {
        None => Ok(Vec::new()),
        Some(json) => Ok(serde_json::from_str::<Vec<Id>>(json)
            .map_err(|e| error(format!("Invalid SQL Server column list: {e}")))?
            .into_iter()
            .map(|id| id.c)
            .collect()),
    }
}

fn flag(row: &[Option<String>], i: usize) -> Result<bool, QueryError> {
    Ok(number::<i32>(row, i)? != 0)
}

fn violation(reason: &str, detail: &str, table: &str) -> Result<EditRefusal, QueryError> {
    let table = table.to_owned();
    let detail = detail.to_owned();
    let column = |issue| Ok(EditRefusal::ColumnType { table: table.clone(), column: detail.clone(), issue });
    match reason {
        "kind" => Ok(EditRefusal::TableKind {
            table_kind: match detail.as_str() {
                "memory-optimized" => TableKind::MemoryOptimized,
                "temporal" => TableKind::Temporal,
                "graph" => TableKind::Graph,
                "ledger" => TableKind::Ledger,
                "FileTable" => TableKind::FileTable,
                "Stretch" => TableKind::Stretch,
                "external" => TableKind::External,
                _ => TableKind::Replicated,
            },
            table,
        }),
        "trigger" => Ok(EditRefusal::Trigger { table, trigger: detail }),
        "policy" => Ok(EditRefusal::SecurityPolicy { table, policy: detail }),
        "ignore_dup_key" => Ok(EditRefusal::IgnoreDupKey { table, index: detail }),
        "indexed_view" => Ok(EditRefusal::IndexedView { table, view: detail }),
        "function" => Ok(EditRefusal::Function { table, object: detail }),
        "permission" => Ok(EditRefusal::Permission { missing: format!("VIEW DEFINITION on {table}") }),
        "clr" => column(ColumnTypeIssue::Clr),
        "alias" => column(ColumnTypeIssue::Alias),
        "encrypted" => column(ColumnTypeIssue::Encrypted),
        "bound_rule" => column(ColumnTypeIssue::BoundRule),
        "bound_default" => column(ColumnTypeIssue::BoundDefault),
        other => Err(error(format!("Unknown SQL Server edit check {other}"))),
    }
}

fn parse(reply: Reply) -> Result<Answer, QueryError> {
    let gate = reply
        .sets
        .first()
        .filter(|set| set.tag == "l1:gate")
        .ok_or_else(|| batch::unexpected(&reply.sets, &["l1:gate"]))?;
    if let Some(row) = gate.rows.first() {
        if reply.sets.len() != 1 {
            return Err(batch::unexpected(&reply.sets, &["l1:gate"]));
        }
        return Ok(match required(row, 0)? {
            "identity" => Answer::Changed,
            "deny" => Answer::Refused(EditRefusal::HiddenMetadata),
            missing => Answer::Refused(EditRefusal::Permission { missing: missing.to_owned() }),
        });
    }
    let sets = reply.expect(&["l1:gate", "l1:closure", "l1:unlocked", "l1:edge", "l1:violation", "l1:column"])?;
    let [_, closure, unlocked, edges, violations, columns] = <[Set; 6]>::try_from(sets).ok().expect("six sets");
    let tables = closure
        .rows
        .iter()
        .map(|r| Ok(Table { id: number(r, 0)?, name: format!("{}.{}", required(r, 1)?, required(r, 2)?) }))
        .collect::<Result<Vec<_>, QueryError>>()?;
    let target = tables.first().ok_or_else(|| error("SQL Server edit check locked no table"))?.id;
    let name = |id: i32| tables.iter().find(|t| t.id == id).map(|t| t.name.clone()).unwrap_or_else(|| id.to_string());
    let violations = violations
        .rows
        .iter()
        .map(|r| {
            let id = number(r, 0)?;
            Ok((id, violation(required(r, 1)?, r.get(2).cloned().flatten().as_deref().unwrap_or(""), &name(id))?))
        })
        .collect::<Result<Vec<_>, QueryError>>()?;
    Ok(Answer::Checked(EditCheck {
        target,
        unlocked: number(unlocked.rows.first().ok_or_else(|| error("Missing SQL Server closure count"))?, 0)?,
        edges: edges
            .rows
            .iter()
            .map(|r| {
                Ok(Edge {
                    name: required(r, 0)?.to_owned(),
                    parent: number(r, 1)?,
                    referenced: number(r, 2)?,
                    deletes: flag(r, 3)?,
                    updates: flag(r, 4)?,
                    columns: ids(r.get(5))?,
                })
            })
            .collect::<Result<_, QueryError>>()?,
        violations,
        columns: columns
            .rows
            .iter()
            .map(|r| {
                Ok(ShapeColumn {
                    id: number(r, 0)?,
                    name: required(r, 1)?.to_owned(),
                    type_name: required(r, 2)?.to_owned(),
                    max_length: number(r, 3)?,
                    precision: number(r, 4)?,
                    scale: number(r, 5)?,
                    identity: flag(r, 6)?,
                    computed: flag(r, 7)?,
                    generated: flag(r, 8)?,
                    column_set: flag(r, 9)?,
                    masked: flag(r, 10)?,
                    pk: flag(r, 11)?,
                    collation: r.get(12).cloned().flatten(),
                    code_page: r
                        .get(13)
                        .cloned()
                        .flatten()
                        .map(|p| p.parse())
                        .transpose()
                        .map_err(|_| error("Invalid code page"))?,
                    depends: ids(r.get(14))?,
                })
            })
            .collect::<Result<_, QueryError>>()?,
        tables,
    }))
}

impl EditCheck {
    fn failing(&self, table: i32) -> Option<&EditRefusal> {
        self.violations.iter().find(|(id, _)| *id == table).map(|(_, reason)| reason)
    }
    fn name(&self, table: i32) -> String {
        self.tables.iter().find(|t| t.id == table).map_or_else(|| table.to_string(), |t| t.name.clone())
    }
    /// `start` and every table reachable from it through edges `follow` accepts.
    fn reach(&self, start: i32, follow: impl Fn(&Edge) -> bool) -> Vec<i32> {
        let mut seen = vec![start];
        let mut queue = VecDeque::from([start]);
        while let Some(table) = queue.pop_front() {
            for edge in self.edges.iter().filter(|e| e.referenced == table && follow(e)) {
                if !seen.contains(&edge.parent) {
                    seen.push(edge.parent);
                    queue.push_back(edge.parent);
                }
            }
        }
        seen
    }
    /// The first table `edge`'s action reaches, through edges `follow` accepts after it, that fails a check: the
    /// edge's name, that table, and its refusal.
    fn cascade(&self, edge: &Edge, follow: impl Fn(&Edge) -> bool) -> Option<(String, String, Box<EditRefusal>)> {
        self.reach(edge.parent, follow).into_iter().find_map(|table| {
            self.failing(table).map(|cause| (edge.name.clone(), self.name(table), Box::new(cause.clone())))
        })
    }
    /// A delete runs the target's delete actions, and a `SET NULL` or `SET DEFAULT` one updates the child, so every
    /// action edge after the first counts.
    fn delete_refusal(&self) -> Option<EditRefusal> {
        let (foreign_key, table, cause) = self
            .edges
            .iter()
            .filter(|e| e.referenced == self.target && e.deletes)
            .find_map(|e| self.cascade(e, |_| true))?;
        Some(EditRefusal::Cascade { foreign_key, table, cause })
    }
    /// An update action runs when an edge's referenced columns change: the column itself, or a computed column
    /// built from it.
    fn column_refusal(&self, column: &ShapeColumn) -> Option<ReadOnlyReason> {
        let derived: Vec<i32> = self.columns.iter().filter(|c| c.depends.contains(&column.id)).map(|c| c.id).collect();
        self.edges
            .iter()
            .filter(|e| {
                e.referenced == self.target
                    && e.updates
                    && e.columns.iter().any(|c| *c == column.id || derived.contains(c))
            })
            .find_map(|e| self.cascade(e, |next| next.updates))
            .map(|(foreign_key, table, cause)| ReadOnlyReason::Cascade { foreign_key, table, cause })
    }
    pub fn support(&self) -> EditSupport {
        match self.decide() {
            Ok(support) => support,
            Err(reason) => EditSupport::Refused { reason },
        }
    }
    fn decide(&self) -> Result<EditSupport, EditRefusal> {
        if self.unlocked > 0 {
            return Err(EditRefusal::CascadeTooWide);
        }
        if let Some(reason) = self.failing(self.target) {
            return Err(reason.clone());
        }
        let table = self.name(self.target);
        if let Some(c) = self.columns.iter().find(|c| c.read_only().is_none() && !c.writable_type()) {
            return Err(EditRefusal::ColumnType {
                table,
                column: c.name.clone(),
                issue: ColumnTypeIssue::Unsupported { type_name: c.type_name.clone() },
            });
        }
        let pk: Vec<&ShapeColumn> = self.columns.iter().filter(|c| c.pk).collect();
        if pk.is_empty() {
            return Err(EditRefusal::NoPrimaryKey);
        }
        if let Some(c) = pk.iter().find(|c| c.masked) {
            return Err(EditRefusal::MaskedKey { column: c.name.clone() });
        }
        let row_check = match self.columns.iter().find(|c| c.type_name == "timestamp") {
            Some(c) => RowCheck::Rowversion { column: c.name.clone() },
            None => RowCheck::Values,
        };
        let columns = self
            .columns
            .iter()
            .map(|c| ColumnEdit { name: c.name.clone(), read_only: c.read_only().or_else(|| self.column_refusal(c)) })
            .collect();
        Ok(EditSupport::Editable { row_check, columns, delete: self.delete_refusal() })
    }
}

fn check_params(target: i32, name: &str) -> [Param; 2] {
    [Param::BigInt(Some(i64::from(target))), Param::Text(Some(name.to_owned()))]
}

/// One parameter of a module. `float` and `real` travel typed, because SQL Server's text conversion of them is
/// not exact.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub(crate) enum Param {
    Text(Option<String>),
    Float(Option<f64>),
    Real(Option<f32>),
    BigInt(Option<i64>),
}

impl tiberius::ToSql for Param {
    fn to_sql(&self) -> tiberius::ColumnData<'_> {
        match self {
            Param::Text(v) => tiberius::ColumnData::String(v.as_deref().map(std::borrow::Cow::Borrowed)),
            Param::Float(v) => tiberius::ColumnData::F64(*v),
            Param::Real(v) => tiberius::ColumnData::F32(*v),
            Param::BigInt(v) => tiberius::ColumnData::I64(*v),
        }
    }
}

#[derive(Debug, serde::Serialize)]
struct Read {
    sql: String,
    params: Vec<Param>,
    /// Per ordinal, from 1: the change, and each selected cell that must still read as the page showed it.
    rows: Vec<(ChangeRef, Vec<Option<Option<String>>>)>,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct Write {
    pub change: ChangeRef,
    sql: String,
    params: Vec<Param>,
    /// The statement as it reads with each parameter written as a literal, for the Output log and the Production
    /// question.
    pub display: String,
}

/// Everything a Submit sends, built once from the request and the admission check. The Production digest binds it
/// whole.
#[derive(Debug, serde::Serialize)]
pub(crate) struct EditPlan {
    target: i32,
    name: String,
    shape: Vec<ShapeColumn>,
    row_check: RowCheck,
    deletes: bool,
    written: Vec<String>,
    check: String,
    reads: Vec<Read>,
    pub writes: Vec<Write>,
    #[serde(skip)]
    #[cfg(test)]
    pause: Option<std::sync::Arc<(tokio::sync::Notify, tokio::sync::Notify)>>,
}

struct Params {
    values: Vec<Param>,
}

impl Params {
    fn new() -> Self {
        Self { values: Vec::new() }
    }
    fn add(&mut self, param: Param) -> String {
        self.values.push(param);
        format!("@P{}", self.values.len() + 1)
    }
}

fn collation(column: &ShapeColumn) -> Result<&str, String> {
    column
        .collation
        .as_deref()
        .filter(|c| !c.is_empty() && c.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
        .ok_or_else(|| format!("Column {} has no usable collation", column.name))
}

/// The expression a module sends for `value` in `column`, and the same expression with the value as a literal.
fn cell(
    column: &ShapeColumn,
    value: &Option<String>,
    key: bool,
    params: &mut Params,
) -> Result<(String, String), String> {
    let native = column.type_display();
    let base = column.type_name.as_str();
    let Some(text) = value else {
        let exec = match base {
            "float" => params.add(Param::Float(None)),
            "real" => params.add(Param::Real(None)),
            _ => input_expression(&native, &params.add(Param::Text(None)))?,
        };
        return Ok((exec, "NULL".into()));
    };
    let display = input_sql(&native, text).map_err(|e| format!("{}: {e}", column.name))?;
    match base {
        "float" | "real" => {
            let parsed: f64 = text.trim().parse().map_err(|_| format!("{}: {text} is not a number", column.name))?;
            let param = if base == "real" {
                let real: f32 = text.trim().parse().map_err(|_| format!("{}: {text} is not a number", column.name))?;
                if !real.is_finite() {
                    return Err(format!("{}: {text} is out of range for real", column.name));
                }
                Param::Real(Some(real))
            } else {
                if !parsed.is_finite() {
                    return Err(format!("{}: {text} is out of range for float", column.name));
                }
                Param::Float(Some(parsed))
            };
            Ok((params.add(param), display))
        }
        "char" | "varchar" if key => {
            let collation = collation(column)?;
            let param = params.add(Param::Text(Some(text.clone())));
            Ok((
                format!("CONVERT({native}, {param} COLLATE {collation})"),
                format!("CONVERT({native}, {} COLLATE {collation})", quote_literal(text)),
            ))
        }
        "char" | "varchar" if !text.is_ascii() => Err(format!(
            "{}: SQL Server char and varchar edits accept ASCII only; use nchar or nvarchar for Unicode",
            column.name
        )),
        _ => {
            let param = params.add(Param::Text(Some(text.clone())));
            Ok((input_expression(&native, &param)?, display))
        }
    }
}

fn failed(change: ChangeRef, message: impl Into<String>) -> ApplyError {
    ApplyError::Failed { change, error: error(message) }
}

fn write_module(statement: &str, lock_timeout: i32) -> String {
    module(
        " DECLARE @rows int;",
        &format!(
            "{statement};\nSELECT @rows = @@ROWCOUNT;\nIF @@TRANCOUNT <> 1 OR XACT_STATE() <> 1 OR CURRENT_TRANSACTION_ID() <> @fence THROW 50002, N'L1 Data edit fence', 1;\nSELECT @rows AS [l1:count];"
        ),
        lock_timeout,
    )
}

struct Planner<'a> {
    check: &'a EditCheck,
    qualified: String,
    lock_timeout: i32,
}

impl Planner<'_> {
    fn column(&self, name: &str) -> Option<&ShapeColumn> {
        self.check.columns.iter().find(|c| c.name == name)
    }
    fn key_columns(&self) -> Vec<&ShapeColumn> {
        self.check.columns.iter().filter(|c| c.pk).collect()
    }
    /// The key cells in primary key order, or `None` unless they name the whole key once.
    fn key<'k>(&self, key: &'k [CellWrite]) -> Option<Vec<(&ShapeColumn, &'k Option<String>)>> {
        let columns = self.key_columns();
        if key.len() != columns.len() {
            return None;
        }
        columns.into_iter().map(|c| key.iter().find(|k| k.column == c.name).map(|k| (c, &k.value))).collect()
    }
    fn condition(
        &self,
        key: &[(&ShapeColumn, &Option<String>)],
        params: &mut Params,
    ) -> Result<(String, String), String> {
        let mut exec = Vec::new();
        let mut shown = Vec::new();
        for (column, value) in key {
            let (e, d) = cell(column, value, true, params)?;
            exec.push(format!("{} = {e}", quote_ident(&column.name)));
            shown.push(format!("{} = {d}", quote_ident(&column.name)));
        }
        Ok((exec.join(" AND "), shown.join(" AND ")))
    }
    fn write(
        &self,
        change: ChangeRef,
        build: impl FnOnce(&mut Params) -> Result<(String, String), String>,
    ) -> Result<Write, ApplyError> {
        let mut params = Params::new();
        let (statement, display) = build(&mut params).map_err(|e| failed(change, e))?;
        Ok(Write { change, sql: write_module(&statement, self.lock_timeout), params: params.values, display })
    }
}

/// A row Submit reads before it writes: its change, its key cells in primary key order, and the cells that must
/// still read as the page showed them.
struct Expected<'a> {
    change: ChangeRef,
    key: Vec<(&'a ShapeColumn, &'a Option<String>)>,
    cells: Vec<(&'a ShapeColumn, &'a Option<String>)>,
}

/// Validates `changes` against the admission check and builds every statement the Submit sends. Nothing is sent
/// when this fails.
pub(crate) fn plan(
    changes: &TableChanges,
    check: &EditCheck,
    support: &EditSupport,
    name: &str,
    lock_timeout: i32,
) -> Result<EditPlan, ApplyError> {
    let (row_check, access, delete) = match support {
        EditSupport::Refused { reason } => return Err(ApplyError::Refused { reason: reason.clone() }),
        EditSupport::Editable { row_check, columns, delete } => (row_check, columns, delete),
    };
    if let Some(change) = crate::session::repeated_key(changes) {
        return Err(failed(change, "Another change names the same row"));
    }
    if !changes.deletes.is_empty()
        && let Some(reason) = delete
    {
        return Err(ApplyError::Refused { reason: reason.clone() });
    }
    let planner = Planner {
        check,
        qualified: format!("{}.{}", quote_ident(&changes.schema), quote_ident(&changes.table)),
        lock_timeout,
    };
    let writable = |change: ChangeRef, cells: &[CellWrite]| -> Result<(), ApplyError> {
        let mut seen = HashSet::new();
        for cell in cells {
            let column = access.iter().find(|c| c.name == cell.column);
            match column {
                Some(ColumnEdit { read_only: None, .. }) if seen.insert(&cell.column) => {}
                Some(ColumnEdit { read_only: Some(ReadOnlyReason::Cascade { foreign_key, table, cause }), .. }) => {
                    return Err(ApplyError::Refused {
                        reason: EditRefusal::Cascade {
                            foreign_key: foreign_key.clone(),
                            table: table.clone(),
                            cause: cause.clone(),
                        },
                    });
                }
                _ => return Err(failed(change, format!("Column {} is unknown, repeated, or read-only", cell.column))),
            }
        }
        Ok(())
    };
    let rowversion = match row_check {
        RowCheck::Rowversion { column } => Some(column.as_str()),
        RowCheck::Values => None,
    };
    let unverifiable = |change: ChangeRef, column: &ShapeColumn, value: &Option<String>| {
        (rowversion.is_none() && column.double_byte() && value.as_ref().is_some_and(|v| !v.is_ascii()))
            .then(|| ApplyError::Unverifiable { change, column: column.name.clone() })
    };
    let mut reads: Vec<Expected> = Vec::new();
    let mut targets = Vec::new();
    for (i, d) in changes.deletes.iter().enumerate() {
        let change = ChangeRef { kind: ChangeKind::Delete, index: i as u32 };
        targets.push((change, &d.key, &d.expect, None));
    }
    for (i, u) in changes.updates.iter().enumerate().filter(|(_, u)| !u.set.is_empty()) {
        let change = ChangeRef { kind: ChangeKind::Update, index: i as u32 };
        writable(change, &u.set)?;
        targets.push((change, &u.key, &u.expect, Some(&u.set)));
    }
    for (change, key, expect, set) in &targets {
        let key = planner.key(key).ok_or(ApplyError::NoPrimaryKey)?;
        if let Some(e) = key.iter().find_map(|(c, v)| unverifiable(*change, c, v)) {
            return Err(e);
        }
        let complete = match (rowversion, set) {
            (Some(rv), _) => expect.len() == 1 && expect[0].column == rv,
            (None, None) => expect.is_empty(),
            (None, Some(set)) => {
                expect.len() == set.len()
                    && set.iter().all(|s| expect.iter().filter(|e| e.column == s.column).count() == 1)
            }
        };
        if !complete {
            return Err(failed(*change, "The change does not carry the row as the page showed it"));
        }
        let mut expected = Vec::new();
        for e in expect.iter() {
            let column =
                planner.column(&e.column).ok_or_else(|| failed(*change, format!("Unknown column {}", e.column)))?;
            if let Some(e) = unverifiable(*change, column, &e.value) {
                return Err(e);
            }
            expected.push((column, &e.value));
        }
        reads.push(Expected { change: *change, key, cells: expected });
    }
    for (i, cells) in changes.inserts.iter().enumerate() {
        writable(ChangeRef { kind: ChangeKind::Insert, index: i as u32 }, cells)?;
    }

    let key_columns = planner.key_columns();
    let per_row = key_columns.len();
    let mut chunks: Vec<Read> = Vec::new();
    for group in reads.chunks((PARAMETERS - 1) / per_row) {
        let mut compared: Vec<&ShapeColumn> = Vec::new();
        for row in group {
            for (column, _) in &row.cells {
                if !compared.iter().any(|c| c.id == column.id) {
                    compared.push(column);
                }
            }
        }
        let mut params = Params::new();
        let mut values = Vec::new();
        let mut rows = Vec::new();
        for (ord, Expected { change, key, cells: expected }) in group.iter().enumerate() {
            let mut cells = vec![format!("{}", ord + 1)];
            for (column, value) in key {
                cells.push(cell(column, value, true, &mut params).map_err(|e| failed(*change, e))?.0);
            }
            values.push(format!("({})", cells.join(", ")));
            let mut want: Vec<Option<Option<String>>> = key.iter().map(|(_, v)| Some((*v).clone())).collect();
            want.extend(
                compared.iter().map(|c| expected.iter().find(|(e, _)| e.id == c.id).map(|(_, v)| (*v).clone())),
            );
            rows.push((*change, want));
        }
        let names = (1..=per_row).map(|i| format!("k{i}")).collect::<Vec<_>>();
        let select = key_columns
            .iter()
            .chain(compared.iter())
            .map(|c| format!("t.{}", quote_ident(&c.name)))
            .collect::<Vec<_>>()
            .join(", ");
        let on = key_columns
            .iter()
            .zip(&names)
            .map(|(c, k)| format!("t.{} = k.{k}", quote_ident(&c.name)))
            .collect::<Vec<_>>()
            .join(" AND ");
        let statement = format!(
            "SELECT k.ord AS [l1:row], {select} FROM {} AS t WITH (UPDLOCK, HOLDLOCK, ROWLOCK, FORCESEEK)\nJOIN (VALUES {}) AS k(ord, {}) ON {on};",
            planner.qualified,
            values.join(", "),
            names.join(", ")
        );
        chunks.push(Read { sql: module("", &statement, lock_timeout), params: params.values, rows });
    }

    let mut writes = Vec::new();
    for (i, d) in changes.deletes.iter().enumerate() {
        let change = ChangeRef { kind: ChangeKind::Delete, index: i as u32 };
        let key = planner.key(&d.key).ok_or(ApplyError::NoPrimaryKey)?;
        writes.push(planner.write(change, |params| {
            let (exec, shown) = planner.condition(&key, params)?;
            Ok((
                format!("DELETE {} WHERE {exec}", planner.qualified),
                format!("DELETE {} WHERE {shown}", planner.qualified),
            ))
        })?);
    }
    for (i, u) in changes.updates.iter().enumerate().filter(|(_, u)| !u.set.is_empty()) {
        let change = ChangeRef { kind: ChangeKind::Update, index: i as u32 };
        let key = planner.key(&u.key).ok_or(ApplyError::NoPrimaryKey)?;
        writes.push(planner.write(change, |params| {
            let mut exec = Vec::new();
            let mut shown = Vec::new();
            for c in &u.set {
                let column = planner.column(&c.column).ok_or_else(|| format!("Unknown column {}", c.column))?;
                let (e, d) = cell(column, &c.value, false, params)?;
                exec.push(format!("{} = {e}", quote_ident(&c.column)));
                shown.push(format!("{} = {d}", quote_ident(&c.column)));
            }
            let (key_exec, key_shown) = planner.condition(&key, params)?;
            Ok((
                format!("UPDATE {} SET {} WHERE {key_exec}", planner.qualified, exec.join(", ")),
                format!("UPDATE {} SET {} WHERE {key_shown}", planner.qualified, shown.join(", ")),
            ))
        })?);
    }
    for (i, cells) in changes.inserts.iter().enumerate() {
        let change = ChangeRef { kind: ChangeKind::Insert, index: i as u32 };
        writes.push(planner.write(change, |params| {
            if cells.is_empty() {
                let sql = format!("INSERT {} DEFAULT VALUES", planner.qualified);
                return Ok((sql.clone(), sql));
            }
            let names = cells.iter().map(|c| quote_ident(&c.column)).collect::<Vec<_>>().join(", ");
            let mut exec = Vec::new();
            let mut shown = Vec::new();
            for c in cells {
                let column = planner.column(&c.column).ok_or_else(|| format!("Unknown column {}", c.column))?;
                let (e, d) = cell(column, &c.value, false, params)?;
                exec.push(e);
                shown.push(d);
            }
            Ok((
                format!("INSERT {} ({names}) VALUES ({})", planner.qualified, exec.join(", ")),
                format!("INSERT {} ({names}) VALUES ({})", planner.qualified, shown.join(", ")),
            ))
        })?);
    }
    let mut written: Vec<String> = changes
        .updates
        .iter()
        .flat_map(|u| u.set.iter())
        .chain(changes.inserts.iter().flatten())
        .map(|c| c.column.clone())
        .collect();
    written.sort();
    written.dedup();
    Ok(EditPlan {
        target: check.target,
        name: name.to_owned(),
        shape: check.columns.clone(),
        row_check: row_check.clone(),
        deletes: !changes.deletes.is_empty(),
        written,
        check: check_module(lock_timeout),
        reads: chunks,
        writes,
        #[cfg(test)]
        pause: None,
    })
}

impl EditPlan {
    /// The in-transaction check's verdict on this plan: `None` when it may go on.
    fn recheck(&self, answer: Answer) -> Option<ApplyError> {
        let check = match answer {
            Answer::Changed => return Some(ApplyError::TableChanged),
            Answer::Refused(reason) => return Some(ApplyError::Refused { reason }),
            Answer::Checked(check) => check,
        };
        if check.columns != self.shape || check.target != self.target {
            return Some(ApplyError::TableChanged);
        }
        match check.support() {
            EditSupport::Refused { reason } => Some(ApplyError::Refused { reason }),
            EditSupport::Editable { row_check, .. } if row_check != self.row_check => Some(ApplyError::TableChanged),
            EditSupport::Editable { delete: Some(reason), .. } if self.deletes => Some(ApplyError::Refused { reason }),
            EditSupport::Editable { columns, .. } => {
                columns.into_iter().filter(|c| self.written.contains(&c.name)).find_map(|c| match c.read_only? {
                    ReadOnlyReason::Cascade { foreign_key, table, cause } => {
                        Some(ApplyError::Refused { reason: EditRefusal::Cascade { foreign_key, table, cause } })
                    }
                    _ => Some(ApplyError::TableChanged),
                })
            }
        }
    }
}

pub(crate) enum EditOutcome {
    Applied,
    /// Nothing was applied, on evidence that the edit's own transaction ended without a commit, or before `BEGIN`.
    RolledBack(ApplyError),
    Unknown(QueryError),
}

pub(crate) struct EditRun {
    pub outcome: EditOutcome,
    /// The row count and time of each write that completed, in `EditPlan::writes` order.
    pub counts: Vec<(u64, u32)>,
}

/// Why the steps after `BEGIN` stopped. `marker` says a fenced `CATCH` rolled the transaction back.
struct Failure {
    error: ApplyError,
    marker: bool,
}

fn server_failure(e: QueryError, marker: bool, change: Option<ChangeRef>) -> Failure {
    let error = match batch::code(&e) {
        Some(LOCK_TIMED_OUT | DEADLOCK) => ApplyError::Locked,
        Some(CHANGED_DURING_CHECK) => ApplyError::TableChanged,
        _ => match change {
            Some(change) => ApplyError::Failed { change, error: e },
            None => ApplyError::Transaction { error: e },
        },
    };
    Failure { error, marker }
}

fn describe(error: &ApplyError) -> String {
    match error {
        ApplyError::Failed { error, .. } | ApplyError::Transaction { error } | ApplyError::OutcomeUnknown { error } => {
            error.message.clone()
        }
        other => format!("{other:?}"),
    }
}

fn unknown(message: impl Into<String>) -> QueryError {
    error(format!(
        "{}. The edits may have been saved. The session was retired; check the saved rows before retrying.",
        message.into()
    ))
}

/// Sends a module with `fence` as `@P1` and `params` from `@P2`.
async fn send(client: &mut TdsClient, sql: &str, fence: Option<i64>, params: &[Param]) -> Result<Reply, QueryError> {
    let fence = Param::BigInt(fence);
    let all: Vec<&dyn tiberius::ToSql> =
        std::iter::once(&fence).chain(params).map(|p| p as &dyn tiberius::ToSql).collect();
    batch::module(client, sql, &all).await
}

const BEGIN: &str = "BEGIN TRANSACTION; DECLARE @c int = @@TRANCOUNT; DECLARE @s int = XACT_STATE(); DECLARE @id bigint = CURRENT_TRANSACTION_ID(); SELECT @c AS [l1:begin], @s, @id FROM (VALUES (1)) AS v(x) WHERE v.x = 1";
const UNDO: &str = "DECLARE @c int = @@TRANCOUNT; DECLARE @id bigint = CURRENT_TRANSACTION_ID(); IF @@TRANCOUNT > 0 ROLLBACK TRANSACTION; DECLARE @c2 int = @@TRANCOUNT; DECLARE @s2 int = XACT_STATE(); SELECT @c AS [l1:undo], @id, @c2, @s2 FROM (VALUES (1)) AS v(x) WHERE v.x = 1";

fn commit_sql(fence: &str) -> String {
    format!(
        "IF @@TRANCOUNT = 1 AND CURRENT_TRANSACTION_ID() = {fence} COMMIT TRANSACTION ELSE THROW 50001, N'L1 Data edit fence', 1; DECLARE @c int = @@TRANCOUNT; DECLARE @s int = XACT_STATE(); SELECT @c AS [l1:commit], @s FROM (VALUES (1)) AS v(x) WHERE v.x = 1"
    )
}

struct Driver<'a> {
    client: &'a mut TdsClient,
    database: &'a str,
    plan: &'a EditPlan,
    stop: &'a AtomicBool,
    counts: &'a mut Vec<(u64, u32)>,
}

impl Driver<'_> {
    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    async fn run(&mut self) -> EditOutcome {
        let probe = match batch::probe(self.client, self.database).await {
            Ok(Probed::State(probe)) => probe,
            Ok(Probed::Shape) => {
                return EditOutcome::RolledBack(ApplyError::Session { reason: SessionRefusal::ResultShape });
            }
            Err(e) if e.severity == "FATAL" => return EditOutcome::Unknown(e),
            Err(e) => return EditOutcome::RolledBack(ApplyError::Transaction { error: e }),
        };
        let refusal = if probe.count != 0 || probe.state != 0 || probe.native != TxnState::Idle {
            Some(SessionRefusal::OpenTransaction)
        } else if probe.options & IMPLICIT_TRANSACTIONS != 0 {
            Some(SessionRefusal::ImplicitTransactions)
        } else if !UNLIMITED_TEXT.contains(&probe.textsize) {
            Some(SessionRefusal::LimitedTextSize)
        } else {
            None
        };
        if let Some(reason) = refusal {
            return EditOutcome::RolledBack(ApplyError::Session { reason });
        }
        if self.stopped() {
            return EditOutcome::RolledBack(ApplyError::Cancelled);
        }
        let fence = match batch::plain(self.client, BEGIN).await.and_then(|r| r.single("l1:begin")) {
            Ok(rows) => match rows.as_slice() {
                [row]
                    if row.first().and_then(Option::as_deref) == Some("1")
                        && row.get(1).and_then(Option::as_deref) == Some("1") =>
                {
                    row.get(2).cloned().flatten()
                }
                _ => None,
            },
            Err(e) if e.severity == "FATAL" => return EditOutcome::Unknown(unknown(e.message)),
            Err(_) => None,
        };
        let Some(fence) = fence else {
            return self
                .undo(
                    None,
                    Failure {
                        error: ApplyError::Transaction {
                            error: error("BEGIN TRANSACTION did not open one transaction"),
                        },
                        marker: false,
                    },
                )
                .await;
        };
        match self.steps(&fence).await {
            Ok(()) => self.commit(&fence).await,
            Err(failure) => match self.undo(Some(&fence), failure).await {
                EditOutcome::RolledBack(_) if self.stopped() => EditOutcome::RolledBack(ApplyError::Cancelled),
                outcome => outcome,
            },
        }
    }

    async fn module(
        &mut self,
        sql: &str,
        fence: &str,
        params: &[Param],
        change: Option<ChangeRef>,
    ) -> Result<Reply, Failure> {
        let fence =
            fence.parse().map_err(|_| server_failure(error("Invalid SQL Server transaction id"), false, change))?;
        match send(self.client, sql, Some(fence), params).await {
            Ok(Reply { rolled_back: Some(e), .. }) => Err(server_failure(e, true, change)),
            Ok(reply) => Ok(reply),
            Err(e) => Err(server_failure(e, false, change)),
        }
    }

    async fn steps(&mut self, fence: &str) -> Result<(), Failure> {
        let fail = |error: ApplyError| Failure { error, marker: false };
        let shape = |e: QueryError| fail(ApplyError::Transaction { error: e });
        if self.stopped() {
            return Err(fail(ApplyError::Cancelled));
        }
        let plan = self.plan;
        let reply = self.module(&plan.check, fence, &check_params(plan.target, &plan.name), None).await?;
        if let Some(error) = plan.recheck(parse(reply).map_err(shape)?) {
            return Err(fail(error));
        }
        #[cfg(test)]
        if let Some(pause) = &plan.pause {
            pause.0.notify_one();
            pause.1.notified().await;
        }
        for read in &plan.reads {
            if self.stopped() {
                return Err(fail(ApplyError::Cancelled));
            }
            let rows = self.module(&read.sql, fence, &read.params, None).await?.single("l1:row").map_err(shape)?;
            let mut found: HashMap<usize, Vec<&Vec<Option<String>>>> = HashMap::new();
            for row in &rows {
                found.entry(number::<usize>(row, 0).map_err(shape)?).or_default().push(row);
            }
            for (i, (change, want)) in read.rows.iter().enumerate() {
                match found.get(&(i + 1)).map(Vec::as_slice) {
                    Some([row]) => {
                        let same = want
                            .iter()
                            .enumerate()
                            .all(|(j, cell)| cell.as_ref().is_none_or(|v| row.get(j + 1) == Some(v)));
                        if !same {
                            return Err(fail(ApplyError::RowChanged { change: *change }));
                        }
                    }
                    other => {
                        return Err(fail(ApplyError::RowMismatch {
                            change: *change,
                            rows: other.map_or(0, |r| r.len() as u64),
                        }));
                    }
                }
            }
        }
        for write in &plan.writes {
            if self.stopped() {
                return Err(fail(ApplyError::Cancelled));
            }
            let started = std::time::Instant::now();
            let rows = self
                .module(&write.sql, fence, &write.params, Some(write.change))
                .await?
                .single("l1:count")
                .map_err(shape)?;
            let count = match rows.as_slice() {
                [row] => number::<u64>(row, 0).map_err(shape)?,
                _ => return Err(shape(error("SQL Server returned no row count"))),
            };
            self.counts.push((count, crate::console::elapsed_ms(started)));
            if count != 1 {
                return Err(fail(ApplyError::RowMismatch { change: write.change, rows: count }));
            }
        }
        if self.stopped() {
            return Err(fail(ApplyError::Cancelled));
        }
        Ok(())
    }

    async fn commit(&mut self, fence: &str) -> EditOutcome {
        match batch::plain(self.client, &commit_sql(fence)).await.and_then(|r| r.single("l1:commit")) {
            Ok(rows)
                if rows == vec![vec![Some("0".to_owned()), Some("0".to_owned())]]
                    && observe(self.client, self.database).is_ok_and(|txn| txn == TxnState::Idle) =>
            {
                EditOutcome::Applied
            }
            Ok(rows) => EditOutcome::Unknown(unknown(format!("COMMIT left the session reading {rows:?}"))),
            Err(e) if e.severity == "FATAL" => {
                EditOutcome::Unknown(unknown(format!("COMMIT was not acknowledged: {}", e.message)))
            }
            Err(e) => {
                self.undo(Some(fence), Failure { error: ApplyError::Transaction { error: e }, marker: false }).await
            }
        }
    }

    /// Rolls back whatever is open and reports Rolled back only on evidence that nothing committed since `BEGIN`.
    async fn undo(&mut self, fence: Option<&str>, failure: Failure) -> EditOutcome {
        let read = batch::plain(self.client, UNDO).await.and_then(|r| r.single("l1:undo"));
        let rows = match read {
            Ok(rows) => rows,
            Err(e) => {
                return EditOutcome::Unknown(unknown(format!(
                    "{}; the rollback could not be verified: {}",
                    describe(&failure.error),
                    e.message
                )));
            }
        };
        let [row] = rows.as_slice() else {
            return EditOutcome::Unknown(unknown(format!(
                "{}; the rollback returned no state",
                describe(&failure.error)
            )));
        };
        let cell = |i: usize| row.get(i).and_then(Option::as_deref);
        let ours = cell(0) == Some("1") && fence.is_some() && cell(1) == fence;
        let evidence = (failure.marker && cell(0) == Some("0")) || ours;
        let idle = cell(2) == Some("0")
            && cell(3) == Some("0")
            && observe(self.client, self.database).is_ok_and(|t| t == TxnState::Idle);
        if evidence && idle {
            EditOutcome::RolledBack(failure.error)
        } else {
            EditOutcome::Unknown(unknown(format!(
                "{}; L1 Data cannot prove the edit's own transaction was the one rolled back",
                describe(&failure.error)
            )))
        }
    }
}

impl MsSession {
    /// On a metadata session a deadlock victim tries again, as the catalog reads do.
    pub(in crate::session) async fn edit_check(&mut self, target: u32, name: &str) -> Result<Answer, ApplyError> {
        let mut attempt = 1;
        loop {
            match self.check_once(target, name).await {
                Err(ApplyError::Locked) if self.metadata && attempt < DEADLOCK_ATTEMPTS => {
                    tokio::time::sleep(DEADLOCK_BACKOFF * attempt).await;
                    attempt += 1;
                }
                result => return result,
            }
        }
    }

    async fn check_once(&mut self, target: u32, name: &str) -> Result<Answer, ApplyError> {
        let database = self.database.clone();
        let Some(client) = self.client.take() else {
            let reason = self.closed_reason().unwrap_or_else(|| "SQL Server session is retired".into());
            return Err(ApplyError::Transaction { error: error(reason) });
        };
        let before_2022 = self.server.version_num < 160000;
        let mut lease = Lease { owner: self, client: Some(client) };
        let reply =
            send(lease.client(), &check_module(LOCK_TIMEOUT), None, &check_params(native_object_id(target), name))
                .await;
        let retire = reply.as_ref().err().filter(|e| e.severity == "FATAL").map(|e| e.message.clone());
        lease.finish(retire, &database);
        match reply.and_then(parse) {
            Err(_) if before_2022 => Err(ApplyError::Refused { reason: EditRefusal::NeedsSqlServer2022 }),
            Err(e) => Err(server_failure(e, false, None).error),
            Ok(answer) => Ok(answer),
        }
    }

    pub(in crate::session) async fn apply_edits(&mut self, plan: &EditPlan, stop: &AtomicBool) -> EditRun {
        let database = self.database.clone();
        let Some(client) = self.client.take() else {
            return EditRun {
                outcome: EditOutcome::Unknown(unknown("The edit's session was unavailable")),
                counts: Vec::new(),
            };
        };
        let mut lease = Lease { owner: self, client: Some(client) };
        let mut counts = Vec::new();
        let outcome = {
            let mut driver = Driver { client: lease.client(), database: &database, plan, stop, counts: &mut counts };
            batch::settle(stop, driver.run(), || {
                EditOutcome::Unknown(unknown("Cancel did not settle within 10 seconds"))
            })
            .await
        };
        let retire = matches!(outcome, EditOutcome::Unknown(_)).then(|| "SQL Server edit outcome unknown".to_owned());
        lease.finish(retire, &database);
        EditRun { outcome, counts }
    }
}

#[cfg(all(test, feature = "mssql-tests"))]
mod tests;
#[cfg(test)]
mod unit;
