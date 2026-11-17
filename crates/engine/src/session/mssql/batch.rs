//! Batches whose every result set names its kind in its first column, and the fenced `CATCH` that CSV import
//! and staged edits share.
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

pub(super) const CLEANUP: Duration = Duration::from_secs(10);

/// The first column of the marker a fenced `CATCH` returns after it rolled back its own transaction.
const ROLLED_BACK: &str = "l1:rolled_back";
/// SQL Server's "Transaction count after EXECUTE indicates a mismatching number of BEGIN and COMMIT statements",
/// which follows a module that rolled back the transaction it was called in.
const COUNT_CHANGED: u32 = 266;

/// Rolls back only the transaction that `fence` proves is the caller's, and says so with the marker; every other
/// failure re-throws, because a trigger may have committed and replaced that transaction, and `XACT_ABORT` would
/// then roll back the replacement and hide it.
pub(super) fn fenced_catch(fence: &str) -> String {
    format!(
        "IF {fence} IS NOT NULL AND @@TRANCOUNT = 1 AND CURRENT_TRANSACTION_ID() = {fence} BEGIN ROLLBACK TRANSACTION; SELECT ERROR_NUMBER() AS [{ROLLED_BACK}], ERROR_SEVERITY(), ERROR_STATE(), ERROR_LINE(), ERROR_PROCEDURE(), ERROR_MESSAGE(); END ELSE THROW;"
    )
}

pub(super) struct Set {
    pub tag: String,
    pub rows: Vec<Vec<Option<String>>>,
}

pub(super) struct Reply {
    pub sets: Vec<Set>,
    /// The error a fenced `CATCH` caught before it rolled back.
    pub rolled_back: Option<QueryError>,
}

impl Reply {
    /// The sets, which must carry exactly these tags in this order.
    pub fn expect(self, tags: &[&str]) -> Result<Vec<Set>, QueryError> {
        if self.sets.len() != tags.len() || self.sets.iter().zip(tags).any(|(set, tag)| set.tag != *tag) {
            return Err(unexpected(&self.sets, tags));
        }
        Ok(self.sets)
    }
    /// The rows of the one set this batch returns.
    pub fn single(self, tag: &str) -> Result<Vec<Vec<Option<String>>>, QueryError> {
        Ok(self.expect(&[tag])?.pop().expect("one set").rows)
    }
}

pub(super) fn unexpected(sets: &[Set], tags: &[&str]) -> QueryError {
    let got = sets.iter().map(|set| set.tag.as_str()).collect::<Vec<_>>();
    error(format!(
        "SQL Server returned result sets {got:?} where L1 Data expected {tags:?}. A session setting such as SHOWPLAN, NOEXEC, FMTONLY, PARSEONLY, or STATISTICS XML or PROFILE changes what batches return."
    ))
}

fn marker(row: &[Option<String>]) -> Result<QueryError, QueryError> {
    let code: u32 = number(row, 0)?;
    let state = row.get(2).cloned().flatten().unwrap_or_default();
    let line = row.get(3).cloned().flatten().unwrap_or_default();
    let class: u8 = number(row, 1)?;
    Ok(QueryError {
        severity: if class >= 20 { "FATAL" } else { "ERROR" }.into(),
        sqlstate: None,
        message: required(row, 5)?.to_owned(),
        detail: Some(format!("{} state {state}, line {line}", server_error_detail(code))),
        hint: None,
        context: row.get(4).cloned().flatten().filter(|p| !p.is_empty()),
        position: None,
    })
}

/// The SQL Server error number of an error this driver built.
pub(super) fn code(e: &QueryError) -> Option<u32> {
    e.detail.as_deref()?.strip_prefix("SQL Server error ")?.split(',').next()?.parse().ok()
}

/// Reads a parameterized module, sent as one `sp_executesql` RPC.
pub(super) async fn module(
    client: &mut TdsClient,
    sql: &str,
    params: &[&dyn tiberius::ToSql],
) -> Result<Reply, QueryError> {
    let stream = client.query(sql, params).await.map_err(query_error)?;
    let mut stream = std::pin::pin!(stream);
    let mut sets: Vec<Set> = Vec::new();
    let mut failure = None;
    let mut rolled_back = None;
    loop {
        let item = match stream.try_next().await {
            Ok(Some(item)) => item,
            Ok(None) => break,
            Err(tiberius::error::Error::Server(e)) if e.code() == COUNT_CHANGED && rolled_back.is_some() => break,
            Err(e) => {
                failure.get_or_insert(query_error(e));
                break;
            }
        };
        match item {
            QueryItem::Metadata(meta) => {
                let tag = meta.columns().first().map(|c| c.name().to_owned()).unwrap_or_default();
                sets.push(Set { tag, rows: Vec::new() });
            }
            QueryItem::Row(row) => {
                let cells = row.cells().map(|(_, data)| values::text(data)).collect::<Result<Vec<_>, _>>()?;
                let set = sets.last_mut().ok_or_else(|| error("SQL Server sent a row before its columns"))?;
                if set.tag == ROLLED_BACK {
                    rolled_back = Some(marker(&cells)?);
                }
                set.rows.push(cells);
            }
            QueryItem::Error(e) if e.code() == COUNT_CHANGED && rolled_back.is_some() => {}
            QueryItem::Error(e) => {
                failure.get_or_insert(query_error(tiberius::error::Error::Server(e)));
            }
            _ => {}
        }
    }
    if let Some(e) = failure {
        return Err(e);
    }
    if rolled_back.is_some() {
        sets.retain(|set| set.tag != ROLLED_BACK);
    }
    Ok(Reply { sets, rolled_back })
}

/// Reads a plain batch, sent through `simple_query`. Any server error fails it.
pub(super) async fn plain(client: &mut TdsClient, sql: &str) -> Result<Reply, QueryError> {
    let mut stream = native_query(client, sql);
    let mut sets: Vec<Set> = Vec::new();
    let mut failure = None;
    while let Some(event) = stream.next().await {
        match event? {
            QueryEvent::Columns(columns) => {
                sets.push(Set { tag: columns.first().map(|c| c.name.clone()).unwrap_or_default(), rows: Vec::new() })
            }
            QueryEvent::Row(row) => {
                sets.last_mut().ok_or_else(|| error("SQL Server sent a row before its columns"))?.rows.push(row)
            }
            QueryEvent::ServerError(e) => {
                failure.get_or_insert(e);
            }
            _ => {}
        }
    }
    failure.map_or(Ok(Reply { sets, rolled_back: None }), Err)
}

/// The session's transaction and the settings that change what an edit or import may send.
pub(super) struct Probe {
    pub count: u32,
    pub state: i32,
    pub options: u32,
    pub textsize: i64,
    pub transaction: String,
    pub native: TxnState,
}

/// Reads into variables first and selects them from a constant scan: a `SELECT` that reads a table reports its own
/// autocommit transaction in `XACT_STATE()`, and a `SELECT` without `FROM` has no plan, so `STATISTICS XML` and
/// `STATISTICS PROFILE` would add no set to show they are on.
pub(super) const PROBE: &str = "DECLARE @c int = @@TRANCOUNT; DECLARE @s int = XACT_STATE(); DECLARE @o int = @@OPTIONS; DECLARE @t int = @@TEXTSIZE; DECLARE @id bigint = CURRENT_TRANSACTION_ID(); SELECT @c AS [l1:state], @s, @o, @t, @id FROM (VALUES (1)) AS v(x) WHERE v.x = 1";

pub(super) enum Probed {
    State(Probe),
    /// The probe came back with other result sets, so a session setting changes what batches return.
    Shape,
}

pub(super) async fn probe(client: &mut TdsClient, database: &str) -> Result<Probed, QueryError> {
    let reply = plain(client, PROBE).await?;
    let [Set { tag, rows }] = reply.sets.as_slice() else { return Ok(Probed::Shape) };
    let [row] = rows.as_slice() else { return Ok(Probed::Shape) };
    if tag != "l1:state" {
        return Ok(Probed::Shape);
    }
    Ok(Probed::State(Probe {
        count: number(row, 0)?,
        state: number(row, 1)?,
        options: number(row, 2)?,
        textsize: number(row, 3)?,
        transaction: required(row, 4)?.to_owned(),
        native: observe(client, database)?,
    }))
}

/// Waits for `work`, but no longer than `CLEANUP` after `stop` is set. The caller then retires its client, which
/// closes the connection, so the server rolls back whatever is open, and the outcome is `expired`.
pub(super) async fn settle<T>(
    stop: &AtomicBool,
    work: impl std::future::Future<Output = T>,
    expired: impl FnOnce() -> T,
) -> T {
    tokio::pin!(work);
    let deadline = async {
        while !stop.load(Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        tokio::time::sleep(CLEANUP).await;
    };
    tokio::select! { biased; result = &mut work => result, _ = deadline => expired() }
}

impl Lease<'_> {
    /// Gives the client back when the outcome is known and the session still reads as it should, and retires it
    /// otherwise.
    pub(super) fn finish(mut self, retire: Option<String>, database: &str) {
        if let Some(reason) = retire {
            self.retire(reason);
        } else if self.client.is_some() {
            match observe(self.client(), database) {
                Ok(txn) => self.restore(txn),
                Err(e) => self.retire(e.message),
            }
        }
    }
}
