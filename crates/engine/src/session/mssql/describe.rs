use super::{MsSession, number, object_id};
use crate::model::{ColumnMeta, ColumnSource, QueryError};
use crate::sql::tsql::quote_literal;

/// The columns of the first result set a batch returns, as SQL Server describes them without running it.
pub(crate) struct FirstResult(Vec<(String, Option<ColumnSource>)>);

impl FirstResult {
    /// Gives `columns` their sources when they are the described columns, in order.
    pub(crate) fn apply(self, columns: &mut [ColumnMeta]) {
        if self.0.len() == columns.len() && self.0.iter().zip(&*columns).all(|((name, _), c)| *name == c.name) {
            for ((_, source), c) in self.0.into_iter().zip(columns) {
                c.source = source;
            }
        }
    }
}

impl MsSession {
    /// Browse mode 2 names a view's column as the view's, as PostgreSQL does. A source on another server or in
    /// another database has no catalog entry here, so it stays empty. The `IF` keeps the query from opening a
    /// transaction under `IMPLICIT_TRANSACTIONS`. The function returns compile errors as rows, so it cannot end
    /// an `XACT_ABORT` transaction over a batch that does not compile.
    pub(in crate::session) async fn first_result(&mut self, batch: &str) -> Result<Option<FirstResult>, QueryError> {
        let sql = format!(
            "IF @@OPTIONS & 2 = 0 OR @@TRANCOUNT > 0
             SELECT CAST(d.is_hidden AS int), d.name, d.error_number, o.object_id, c.column_id
             FROM sys.dm_exec_describe_first_result_set({}, NULL, 2) d
             LEFT JOIN sys.schemas s ON d.source_server IS NULL AND d.source_database = DB_NAME() AND s.name = d.source_schema
             LEFT JOIN sys.objects o ON o.schema_id = s.schema_id AND o.name = d.source_table AND o.type IN ('U', 'V')
             LEFT JOIN sys.columns c ON c.object_id = o.object_id AND c.name = d.source_column
             ORDER BY d.column_ordinal",
            quote_literal(batch)
        );
        let sets = self.results(&sql).await?;
        Ok(sets.first().and_then(|rows| parse(rows)))
    }
}

/// `None` when the batch has no single first result set, or when the rows are not this description, as under
/// `SET SHOWPLAN_XML ON`.
fn parse(rows: &[Vec<Option<String>>]) -> Option<FirstResult> {
    let mut columns = Vec::new();
    for row in rows {
        let [hidden, name, error, table, column] = row.as_slice() else { return None };
        if error.is_some() {
            return None;
        }
        match hidden.as_deref()? {
            "1" => continue,
            "0" => {}
            _ => return None,
        }
        let source = match (table, column) {
            (Some(_), Some(_)) => {
                Some(ColumnSource { table_oid: object_id(row, 3).ok()?, attnum: number(row, 4).ok()? })
            }
            _ => None,
        };
        columns.push((name.clone().unwrap_or_default(), source));
    }
    (!columns.is_empty()).then_some(FirstResult(columns))
}
