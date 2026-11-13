mod catalog;
mod detail;
pub(super) mod import;
#[cfg(all(test, feature = "mssql-tests"))]
mod import_tests;
mod map;
mod source;
mod values;

use super::stream::DbStream;
use super::{QueryEvent, QueryStream, error};
use crate::connection::{CONNECT_TIMEOUT, ConnectParams};
use crate::entra::AzCli;
use crate::model::*;
use futures_util::{StreamExt, TryStreamExt};
use std::time::Duration;
use tiberius::{AuthMethod, Client, Config, EncryptionLevel, QueryItem};
use tokio::sync::watch;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

type TdsClient = Client<Compat<DbStream>>;
const ATTENTION_TIMEOUT: Duration = Duration::from_millis(300);
const DEADLOCK_VICTIM: u32 = 1205;
const DEADLOCK_ATTEMPTS: u32 = 5;
const DEADLOCK_BACKOFF: Duration = Duration::from_millis(100);

fn server_error_detail(code: u32) -> String {
    format!("SQL Server error {code},")
}

fn deadlock_victim(e: &QueryError) -> bool {
    e.detail.as_deref().is_some_and(|d| d.starts_with(&server_error_detail(DEADLOCK_VICTIM)))
}

pub(super) struct MsSession {
    client: Option<TdsClient>,
    pub backend_pid: i32,
    pub server: ServerInfo,
    database: String,
    txn: TxnState,
    cancel: watch::Sender<u64>,
    closed: watch::Sender<Option<String>>,
    metadata: bool,
    script_features: Option<source::Features>,
}

#[derive(Clone)]
pub(super) struct MsCancel(watch::Sender<u64>);
impl MsCancel {
    pub async fn cancel(&self) {
        self.0.send_modify(|revision| *revision = revision.wrapping_add(1));
    }
}

struct Lease<'a> {
    owner: &'a mut MsSession,
    client: Option<TdsClient>,
}
impl Lease<'_> {
    fn client(&mut self) -> &mut TdsClient {
        self.client.as_mut().expect("lease owns the client")
    }
    fn restore(mut self, txn: TxnState) {
        self.owner.txn = txn;
        self.owner.client = self.client.take();
    }
    fn retire(&mut self, reason: String) {
        self.owner.closed.send_replace(Some(reason));
        self.client.take();
    }
}
impl Drop for Lease<'_> {
    fn drop(&mut self) {
        if let Some(mut client) = self.client.take() {
            self.owner.closed.send_replace(Some(
                "SQL Server operation was abandoned; the session and any open transaction were discarded.".into(),
            ));
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                let closed = self.owner.closed.clone();
                runtime.spawn(async move {
                    let _ = tokio::time::timeout(ATTENTION_TIMEOUT, client.cancel_query()).await;
                    drop(client);
                    drop(closed);
                });
            }
        }
    }
}

pub(super) fn query_error(e: tiberius::error::Error) -> QueryError {
    match e {
        tiberius::error::Error::Server(e) => QueryError {
            severity: if e.class() >= 20 { "FATAL" } else { "ERROR" }.into(),
            sqlstate: None,
            message: e.message().to_owned(),
            detail: Some(format!("{} state {}, line {}", server_error_detail(e.code()), e.state(), e.line())),
            hint: None,
            context: (!e.procedure().is_empty()).then(|| e.procedure().to_owned()),
            position: None,
        },
        e => QueryError { severity: "FATAL".into(), ..error(e.to_string()) },
    }
}

fn config(p: &ConnectParams) -> Result<Config, ConnectError> {
    let mut config = Config::new();
    config.host(&p.host);
    config.port(p.port);
    config.database(&p.database);
    config.application_name("L1 Data");
    config.command_timeout(None);
    config.authentication(AuthMethod::sql_server(&p.user, p.password.as_deref().unwrap_or("")));
    config.encryption(EncryptionLevel::Required);
    match (p.ssl_mode, &p.ca_cert_path) {
        (SslMode::Require, None) if p.entra.is_some() => {
            return Err(ConnectError::Tls { message: "Choose Verify full for Microsoft Entra ID. Its token works on every Azure SQL database you can reach, so it goes only to a verified server.".into() });
        }
        (SslMode::Require, None) => config.trust_cert(),
        (SslMode::VerifyFull, None) => {}
        (SslMode::VerifyFull, Some(_)) => {
            return Err(ConnectError::Tls { message: "Custom SQL Server CA files are not supported by this adapter yet. Use system-trusted certificates with VerifyFull.".into() });
        }
        _ => {
            return Err(ConnectError::Tls {
                message:
                    "SQL Server supports VerifyFull, or Require for encrypted connections without certificate verification. Other SSL modes and CA combinations are unsupported."
                        .into(),
            });
        }
    }
    Ok(config)
}

const LOGIN_FAILED: u32 = 18456;
const UNTRUSTED_DOMAIN_LOGIN: u32 = 18452;

const CERTIFICATE_REJECTED: &str = "the server's certificate was rejected during the TLS handshake: ";
/// The vendored driver's advice for developers, such as `Config::trust_cert()`, starts here. It
/// names settings the app does not have.
const DEVELOPER_ADVICE: &str = " To connect anyway";

fn tls_failure(message: String, entra: bool) -> ConnectError {
    let Some(rest) = message.strip_prefix(CERTIFICATE_REJECTED) else {
        return ConnectError::Tls { message: format!("TLS failed: {message}") };
    };
    let reason = rest.split_once(DEVELOPER_ADVICE).map_or(rest, |(reason, _)| reason).trim_end_matches('.');
    let message = if entra {
        format!(
            "The server's certificate could not be verified, so the Microsoft Entra ID token was not sent. Entra ID sign-in needs a server with a publicly trusted certificate, such as Azure SQL. Check the host name, or ask the server's administrator for a publicly trusted certificate. Reason: {reason}."
        )
    } else {
        format!(
            "The server's certificate could not be verified. Check the host name, give the server a certificate this computer trusts, or choose Require under TLS to connect without checking it. Reason: {reason}."
        )
    };
    ConnectError::Tls { message }
}

async fn login_refused(code: u32, message: &str, entra: Option<(&AzCli, &str)>) -> Option<ConnectError> {
    if code != LOGIN_FAILED && !(entra.is_some() && code == UNTRUSTED_DOMAIN_LOGIN) {
        return None;
    }
    if let Some((az, token)) = entra {
        az.forget(token).await;
    }
    Some(ConnectError::Auth { message: message.to_owned() })
}

impl MsSession {
    pub(super) async fn open(p: &ConnectParams) -> Result<Self, ConnectError> {
        let mut cfg = config(p)?;
        let stream = p.dial().await?;
        let token = match &p.entra {
            Some(az) => Some(az.token().await?),
            None => None,
        };
        if let Some(token) = &token {
            cfg.authentication(AuthMethod::aad_token(token));
        }
        // The handshake's future is tens of kilobytes in a debug build. On the heap it leaves every caller its stack.
        let connection = Box::pin(Client::connect(cfg, stream.compat_write()));
        let connected = tokio::time::timeout(CONNECT_TIMEOUT, connection)
            .await
            .map_err(|_| ConnectError::Network { message: "SQL Server connection timed out".into() })?;
        let client = match connected {
            Ok(client) => client,
            Err(tiberius::error::Error::Tls(message)) => return Err(tls_failure(message, token.is_some())),
            Err(tiberius::error::Error::Server(e)) => {
                let entra = p.entra.as_deref().zip(token.as_deref());
                return Err(login_refused(e.code(), e.message(), entra)
                    .await
                    .unwrap_or_else(|| ConnectError::Server(query_error(tiberius::error::Error::Server(e)))));
            }
            Err(e) => return Err(ConnectError::Server(query_error(e))),
        };
        let state = client.session_state().map_err(|e| ConnectError::Server(query_error(e)))?;
        let backend_pid = i32::from(state.server_process_id);
        let (major, minor, build) = state.server_version;
        let version = format!("{major}.{minor}.{build}");
        let version_num = u32::from(major) * 10000;
        let database = state.database.to_owned();
        let txn = transaction(state.has_transaction);
        let (cancel, _) = watch::channel(0);
        let (closed, _) = watch::channel(None);
        Ok(Self {
            client: Some(client),
            backend_pid,
            server: ServerInfo { version, version_num, encoding: "Unicode".into() },
            database,
            txn,
            cancel,
            closed,
            metadata: false,
            script_features: None,
        })
    }
    pub(super) async fn open_metadata(p: &ConnectParams) -> Result<Self, ConnectError> {
        let mut session = Self::open(p).await?;
        session.batch("SET DEADLOCK_PRIORITY LOW").await.map_err(ConnectError::Server)?;
        session.metadata = true;
        Ok(session)
    }
    pub(super) fn txn(&self) -> (TxnState, Option<u32>) {
        (self.txn, (self.txn == TxnState::Idle).then_some(0))
    }
    pub(super) fn cancel_handle(&self) -> MsCancel {
        MsCancel(self.cancel.clone())
    }
    pub(super) fn closed_reason(&self) -> Option<String> {
        self.closed.borrow().clone().or_else(|| (*self.cancel.borrow() != 0).then(|| "Cancelled SQL Server session was retired; earlier commits remain committed; the last transaction outcome may be unknown.".into()))
    }
    pub(super) fn closed_watch(&self) -> watch::Receiver<Option<String>> {
        self.closed.subscribe()
    }

    pub(super) fn query<'a>(&'a mut self, sql: &'a str) -> QueryStream<'a> {
        Box::pin(async_stream::stream! {
            let Some(client) = self.client.take() else { yield Err(error(self.closed_reason().unwrap_or_else(|| "SQL Server session is retired".into()))); return; };
            let database = self.database.clone();
            let mut cancel = self.cancel.subscribe();
            let mut lease = Lease { owner: self, client: Some(client) };
            if *cancel.borrow() != 0 {
                lease.retire("Cancelled before SQL Server execution. The session was retired.".into());
                yield Err(error("Cancelled"));
                return;
            }
            let mut cancelled = false;
            let mut failed = None;
            let mut completed = false;
            let mut fatal = None;
            {
                let events = native_query(lease.client(), sql);
                let mut events = std::pin::pin!(events);
                loop {
                    let next = tokio::select! {
                        biased;
                        _ = cancel.changed() => { cancelled = true; break; },
                        next = events.next() => next,
                    };
                    match next {
                        Some(Ok(event)) => {
                            completed |= matches!(event, QueryEvent::Complete { .. } | QueryEvent::ServerError(_));
                            if let QueryEvent::ServerError(e) = &event && e.severity == "FATAL" { fatal.get_or_insert_with(|| e.message.clone()); }
                            yield Ok(event);
                        }
                        Some(Err(e)) => { failed = Some(e); break; }
                        None => break,
                    }
                }
            }
            if cancelled {
                let _ = tokio::time::timeout(ATTENTION_TIMEOUT, lease.client().cancel_query()).await;
                lease.retire("Cancelled SQL Server operation. The session was retired and earlier commits remain committed; the last transaction outcome may be unknown.".into());
                yield Err(error("Cancelled"));
                return;
            }
            if failed.as_ref().is_some_and(|e| e.severity == "FATAL") {
                let e = failed.unwrap();
                lease.retire(e.message.clone());
                yield Err(e);
                return;
            }
            if let Some(message) = fatal {
                lease.retire(message);
                return;
            }
            let observed = if *cancel.borrow() != 0 {
                Err(error("Cancelled after SQL Server execution"))
            } else {
                observe(lease.client(), &database)
            };
            match observed {
                Ok(txn) => lease.restore(txn),
                Err(e) => {
                    let _ = tokio::time::timeout(ATTENTION_TIMEOUT, lease.client().cancel_query()).await;
                    lease.retire(e.message.clone());
                    if failed.is_none() { failed = Some(e); }
                },
            }
            if let Some(e) = failed { yield Err(e); }
            else if !completed { yield Ok(QueryEvent::Complete { affected_rows: None }); }
        })
    }

    pub(super) async fn rows(&mut self, sql: &str) -> Result<Vec<Vec<Option<String>>>, QueryError> {
        Ok(self.results(sql).await?.concat())
    }
    /// The rows of each result set a batch returns, in order.
    pub(super) async fn results(&mut self, sql: &str) -> Result<Vec<Vec<Vec<Option<String>>>>, QueryError> {
        let mut attempt = 1;
        loop {
            match self.read_results(sql).await {
                Err(e) if self.metadata && deadlock_victim(&e) && attempt < DEADLOCK_ATTEMPTS => {
                    tokio::time::sleep(DEADLOCK_BACKOFF * attempt).await;
                    attempt += 1;
                }
                result => return result,
            }
        }
    }
    async fn read_results(&mut self, sql: &str) -> Result<Vec<Vec<Vec<Option<String>>>>, QueryError> {
        let mut stream = self.query(sql);
        let mut sets: Vec<Vec<Vec<Option<String>>>> = Vec::new();
        let mut failed = None;
        while let Some(event) = stream.next().await {
            match event? {
                QueryEvent::Columns(_) => sets.push(Vec::new()),
                QueryEvent::Row(row) => {
                    sets.last_mut().ok_or_else(|| error("SQL Server sent a row before its columns"))?.push(row)
                }
                QueryEvent::ServerError(e) => {
                    failed.get_or_insert(e);
                }
                _ => {}
            }
        }
        failed.map_or(Ok(sets), Err)
    }
    pub(super) async fn batch(&mut self, sql: &str) -> Result<(), QueryError> {
        self.rows(sql).await.map(drop)
    }
    pub(super) async fn finish_transaction(&mut self, commit: bool) -> Result<(), QueryError> {
        self.batch(if commit {
            "WHILE @@TRANCOUNT > 0 COMMIT TRANSACTION"
        } else {
            "IF @@TRANCOUNT > 0 ROLLBACK TRANSACTION"
        })
        .await
    }
    pub(super) async fn count(&mut self, sql: &str) -> Result<u64, QueryError> {
        let rows = self.rows(sql).await?;
        number(rows.first().ok_or_else(|| error("Count returned no row"))?, 0)
    }
}

fn native_query<'a>(client: &'a mut TdsClient, sql: &'a str) -> QueryStream<'a> {
    Box::pin(async_stream::try_stream! {
        let stream = client.simple_query(sql).await.map_err(query_error)?;
        let mut stream = std::pin::pin!(stream);
        let mut in_result = false;
        let mut reported = false;
        let mut conversion_error = None;
        loop {
            let item = match stream.try_next().await {
                Ok(Some(item)) => item,
                Ok(None) => break,
                Err(tiberius::error::Error::Server(_)) if reported => break,
                Err(e) => Err(query_error(e))?,
            };
            if conversion_error.is_some() { continue; }
            match item {
                QueryItem::Metadata(meta) => {
                    in_result = true;
                    yield QueryEvent::Columns(meta.columns().iter().map(values::metadata).collect());
                }
                QueryItem::Row(row) => {
                    let cells = match row.cells().map(|(_, data)| values::text(data)).collect::<Result<Vec<_>, _>>() {
                        Ok(cells) => cells,
                        Err(e) => { conversion_error = Some(e); continue; }
                    };
                    yield QueryEvent::Row(cells);
                }
                QueryItem::Info(info) => yield QueryEvent::Notice(notice(&info)),
                QueryItem::Error(e) => {
                    in_result = false;
                    reported = true;
                    yield QueryEvent::ServerError(query_error(tiberius::error::Error::Server(e)));
                }
                QueryItem::Done(count) if in_result => {
                    in_result = false;
                    yield QueryEvent::Complete { affected_rows: count };
                }
                QueryItem::Done(Some(count)) => yield QueryEvent::Complete { affected_rows: Some(count) },
                QueryItem::Done(None) => {}
            }
        }
        if let Some(e) = conversion_error { Err(e)?; }
    })
}

fn notice(info: &tiberius::TokenInfo) -> OutputBody {
    let severity = if info.class() == 0 { "NOTICE" } else { "WARNING" };
    OutputBody::Notice { severity: severity.into(), message: info.message().to_owned(), detail: None }
}

fn observe(client: &TdsClient, database: &str) -> Result<TxnState, QueryError> {
    let state = client.session_state().map_err(query_error)?;
    let actual = state.database;
    if actual != database {
        return Err(error(format!(
            "Execution database changed to {actual}. The session was retired; select a database through the tab target."
        )));
    }
    Ok(transaction(state.has_transaction))
}

fn transaction(has_transaction: bool) -> TxnState {
    if has_transaction { TxnState::Open } else { TxnState::Idle }
}

fn required(row: &[Option<String>], i: usize) -> Result<&str, QueryError> {
    row.get(i).and_then(Option::as_deref).ok_or_else(|| error(format!("Missing SQL Server metadata field {i}")))
}
fn number<T: std::str::FromStr>(row: &[Option<String>], i: usize) -> Result<T, QueryError> {
    required(row, i)?.parse().map_err(|_| error(format!("Invalid SQL Server metadata field {i}")))
}

fn object_id(row: &[Option<String>], i: usize) -> Result<u32, QueryError> {
    Ok(u32::from_be_bytes(number::<i32>(row, i)?.to_be_bytes()))
}

fn native_object_id(opaque: u32) -> i32 {
    i32::from_be_bytes(opaque.to_be_bytes())
}

impl MsSession {
    pub(super) fn retire(&mut self, reason: String) {
        self.closed.send_replace(Some(reason));
        self.client.take();
    }
}

#[cfg(all(test, unix))]
mod login_tests {
    use super::*;
    use crate::shell_path::AgentPath;
    use std::os::unix::fs::PermissionsExt;

    struct FakeAz {
        az: AzCli,
        log: std::path::PathBuf,
        _dir: tempfile::TempDir,
    }

    impl FakeAz {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let (program, log) = (dir.path().join("az"), dir.path().join("az.log"));
            std::fs::write(
                &program,
                format!(
                    "#!/bin/sh\necho call >> '{}'\necho '{{\"accessToken\":\"token\",\"expires_on\":9999999999}}'\n",
                    log.display()
                ),
            )
            .unwrap();
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
            Self { az: AzCli::new(AgentPath::Fixed("".into()), Some(program)), log, _dir: dir }
        }

        async fn calls_after_another_token(&self) -> usize {
            assert_eq!(self.az.token().await.unwrap(), "token");
            std::fs::read_to_string(&self.log).unwrap().lines().count()
        }
    }

    #[tokio::test]
    async fn a_refused_entra_login_fails_as_auth_and_drops_the_token() {
        for code in [18456, 18452] {
            let fake = FakeAz::new();
            let token = fake.az.token().await.unwrap();
            let refused =
                login_refused(code, "Login failed for user '<token-identified principal>'.", Some((&fake.az, &token)))
                    .await;
            assert!(
                matches!(refused, Some(ConnectError::Auth { ref message }) if message.starts_with("Login failed")),
                "{code}: {refused:?}"
            );
            assert_eq!(
                fake.calls_after_another_token().await,
                2,
                "{code} drops the token, so the next sign-in runs az"
            );
        }
    }

    #[test]
    fn a_rejected_certificate_names_what_the_user_can_do_and_keeps_the_reason() {
        let rejected = "the server's certificate was rejected during the TLS handshake: invalid peer certificate: UnknownIssuer. To connect anyway you can: (1) trust a specific CA certificate with `Config::trust_cert_ca(path)`; (3) skip certificate validation entirely with `Config::trust_cert()`.".to_owned();
        for (entra, advice) in [(true, "publicly trusted certificate"), (false, "choose Require")] {
            let ConnectError::Tls { message } = tls_failure(rejected.clone(), entra) else {
                panic!("a TLS failure stays Tls")
            };
            assert!(
                message.starts_with("The server's certificate could not be verified") && message.contains(advice),
                "{message}"
            );
            assert!(message.ends_with("Reason: invalid peer certificate: UnknownIssuer."), "{message}");
            assert!(!message.contains("Config::") && !message.contains("trust_cert"), "{message}");
        }
        let old_cert = "the server's certificate was rejected during the TLS handshake: invalid peer certificate: Other(OtherError(UnsupportedCertVersion)). The SQL Server presented a certificate rustls considers too old (an outdated X.509 version), which frequently happens with the self-signed certificate SQL Server auto-generates. To connect anyway you can: (3) skip certificate validation entirely with `Config::trust_cert()`.".to_owned();
        let ConnectError::Tls { message } = tls_failure(old_cert, false) else { panic!("a TLS failure stays Tls") };
        assert!(message.ends_with("Reason: invalid peer certificate: Other(OtherError(UnsupportedCertVersion)). The SQL Server presented a certificate rustls considers too old (an outdated X.509 version), which frequently happens with the self-signed certificate SQL Server auto-generates."), "{message}");
        let ConnectError::Tls { message } = tls_failure("invalid dns name".into(), true) else {
            panic!("a TLS failure stays Tls")
        };
        assert_eq!(message, "TLS failed: invalid dns name");
    }

    #[tokio::test]
    async fn other_login_errors_keep_the_token_and_a_sql_login_keeps_trunks_mapping() {
        let fake = FakeAz::new();
        let token = fake.az.token().await.unwrap();
        assert!(
            login_refused(4060, "Cannot open database", Some((&fake.az, &token))).await.is_none(),
            "a missing database is not a refused token"
        );
        assert_eq!(fake.calls_after_another_token().await, 1);
        assert!(matches!(
            login_refused(18456, "Login failed for user 'sa'.", None).await,
            Some(ConnectError::Auth { .. })
        ));
        assert!(login_refused(18452, "untrusted domain", None).await.is_none());
    }
}

#[cfg(all(test, feature = "mssql-tests"))]
mod tests {
    use super::*;

    pub(super) fn params() -> ConnectParams {
        ConnectParams {
            database_kind: DatabaseKind::SqlServer,
            host: std::env::var("L1_DATA_TEST_MSSQL_HOST").unwrap_or_else(|_| "localhost".into()),
            port: std::env::var("L1_DATA_TEST_MSSQL_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(1434),
            database: "shop".into(),
            user: "sa".into(),
            password: Some(std::env::var("L1_DATA_TEST_MSSQL_PASSWORD").unwrap_or_else(|_| "L1Data_dev_Str0ng".into())),
            ssl_mode: SslMode::Require,
            ca_cert_path: None,
            via: None,
            entra: None,
        }
    }

    #[tokio::test]
    async fn a_login_alias_still_retires_a_session_that_changes_database() {
        let mut p = params();
        p.database = "SHOP".into();
        let mut session = MsSession::open(&p).await.unwrap();
        assert_eq!(session.database, "SHOP");
        let error = session.rows("USE reporting; SELECT DB_NAME()").await.unwrap_err();
        assert!(error.message.contains("Execution database changed to reporting"), "{}", error.message);
        assert!(session.closed_reason().is_some());
    }

    #[tokio::test]
    async fn a_batch_that_fails_partway_returns_the_server_error_not_fewer_result_sets() {
        let mut session = MsSession::open(&params()).await.unwrap();
        let error = session.results("SELECT 1; SELECT 1/0; SELECT 2").await.unwrap_err();
        assert!(error.message.contains("Divide by zero"), "{}", error.message);
        assert_eq!(
            session.results("SELECT 1; SELECT 2 WHERE 1=0").await.unwrap(),
            vec![vec![vec![Some("1".to_owned())]], vec![]]
        );
    }

    async fn stopped(observer: &mut MsSession, pid: i32) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let count = observer
                    .count(&format!("SELECT COUNT_BIG(*) FROM sys.dm_exec_requests WHERE session_id={pid}"))
                    .await
                    .unwrap();
                if count == 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("Abandoned server request did not stop");
    }

    #[tokio::test]
    async fn dropped_streams_and_metadata_futures_retire_the_client_and_stop_native_work() {
        let mut observer = MsSession::open(&params()).await.unwrap();
        let mut session = MsSession::open(&params()).await.unwrap();
        let pid = session.backend_pid;
        {
            let mut stream =
                session.query("SELECT 1; RAISERROR('flush',0,1) WITH NOWAIT; WAITFOR DELAY '00:00:30'; SELECT 2");
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    if matches!(stream.next().await.unwrap().unwrap(), QueryEvent::Row(_)) {
                        break;
                    }
                }
            })
            .await
            .expect("First row did not stream before WAITFOR");
        }
        assert!(session.closed_reason().is_some());
        assert!(session.client.is_none());
        stopped(&mut observer, pid).await;
        assert!(session.rows("SELECT 99").await.is_err());

        let mut session = MsSession::open(&params()).await.unwrap();
        let pid = session.backend_pid;
        assert!(
            tokio::time::timeout(Duration::from_millis(100), session.rows("WAITFOR DELAY '00:00:30'; SELECT 1"))
                .await
                .is_err()
        );
        assert!(session.client.is_none());
        assert!(session.closed_reason().is_some());
        stopped(&mut observer, pid).await;
    }

    #[tokio::test]
    async fn cancel_before_the_lazy_query_subscribes_is_sticky() {
        let mut session = MsSession::open(&params()).await.unwrap();
        let cancel = session.cancel_handle();
        let mut query = session.query("WAITFOR DELAY '00:00:30'; SELECT 1");
        cancel.cancel().await;
        let event = tokio::time::timeout(Duration::from_secs(1), query.next()).await.unwrap().unwrap();
        assert!(event.is_err());
        drop(query);
        assert!(session.client.is_none());
        assert!(session.closed_reason().is_some());
    }

    #[tokio::test]
    async fn changed_database_is_retired_and_unrepresentable_tls_settings_are_rejected() {
        let mut session = MsSession::open(&params()).await.unwrap();
        assert!(session.rows("USE reporting").await.is_err());
        assert!(session.client.is_none());
        assert!(session.closed_reason().unwrap().contains("Execution database changed"));
        let mut p = params();
        p.ssl_mode = SslMode::Disable;
        assert!(matches!(MsSession::open(&p).await, Err(ConnectError::Tls { .. })));
        p.ssl_mode = SslMode::VerifyFull;
        let Err(ConnectError::Tls { message }) = MsSession::open(&p).await else {
            panic!("the dev server's certificate is self-signed")
        };
        assert!(
            message.starts_with("The server's certificate could not be verified.")
                && message.contains("choose Require")
                && !message.contains("trust_cert"),
            "{message}"
        );
        p.ca_cert_path = Some("not-a-real-ca.pem".into());
        assert!(matches!(MsSession::open(&p).await, Err(ConnectError::Tls { .. })));
    }

    #[tokio::test]
    async fn failed_batch_still_consumes_database_environment_changes() {
        let mut session = MsSession::open(&params()).await.unwrap();
        assert!(session.rows("RAISERROR('expected failure',16,1); SELECT REPLICATE(CAST('x' AS varchar(max)),9000); USE reporting").await.is_err());
        assert!(session.client.is_none());
        assert!(session.closed_reason().unwrap().contains("Execution database changed to reporting"));
    }

    #[tokio::test]
    async fn closed_watch_outlives_detached_attention_and_client() {
        let mut observer = MsSession::open(&params()).await.unwrap();
        let mut session = MsSession::open(&params()).await.unwrap();
        let pid = session.backend_pid;
        let mut closed = session.closed_watch();
        {
            let mut stream = session.query("SELECT 1; RAISERROR('flush',0,1) WITH NOWAIT; WAITFOR DELAY '00:00:30'");
            tokio::time::timeout(Duration::from_secs(2), async {
                while !matches!(stream.next().await.unwrap().unwrap(), QueryEvent::Row(_)) {}
            })
            .await
            .unwrap();
        }
        closed.borrow_and_update();
        drop(session);
        assert!(closed.has_changed().is_ok(), "Detached cleanup must keep the client lifetime signal open");
        tokio::time::timeout(Duration::from_secs(1), async { while closed.changed().await.is_ok() {} })
            .await
            .expect("Detached Attention retained the client beyond its bound");
        stopped(&mut observer, pid).await;
    }
}
