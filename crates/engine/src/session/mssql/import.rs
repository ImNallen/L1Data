use super::batch::{self, CLEANUP};
use super::checks::{self, Children};
use super::*;
use crate::sql::tsql::{quote_ident, quote_literal};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::mpsc;

const SETTINGS: &str = "SET ANSI_WARNINGS ON; SET IMPLICIT_TRANSACTIONS OFF; SET XACT_ABORT OFF; SET NOCOUNT OFF";

#[derive(Clone, serde::Serialize)]
pub(crate) struct ImportTargetShape {
    signature: String,
    columns: Vec<(String, String)>,
}

#[derive(serde::Serialize)]
pub(crate) struct InsertPlan {
    target: ImportTarget,
    shape: ImportTargetShape,
    lock: String,
    metadata: String,
    templates: Vec<String>,
    display: String,
    ascii: Vec<bool>,
    #[cfg(test)]
    commit: Option<String>,
    #[cfg(test)]
    restore_suffix: Option<String>,
}

impl InsertPlan {
    pub fn new(target: ImportTarget, shape: ImportTargetShape, columns: &[&str]) -> Result<Self, ImportError> {
        let mut ascii = Vec::new();
        for name in columns {
            let kind = shape
                .columns
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, t)| t.as_str())
                .ok_or(ImportError::TargetChanged)?;
            if !matches!(
                kind,
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
                    | "nvarchar"
                    | "nchar"
                    | "varchar"
                    | "char"
            ) {
                return Err(query(error(format!("CSV import does not support column {name} of type {kind}"))));
            }
            ascii.push(matches!(kind, "varchar" | "char"));
        }
        let qualified = format!("{}.{}", quote_ident(&target.schema), quote_ident(&target.table));
        let names = columns.iter().map(|n| quote_ident(n)).collect::<Vec<_>>().join(",");
        let display = format!(
            "INSERT {qualified} ({names}) VALUES ({})",
            (1..=columns.len()).map(|n| format!("@P{n}")).collect::<Vec<_>>().join(",")
        );
        let mut values = Vec::new();
        let templates = (0..128.min(2000 / columns.len())).map(|row| {
            values.push(format!("({})", (0..columns.len()).map(|col| format!("@P{}", row * columns.len() + col + 1)).collect::<Vec<_>>().join(",")));
            format!("DECLARE @import_transaction bigint = CURRENT_TRANSACTION_ID(); BEGIN TRY INSERT {qualified} ({names}) VALUES {}; SELECT @@ROWCOUNT AS [l1:count]; END TRY BEGIN CATCH {} END CATCH", values.join(","), batch::fenced_catch("@import_transaction"))
        }).collect();
        Ok(Self {
            metadata: metadata(&target),
            target,
            shape,
            lock: format!("SELECT TOP (1) 1 FROM {qualified} WITH (TABLOCKX,HOLDLOCK)"),
            templates,
            display,
            ascii,
            #[cfg(test)]
            commit: None,
            #[cfg(test)]
            restore_suffix: None,
        })
    }
    fn commit_sql(&self) -> &str {
        #[cfg(test)]
        if let Some(sql) = &self.commit {
            return sql;
        }
        "COMMIT TRANSACTION"
    }
    pub fn validate_row(&self, row: &[Option<String>], line: u64, record: u64) -> Result<(), ImportError> {
        if row.iter().zip(&self.ascii).any(|(value, ascii)| *ascii && value.as_ref().is_some_and(|v| !v.is_ascii())) {
            return Err(ImportError::Csv {
                line,
                record,
                message: "SQL Server char/varchar import accepts ASCII only; use nchar/nvarchar for Unicode".into(),
            });
        }
        Ok(())
    }
    pub fn max_rows(&self) -> usize {
        self.templates.len()
    }
    pub fn descriptor(&self) -> String {
        serde_json::to_string(&(
            1,
            self,
            SETTINGS,
            batch::PROBE,
            "BEGIN TRANSACTION",
            "COMMIT TRANSACTION",
            "ROLLBACK TRANSACTION",
            "SET ANSI_WARNINGS ON/OFF; SET IMPLICIT_TRANSACTIONS ON/OFF; SET XACT_ABORT ON/OFF; SET NOCOUNT ON/OFF",
            "128 rows / 256KiB / 2000 parameters; nullable Unicode; ASCII char/varchar",
        ))
        .unwrap()
    }
    pub fn display(&self) -> &str {
        &self.display
    }
}

pub(crate) struct ImportRows {
    pub rows: Vec<Vec<Option<String>>>,
    pub bytes: u64,
    pub records: u64,
    pub first_record: u64,
}

fn query(error: QueryError) -> ImportError {
    ImportError::Query { error }
}
fn unknown(message: impl Into<String>) -> ImportError {
    ImportError::OutcomeUnknown {
        error: error(format!(
            "{}. Import may have committed. The session was retired; do not retry without checking stored rows.",
            message.into()
        )),
    }
}

fn metadata(target: &ImportTarget) -> String {
    let oid = native_object_id(target.oid);
    let name = quote_literal(&format!("{}.{}", quote_ident(&target.schema), quote_ident(&target.table)));
    let violations = checks::violations("l.object_id", Children::All);
    format!(
        r#"
DECLARE @id int = OBJECT_ID({name}, N'U');
IF ISNULL(HAS_PERMS_BY_NAME(DB_NAME(), 'DATABASE', 'VIEW DEFINITION'),0) <> 1 OR ISNULL(HAS_PERMS_BY_NAME({name}, 'OBJECT', 'VIEW DEFINITION'),0) <> 1
    THROW 50000, 'CSV import requires database and target VIEW DEFINITION to verify target safety', 1;
IF @id IS NULL OR @id <> {oid} THROW 50000, 'CSV import target identity changed', 1;
IF ISNULL(IS_SRVROLEMEMBER('sysadmin'),0) <> 1 AND USER_NAME() <> N'dbo'
BEGIN
    IF ISNULL(HAS_PERMS_BY_NAME(DB_NAME(), 'DATABASE', 'VIEW SECURITY DEFINITION'),0) <> 1
        THROW 50000, 'CSV import requires VIEW SECURITY DEFINITION to check metadata visibility', 1;
    IF EXISTS (SELECT 1 FROM sys.database_permissions WHERE state='D' AND permission_name IN ('VIEW DEFINITION','CONTROL'))
        THROW 50000, 'CSV import cannot prove metadata visibility while database permissions contain DENY VIEW DEFINITION or DENY CONTROL', 1;
END;
DECLARE @reason nvarchar(128), @message nvarchar(2048);
SELECT TOP (1) @reason = v.reason FROM (VALUES (@id)) AS l(object_id) CROSS APPLY ({violations}) AS v(reason, detail);
SET @message = CASE @reason
    WHEN N'kind' THEN N'CSV import supports ordinary disk-based tables only'
    WHEN N'trigger' THEN N'CSV import does not support enabled DML triggers'
    WHEN N'policy' THEN N'CSV import does not support row-level security policies'
    WHEN N'ignore_dup_key' THEN N'CSV import does not support IGNORE_DUP_KEY indexes'
    WHEN N'indexed_view' THEN N'CSV import does not support indexed-view maintenance'
    WHEN N'function' THEN N'CSV import does not support unresolved or user-function expression dependencies'
    WHEN N'permission' THEN N'CSV import requires database and target VIEW DEFINITION to verify target safety'
    ELSE N'CSV import does not support CLR, alias, encrypted, or bound-rule/default columns' END;
IF @reason IS NOT NULL THROW 50000, @message, 1;
SELECT (SELECT DB_NAME() AS db, t.object_id, t.create_date, t.modify_date,
    (SELECT c.*, dc.definition AS default_expression, cc.definition AS computed_expression FROM sys.columns c LEFT JOIN sys.default_constraints dc ON dc.object_id=c.default_object_id LEFT JOIN sys.computed_columns cc ON cc.object_id=c.object_id AND cc.column_id=c.column_id WHERE c.object_id=@id ORDER BY c.column_id FOR JSON PATH) AS columns,
    (SELECT object_id, name, type, create_date, modify_date FROM sys.objects WHERE parent_object_id=@id ORDER BY object_id FOR JSON PATH) AS children,
    (SELECT * FROM sys.indexes WHERE object_id=@id ORDER BY index_id FOR JSON PATH) AS indexes
    FROM sys.tables t WHERE t.object_id=@id FOR JSON PATH) AS signature;
SELECT c.name, t.name FROM sys.columns c JOIN sys.types t ON t.user_type_id=c.user_type_id WHERE c.object_id=@id AND c.is_identity=0 AND c.is_computed=0 AND c.generated_always_type=0 AND t.name NOT IN ('timestamp','rowversion') ORDER BY c.column_id;
"#
    )
}

async fn rows(client: &mut TdsClient, sql: &str) -> Result<Vec<Vec<Option<String>>>, QueryError> {
    let mut stream = native_query(client, sql);
    let mut rows = Vec::new();
    let mut failure = None;
    while let Some(event) = stream.next().await {
        match event? {
            QueryEvent::Row(row) => rows.push(row),
            QueryEvent::ServerError(e) => {
                failure.get_or_insert(e);
            }
            _ => {}
        }
    }
    failure.map_or(Ok(rows), Err)
}
fn shape(rows: Vec<Vec<Option<String>>>) -> Result<ImportTargetShape, QueryError> {
    let first = rows.first().ok_or_else(|| error("Missing import target metadata"))?;
    Ok(ImportTargetShape {
        signature: required(first, 0)?.into(),
        columns: rows
            .iter()
            .skip(1)
            .map(|r| Ok((required(r, 0)?.into(), required(r, 1)?.into())))
            .collect::<Result<_, QueryError>>()?,
    })
}

impl MsSession {
    pub(in crate::session) async fn import_target(
        &mut self,
        target: &ImportTarget,
    ) -> Result<ImportTargetShape, QueryError> {
        shape(self.rows(&metadata(target)).await?)
    }
}

async fn state(client: &mut TdsClient, database: &str, expected: TxnState) -> Result<batch::Probe, QueryError> {
    let batch::Probed::State(probe) = batch::probe(client, database).await? else {
        return Err(batch::unexpected(&[], &["l1:state"]));
    };
    let (count, active) = if expected == TxnState::Idle { (0, 0) } else { (1, 1) };
    if probe.count != count || probe.state != active || probe.native != expected {
        return Err(error(format!(
            "SQL Server import transaction state changed unexpectedly: expected {expected:?}, server {} {}, native {:?}",
            probe.count, probe.state, probe.native
        )));
    }
    Ok(probe)
}

async fn check_fence(client: &mut TdsClient, database: &str, id: &str) -> Result<(), QueryError> {
    if state(client, database, TxnState::Open).await?.transaction != id {
        return Err(error("SQL Server import transaction identity changed"));
    }
    Ok(())
}

enum BatchOutcome {
    Inserted,
    RolledBack(ImportError),
}

async fn insert(client: &mut TdsClient, plan: &InsertPlan, chunk: &ImportRows) -> Result<BatchOutcome, ImportError> {
    let values = chunk.rows.iter().flatten().map(|v| v.as_deref()).collect::<Vec<_>>();
    let params = values.iter().map(|v| v as &dyn tiberius::ToSql).collect::<Vec<_>>();
    let reply = batch::module(client, &plan.templates[chunk.rows.len() - 1], &params).await.map_err(query)?;
    if let Some(mut e) = reply.rolled_back {
        e.context = Some(format!(
            "CSV records {} through {}",
            chunk.first_record,
            chunk.first_record + chunk.rows.len() as u64 - 1
        ));
        return Ok(BatchOutcome::RolledBack(query(e)));
    }
    let rows = reply.single("l1:count").map_err(query)?;
    if rows.len() != 1 || number::<usize>(&rows[0], 0).map_err(query)? != chunk.rows.len() {
        return Err(query(error("SQL Server did not insert every CSV row")));
    }
    Ok(BatchOutcome::Inserted)
}

enum FenceState {
    Owned,
    RolledBack,
}

impl MsSession {
    pub(in crate::session) async fn import_rows(
        &mut self,
        plan: &InsertPlan,
        mut chunks: mpsc::Receiver<Result<ImportRows, ImportError>>,
        producer: tokio::task::JoinHandle<Result<(), ImportError>>,
        stop: &Arc<AtomicBool>,
        progress: watch::Sender<(u64, u64)>,
    ) -> Result<u64, ImportError> {
        let database = self.database.clone();
        let Some(client) = self.client.take() else {
            chunks.close();
            stop.store(true, Ordering::Relaxed);
            let _ = producer.await;
            return Err(unknown("Import session unavailable"));
        };
        let mut lease = Lease { owner: self, client: Some(client) };
        let mut producer = Some(producer);
        let mut settled = None;
        let work = async {
            let options = state(lease.client(), &database, TxnState::Idle).await.map_err(query)?.options;
            if stop.load(Ordering::Relaxed) {
                return Err(ImportError::Cancelled);
            }
            if let Err(e) = rows(lease.client(), SETTINGS).await {
                lease.retire(e.message.clone());
                return Err(query(e));
            }
            let result = async {
                rows(lease.client(), "BEGIN TRANSACTION").await.map_err(|e| unknown(e.message))?;
                let id =
                    state(lease.client(), &database, TxnState::Open).await.map_err(|e| unknown(e.message))?.transaction;
                let mut fence = FenceState::Owned;
                let imported = async {
                    rows(lease.client(), &plan.lock).await.map_err(query)?;
                    let actual = shape(rows(lease.client(), &plan.metadata).await.map_err(query)?).map_err(query)?;
                    if actual.signature != plan.shape.signature {
                        return Err(ImportError::TargetChanged);
                    }
                    let mut total = 0;
                    while let Some(chunk) = chunks.recv().await {
                        if stop.load(Ordering::Relaxed) {
                            return Err(ImportError::Cancelled);
                        }
                        let chunk = chunk?;
                        check_fence(lease.client(), &database, &id).await.map_err(|e| unknown(e.message))?;
                        if !chunk.rows.is_empty() {
                            match insert(lease.client(), plan, &chunk).await? {
                                BatchOutcome::Inserted => {}
                                BatchOutcome::RolledBack(e) => {
                                    fence = FenceState::RolledBack;
                                    return Err(e);
                                }
                            }
                        }
                        total += chunk.rows.len() as u64;
                        progress.send_replace((chunk.bytes, chunk.records));
                    }
                    Ok(total)
                }
                .await;
                chunks.close();
                if imported.is_err() {
                    stop.store(true, Ordering::Relaxed);
                }
                let produced =
                    producer.as_mut().unwrap().await.map_err(|e| query(error(e.to_string()))).and_then(|r| r);
                producer.take();
                let imported = imported.and_then(|n| produced.map(|()| n));
                if matches!(fence, FenceState::RolledBack) {
                    state(lease.client(), &database, TxnState::Idle).await.map_err(|e| unknown(e.message))?;
                    return imported;
                }
                check_fence(lease.client(), &database, &id)
                    .await
                    .map_err(|e| unknown(format!("{}; operation result {imported:?}", e.message)))?;
                match imported {
                    Ok(count) if !stop.load(Ordering::Relaxed) => {
                        rows(lease.client(), plan.commit_sql()).await.map_err(|e| unknown(e.message))?;
                        if observe(lease.client(), &database).map_err(|e| unknown(e.message))? != TxnState::Idle {
                            return Err(unknown("SQL Server did not acknowledge an idle session after COMMIT"));
                        }
                        Ok(count)
                    }
                    result => {
                        rows(lease.client(), "ROLLBACK TRANSACTION").await.map_err(|e| unknown(e.message))?;
                        state(lease.client(), &database, TxnState::Idle).await.map_err(|e| unknown(e.message))?;
                        result.and(Err(ImportError::Cancelled))
                    }
                }
            }
            .await;
            if matches!(result, Err(ImportError::OutcomeUnknown { .. })) {
                return result;
            }
            settled = Some(result.clone());
            let restore = format!(
                "SET ANSI_WARNINGS {}; SET IMPLICIT_TRANSACTIONS {}; SET XACT_ABORT {}; SET NOCOUNT {}",
                if options & 8 != 0 { "ON" } else { "OFF" },
                if options & 2 != 0 { "ON" } else { "OFF" },
                if options & 16384 != 0 { "ON" } else { "OFF" },
                if options & 512 != 0 { "ON" } else { "OFF" }
            );
            #[cfg(test)]
            let restore = format!("{restore}; {}", plan.restore_suffix.as_deref().unwrap_or(""));
            match tokio::time::timeout(CLEANUP, rows(lease.client(), &restore)).await {
                Ok(Ok(_)) => {}
                _ => lease.retire("CSV import settled but settings restoration failed".into()),
            }
            result
        };
        let mut result = batch::settle(stop, work, || Err(unknown("CSV import cleanup deadline expired"))).await;
        if matches!(result, Err(ImportError::OutcomeUnknown { .. }))
            && let Some(outcome) = settled
        {
            lease.retire("CSV import settled but session cleanup did not finish".into());
            result = outcome;
        }
        chunks.close();
        if let Some(producer) = producer {
            stop.store(true, Ordering::Relaxed);
            let _ = producer.await;
        }
        let retire = match &result {
            Err(ImportError::OutcomeUnknown { .. }) => Some("CSV import outcome unknown".into()),
            Err(ImportError::Query { error }) if error.severity == "FATAL" => {
                Some("CSV import connection failed".into())
            }
            _ => None,
        };
        lease.finish(retire, &database);
        result
    }
}

#[cfg(all(test, feature = "mssql-tests"))]
mod tests;
