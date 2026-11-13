mod catalog;
mod detail;
mod map;
mod source;
mod types;

use std::error::Error as _;
use std::future::poll_fn;
use std::str::FromStr;
use std::time::Duration;
use tokio_postgres::config::Host;
use tokio_postgres::error::ErrorPosition;

use postgres_native_tls::MakeTlsConnector;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, watch};
use tokio_postgres::error::DbError;
use tokio_postgres::tls::MakeTlsConnect;
use tokio_postgres::{AsyncMessage, CancelToken, Client, Connection, SimpleQueryMessage};

use super::stream::DbStream;
use crate::connection::{CONNECT_TIMEOUT, ConnectParams, KEEPALIVE_IDLE};
use crate::model::*;
use crate::text::quote_ident;

pub(super) struct PgSession {
    client: Client,
    pub backend_pid: i32,
    pub server: ServerInfo,
    search_path: SearchPath,
    cancel: PgCancel,
    notices: mpsc::UnboundedReceiver<DbError>,
    closed: watch::Receiver<Option<String>>,
    retired: Option<String>,
}

enum SearchPath {
    /// Set outside a transaction, so no rollback reverts it.
    Settled(Option<String>),
    /// Not set yet, or set inside a transaction. A rollback of that transaction, or to a
    /// savepoint taken before the `SET`, puts the server back on the path it had.
    Unsettled,
}

#[derive(Clone)]
pub(super) struct PgCancel {
    token: CancelToken,
    tls: MakeTlsConnector,
    /// A tunneled session's cancel goes through a fresh channel of its tunnel, since the token has
    /// no socket to dial. Kept without the password, which a cancel request does not send.
    tunnel: Option<ConnectParams>,
}

impl PgCancel {
    pub async fn cancel(&self) {
        let sent = match &self.tunnel {
            None => self.token.cancel_query(self.tls.clone()).await.map_err(|e| error_text(&e)),
            Some(p) => self.cancel_through(p).await,
        };
        if let Err(e) = sent {
            eprintln!("cancel request failed: {e}");
        }
    }

    async fn cancel_through(&self, p: &ConnectParams) -> Result<(), String> {
        let stream = p.dial().await.map_err(|e| crate::connection::connect_error_text(&e))?;
        let tls =
            MakeTlsConnect::<DbStream>::make_tls_connect(&mut self.tls.clone(), &p.host).map_err(|e| e.to_string())?;
        self.token.cancel_query_raw(stream, tls).await.map_err(|e| error_text(&e))
    }
}

fn drive<S, T>(mut connection: Connection<S, T>) -> (mpsc::UnboundedReceiver<DbError>, watch::Receiver<Option<String>>)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (notice_tx, notices) = mpsc::unbounded_channel();
    let (closed_tx, closed) = watch::channel(None);
    tokio::spawn(async move {
        let reason = loop {
            match poll_fn(|cx| connection.poll_message(cx)).await {
                Some(Ok(AsyncMessage::Notice(n))) => {
                    let _ = notice_tx.send(n);
                }
                Some(Ok(_)) => {}
                Some(Err(e)) => break error_text(&e),
                None => break "connection closed".to_owned(),
            }
        };
        let _ = closed_tx.send(Some(reason));
    });
    (notices, closed)
}

impl PgSession {
    pub async fn open(p: &ConnectParams, schema: Option<&str>, timezone: &str) -> Result<PgSession, ConnectError> {
        let tls = p.tls()?;
        let (client, (notices, closed)) = if p.via.is_none() {
            let (client, connection) = p.pg_config().connect(tls.clone()).await.map_err(connect_error)?;
            (client, drive(connection))
        } else {
            let stream = p.dial().await?;
            let connector = MakeTlsConnect::<DbStream>::make_tls_connect(&mut tls.clone(), &p.host)
                .map_err(|e| ConnectError::Tls { message: e.to_string() })?;
            // `connect_raw` ignores the config's connect_timeout.
            let login = tokio::time::timeout(CONNECT_TIMEOUT, p.pg_config().connect_raw(stream, connector))
                .await
                .map_err(|_| ConnectError::Network {
                    message: format!(
                        "{}:{} did not finish the PostgreSQL login within {} s",
                        p.host,
                        p.port,
                        CONNECT_TIMEOUT.as_secs()
                    ),
                })?;
            let (client, connection) = login.map_err(connect_error)?;
            (client, drive(connection))
        };

        // An OS zone Postgres does not know is not worth failing the connection over.
        let _ = client.batch_execute(&format!("SET TimeZone = '{}'", timezone.replace('\'', "''"))).await;
        let server = server_info(&client).await.map_err(connect_error)?;
        let mut session = PgSession {
            backend_pid: server.1,
            server: server.0,
            search_path: SearchPath::Unsettled,
            cancel: PgCancel {
                token: client.cancel_token(),
                tls,
                tunnel: p.via.is_some().then(|| ConnectParams { password: None, ..p.clone() }),
            },
            notices,
            closed,
            retired: None,
            client,
        };
        session.use_schema(schema, TxnState::Idle).await.map_err(connect_error)?;
        Ok(session)
    }

    pub fn settled_on(&self, schema: Option<&str>) -> bool {
        matches!(&self.search_path, SearchPath::Settled(s) if s.as_deref() == schema)
    }

    pub async fn use_schema(&mut self, schema: Option<&str>, txn: TxnState) -> Result<(), tokio_postgres::Error> {
        // An aborted transaction refuses `SET`. The first statement after it sends the path.
        if txn == TxnState::Aborted || self.settled_on(schema) {
            return Ok(());
        }
        if let Some(s) = schema {
            self.client.batch_execute(&format!("SET search_path TO {}", quote_ident(s))).await?;
        } else {
            self.client.batch_execute("RESET search_path").await?;
        }
        self.search_path =
            if txn == TxnState::Idle { SearchPath::Settled(schema.map(str::to_owned)) } else { SearchPath::Unsettled };
        Ok(())
    }

    pub fn cancel_handle(&self) -> PgCancel {
        self.cancel.clone()
    }

    pub fn drain_notices(&mut self) -> Vec<DbError> {
        let mut out = Vec::new();
        while let Ok(n) = self.notices.try_recv() {
            out.push(n);
        }
        out
    }

    /// `Some(reason)` once the connection is dead. The client learns of a dead socket before the
    /// driver task records why, so a closed client without a reason yet gets a generic one.
    pub fn closed_reason(&self) -> Option<String> {
        self.retired
            .clone()
            .or_else(|| self.closed.borrow().clone())
            .or_else(|| self.client.is_closed().then(|| "connection closed".to_owned()))
    }

    /// Like `closed_reason`, but gives the driver task a moment to deliver the server's reason.
    /// A FATAL error answered to a statement (`terminating connection due to administrator
    /// command`) means the server is closing the socket; `fatal` carries that message so the
    /// session counts as dead even if EOF has not been read yet.
    pub async fn closed_reason_settled(&mut self, fatal: Option<String>) -> Option<String> {
        if self.closed.borrow().is_none() && (fatal.is_some() || self.client.is_closed()) {
            let _ = tokio::time::timeout(Duration::from_millis(500), self.closed.changed()).await;
        }
        self.closed.borrow().clone().or(fatal).or_else(|| self.closed_reason())
    }

    pub fn closed_watch(&self) -> watch::Receiver<Option<String>> {
        self.closed.clone()
    }
}

async fn server_info(client: &Client) -> Result<(ServerInfo, i32), tokio_postgres::Error> {
    let messages = client
        .simple_query(
            "SELECT pg_backend_pid()::text, current_setting('server_version'), current_setting('server_version_num'), current_setting('server_encoding')",
        )
        .await?;
    let row = messages.iter().find_map(|m| match m {
        SimpleQueryMessage::Row(r) => Some(r),
        _ => None,
    });
    let cell = |i: usize| row.and_then(|r| r.get(i)).unwrap_or("");
    let full_version = cell(1);
    Ok((
        ServerInfo {
            version: full_version.split_whitespace().next().unwrap_or(full_version).to_owned(),
            version_num: cell(2).parse().unwrap_or(0),
            encoding: cell(3).to_owned(),
        },
        cell(0).parse().unwrap_or(0),
    ))
}

impl ConnectParams {
    fn pg_config(&self) -> tokio_postgres::Config {
        let mut cfg = tokio_postgres::Config::new();
        cfg.host(&self.host)
            .port(self.port)
            .dbname(&self.database)
            .user(&self.user)
            .application_name("L1 Data")
            .connect_timeout(CONNECT_TIMEOUT)
            .keepalives(true)
            .keepalives_idle(KEEPALIVE_IDLE)
            .ssl_mode(match self.ssl_mode {
                SslMode::Disable => tokio_postgres::config::SslMode::Disable,
                SslMode::Prefer => tokio_postgres::config::SslMode::Prefer,
                SslMode::Require | SslMode::VerifyCa | SslMode::VerifyFull => tokio_postgres::config::SslMode::Require,
            });
        if let Some(pw) = &self.password {
            cfg.password(pw);
        }
        cfg
    }

    /// libpq semantics: `require` encrypts without verifying, `verify-ca` checks the chain,
    /// `verify-full` also checks the host name.
    fn tls(&self) -> Result<MakeTlsConnector, ConnectError> {
        let mut builder = native_tls::TlsConnector::builder();
        match self.ssl_mode {
            SslMode::Disable | SslMode::Prefer | SslMode::Require => {
                builder.danger_accept_invalid_certs(true).danger_accept_invalid_hostnames(true);
            }
            SslMode::VerifyCa => {
                builder.danger_accept_invalid_hostnames(true);
            }
            SslMode::VerifyFull => {}
        }
        if let Some(path) = &self.ca_cert_path {
            let pem = std::fs::read(path)
                .map_err(|e| ConnectError::Tls { message: format!("CA certificate {path}: {e}") })?;
            let cert =
                native_tls::Certificate::from_pem(&pem).map_err(|e| ConnectError::Tls { message: e.to_string() })?;
            builder.add_root_certificate(cert);
        }
        let connector = builder.build().map_err(|e| ConnectError::Tls { message: e.to_string() })?;
        Ok(MakeTlsConnector::new(connector))
    }
}

pub fn draft_from_url(url: &str, name: &str) -> Result<ConnectionDraft, String> {
    // tokio-postgres rejects the verify modes, which `tls()` implements on top of `require`.
    let verify = [("sslmode=verify-ca", SslMode::VerifyCa), ("sslmode=verify-full", SslMode::VerifyFull)]
        .into_iter()
        .find(|(param, _)| url.contains(param));
    let url = verify.map_or_else(|| url.to_owned(), |(param, _)| url.replace(param, "sslmode=require"));
    let cfg = tokio_postgres::Config::from_str(&url).map_err(|e| error_text(&e))?;
    let host = match cfg.get_hosts().first() {
        Some(Host::Tcp(h)) => h.clone(),
        #[cfg(unix)]
        Some(Host::Unix(p)) => p.display().to_string(),
        None => "localhost".to_owned(),
    };
    let password = cfg.get_password().map(|b| String::from_utf8(b.to_vec()).map_err(|e| e.to_string())).transpose()?;
    Ok(ConnectionDraft {
        database_kind: crate::model::DatabaseKind::Postgres,
        id: None,
        name: name.to_owned(),
        host,
        port: cfg.get_ports().first().copied().unwrap_or(5432),
        database: cfg.get_dbname().unwrap_or("postgres").to_owned(),
        user: cfg.get_user().unwrap_or("postgres").to_owned(),
        ssl_mode: match (verify, cfg.get_ssl_mode()) {
            (Some((_, mode)), _) => mode,
            (None, tokio_postgres::config::SslMode::Disable) => SslMode::Disable,
            (None, tokio_postgres::config::SslMode::Require) => SslMode::Require,
            (None, _) => SslMode::Prefer,
        },
        ca_cert_path: None,
        password: password.map_or(PasswordInput::Clear, PasswordInput::Set),
        color: None,
        ssh: None,
        environment: None,
        sql_server_auth: None,
    })
}

/// tokio-postgres' `Display` names only the kind; the cause carries the useful part.
pub fn error_text(e: &tokio_postgres::Error) -> String {
    match e.source() {
        Some(src) => format!("{e}: {src}"),
        None => e.to_string(),
    }
}

pub fn connect_error(e: tokio_postgres::Error) -> ConnectError {
    if let Some(db) = e.as_db_error() {
        let code = db.code().code();
        return if code.starts_with("28") {
            ConnectError::Auth { message: db.message().to_owned() }
        } else {
            ConnectError::Server(query_error(db, None))
        };
    }
    let text = error_text(&e);
    let display = e.to_string();
    if display.contains("TLS") {
        ConnectError::Tls { message: text }
    } else if display == "invalid configuration" && text.contains("password missing") {
        ConnectError::MissingPassword
    } else {
        ConnectError::Network { message: text }
    }
}

pub fn query_error(db: &DbError, position: Option<DocOffset>) -> QueryError {
    QueryError {
        severity: db.severity().to_owned(),
        sqlstate: Some(db.code().code().to_owned()),
        message: db.message().to_owned(),
        detail: db.detail().map(str::to_owned),
        hint: db.hint().map(str::to_owned),
        context: db.where_().map(str::to_owned),
        position,
    }
}

pub fn original_position(db: &DbError) -> Option<u32> {
    match db.position() {
        Some(ErrorPosition::Original(n)) => Some(*n),
        _ => None,
    }
}

pub fn transport_error(e: &tokio_postgres::Error) -> QueryError {
    QueryError {
        severity: "ERROR".into(),
        sqlstate: None,
        message: error_text(e),
        detail: None,
        hint: None,
        context: None,
        position: None,
    }
}

pub(super) fn query_failure(e: tokio_postgres::Error, sql: Option<&str>) -> QueryError {
    match e.as_db_error() {
        Some(db) => {
            let position = sql.zip(original_position(db)).map(|(sql, n)| {
                let map = crate::text::OffsetMap::new(sql);
                map.pg_position(&crate::sql::StmtSpan { start: 0, end: sql.len() }, n)
            });
            query_error(db, position)
        }
        None => transport_error(&e),
    }
}

/// The server sends such a cell only when the session's `client_encoding` is not UTF8, and the driver keeps its bytes private.
fn not_utf8(column: &str) -> QueryError {
    QueryError {
        hint: Some("L1 Data reads text as UTF-8. Run SET client_encoding = 'UTF8' on this tab.".into()),
        ..super::error(format!("Column \"{column}\" holds text that is not valid UTF-8."))
    }
}

pub(super) fn notice(n: &DbError) -> OutputBody {
    OutputBody::Notice {
        severity: n.severity().to_owned(),
        message: n.message().to_owned(),
        detail: n.detail().map(str::to_owned),
    }
}

impl PgSession {
    pub(super) async fn batch(&self, sql: &str) -> Result<(), QueryError> {
        self.client.batch_execute(sql).await.map_err(|e| query_failure(e, Some(sql)))
    }
    pub(super) async fn import_csv(
        &mut self,
        sql: &str,
        mut chunks: mpsc::Receiver<Result<super::CopyChunk, ImportError>>,
        stop: &std::sync::atomic::AtomicBool,
        progress: watch::Sender<(u64, u64)>,
        mut emit: impl FnMut(OutputBody) + Send,
    ) -> Result<u64, ImportError> {
        use futures_util::SinkExt;
        use std::sync::atomic::Ordering;
        let query = |e| ImportError::Query { error: query_failure(e, Some(sql)) };
        let copying = async {
            if stop.load(Ordering::Relaxed) {
                return Err(ImportError::Cancelled);
            }
            let sink = self.client.copy_in::<_, bytes::Bytes>(sql).await.map_err(query)?;
            tokio::pin!(sink);
            while let Some(chunk) = chunks.recv().await {
                let chunk = chunk?;
                if stop.load(Ordering::Relaxed) {
                    return Err(ImportError::Cancelled);
                }
                sink.as_mut().send(bytes::Bytes::from(chunk.data)).await.map_err(query)?;
                progress.send_replace((chunk.bytes, chunk.rows));
            }
            if stop.load(Ordering::Relaxed) {
                return Err(ImportError::Cancelled);
            }
            sink.as_mut().finish().await.map_err(query)
        };
        tokio::pin!(copying);
        let mut tick = tokio::time::interval(crate::console::STREAM_EMIT_INTERVAL);
        let mut notices_open = true;
        loop {
            tokio::select! {
                result = &mut copying => return if stop.load(Ordering::Relaxed) { Err(ImportError::Cancelled) } else { result },
                _ = tick.tick() => if stop.load(Ordering::Relaxed) { return Err(ImportError::Cancelled); },
                next = self.notices.recv(), if notices_open => match next { Some(n) => emit(notice(&n)), None => notices_open = false },
            }
        }
    }
    pub(super) async fn execute_edit(&self, sql: &str) -> Result<u64, QueryError> {
        self.client.execute(sql, &[]).await.map_err(|e| query_failure(e, Some(sql)))
    }
    pub(super) fn query<'a>(&'a mut self, sql: &'a str) -> super::QueryStream<'a> {
        use super::QueryEvent;
        use futures_util::StreamExt;
        Box::pin(async_stream::try_stream! {
            let prepared = self.client.prepare(sql).await.map_err(|e| query_failure(e, Some(sql)))?;
            let columns = prepared.columns().iter().map(|c| ColumnMeta {
                name: c.name().to_owned(), r#type: types::type_ref(c.type_()),
                source: c.table_oid().zip(c.column_id()).map(|(table_oid, attnum)| ColumnSource { table_oid, attnum }),
            }).collect::<Vec<_>>();
            drop(prepared);
            yield QueryEvent::Prepared;
            if !columns.is_empty() { yield QueryEvent::Columns(columns); }
            let stream = self.client.simple_query_raw(sql).await.map_err(|e| query_failure(e, Some(sql)))?;
            let mut stream = std::pin::pin!(stream);
            let mut notices_open = true;
            // After a cell this side cannot read, the rest of the reply is still read, because a
            // server error later in it is what decides the transaction's state.
            let mut unread = None;
            loop {
                let message = tokio::select! {
                    biased;
                    n = self.notices.recv(), if notices_open => {
                        if let Some(n) = n { yield QueryEvent::Notice(notice(&n)); }
                        else { notices_open = false; }
                        continue;
                    }
                    message = stream.next() => message,
                };
                for n in self.drain_notices() { yield QueryEvent::Notice(notice(&n)); }
                let Some(message) = message else { break; };
                match message.map_err(|e| query_failure(e, Some(sql)))? {
                    SimpleQueryMessage::Row(_) if unread.is_some() => {}
                    SimpleQueryMessage::Row(row) => match (0..row.len()).map(|c| row.try_get(c).map(|cell| cell.map(str::to_owned)).map_err(|_| not_utf8(row.columns()[c].name()))).collect() {
                        Ok(cells) => yield QueryEvent::Row(cells),
                        Err(error) => unread = Some(error),
                    },
                    SimpleQueryMessage::CommandComplete(_) if unread.is_some() => {}
                    SimpleQueryMessage::CommandComplete(n) => yield QueryEvent::Complete { affected_rows: Some(n) },
                    _ => {}
                }
            }
            if let Some(error) = unread {
                yield QueryEvent::ServerDone;
                Err(error)?;
            }
        })
    }
    pub(super) async fn list_databases(&self) -> Result<(Vec<crate::connection::DatabaseInfo>, String), QueryError> {
        catalog::list_databases(&self.client).await.map_err(|e| query_failure(e, None))
    }
    pub(super) async fn count_schemas(&self) -> Result<u32, QueryError> {
        catalog::count_schemas(&self.client).await.map_err(|e| query_failure(e, None))
    }
    pub(super) async fn catalog(&self, db: DatabaseRef, version: u64, extra: &[String]) -> Result<Catalog, QueryError> {
        catalog::load(&self.client, db, version, extra).await.map_err(|e| query_failure(e, None))
    }
    pub(super) async fn detail(
        &self,
        db: DatabaseRef,
        oid: u32,
        version: u64,
    ) -> Result<Option<RelationDetail>, QueryError> {
        detail::load(&self.client, db, oid, version).await.map_err(|e| query_failure(e, None))
    }
    pub(super) async fn script(&mut self, target: &ScriptTarget) -> Result<ObjectScript, DetailError> {
        source::load(&mut self.client, target).await
    }
    pub(super) async fn count(&self, sql: &str) -> Result<u64, QueryError> {
        let row = self.client.query_one(sql, &[]).await.map_err(|e| query_failure(e, Some(sql)))?;
        u64::try_from(row.get::<_, i64>(0)).map_err(|_| super::error("Negative row count"))
    }
    pub(super) async fn has_writes(&self) -> bool {
        match self.client.simple_query("SELECT pg_current_xact_id_if_assigned() IS NOT NULL").await {
            Ok(msgs) => msgs.iter().any(|m| matches!(m, SimpleQueryMessage::Row(r) if r.get(0) == Some("t"))),
            Err(_) => true,
        }
    }
}

impl PgSession {
    pub(super) fn retire(&mut self, reason: String) {
        self.retired = Some(reason);
    }
}
