use std::collections::VecDeque;

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::model::{
    ConnectionConfig, ConsoleConfigView, DatabaseKind, Environment, ExecutionUnitKind, TxnState, WriteConfirmation,
    WriteDigest, WriteList,
};
use crate::sql::{self, StmtKind};
use crate::text;

const SHOWN: usize = 3;
const SHOWN_CHARS: usize = 2000;
const PROVEN: usize = 32;

#[derive(Serialize)]
pub struct Target<'a> {
    pub saved: &'a ConnectionConfig,
    pub opened: &'a ConnectionConfig,
    pub console: &'a ConsoleConfigView,
    pub txn: TxnState,
    pub unasked: &'a WriteList,
}

#[derive(Debug)]
pub struct WritesChecked {
    on_production: bool,
    consumes_hold: bool,
}

impl WritesChecked {
    pub fn writes_stay_unasked(&self) -> bool {
        !self.on_production
    }

    pub fn mark_held_writes_asked(&self, unasked: &mut WriteList) {
        if self.on_production && self.consumes_hold {
            *unasked = WriteList::default();
        }
    }
}

/// Which statements of a run go inside a server-enforced read-only fence. Only `check_run` builds one.
#[derive(Debug)]
pub struct ReadOnly(Box<[bool]>);

impl ReadOnly {
    pub fn covers(&self, index: usize) -> bool {
        self.0.get(index).copied().unwrap_or(false)
    }
}

/// Statement texts a read-only fence on this tab refused as writes, newest last.
#[derive(Debug, Default)]
pub struct Proven(VecDeque<String>);

impl Proven {
    pub fn add(&mut self, sql: &str) {
        if self.contains(sql) {
            return;
        }
        if self.0.len() == PROVEN {
            self.0.pop_front();
        }
        self.0.push_back(sql.trim().to_owned());
    }

    pub fn contains(&self, sql: &str) -> bool {
        let sql = sql.trim();
        self.0.iter().any(|proven| proven == sql)
    }
}

impl WriteList {
    pub fn add(&mut self, sql: &str) {
        if self.statements.len() < SHOWN {
            self.statements.push(shown(sql));
        }
        self.count = self.count.saturating_add(1);
    }
}

impl Target<'_> {
    pub(crate) fn on_production(&self) -> bool {
        [self.saved, self.opened].iter().any(|c| c.environment == Some(Environment::Production))
    }

    fn digest(&self, work: impl Serialize) -> WriteDigest {
        let mut hasher = Sha256::new();
        serde_json::to_writer(&mut hasher, &(self, work)).expect("model types serialize to JSON");
        WriteDigest::new(format!("{:x}", hasher.finalize()))
    }
}

pub(crate) fn admit(target: &Target, consumes_hold: bool) -> WritesChecked {
    WritesChecked { on_production: target.on_production(), consumes_hold }
}

pub(crate) fn ask(
    target: &Target,
    shown: &[&str],
    bound: &[&str],
    confirmed: Option<&WriteDigest>,
) -> Result<WritesChecked, WriteConfirmation> {
    let digest = target.digest(bound);
    if confirmed == Some(&digest) {
        return Ok(WritesChecked { on_production: target.on_production(), consumes_hold: true });
    }
    let kind = target.saved.database_kind;
    let mut sends = WriteList::default();
    shown.iter().filter(|sql| text::is_write(sql, kind)).for_each(|sql| sends.add(sql));
    if sends.count == 0 {
        shown.iter().for_each(|sql| sends.add(sql));
    }
    Err(confirmation(target, sends, digest))
}

pub fn check(
    target: &Target,
    statements: &[&str],
    confirmed: Option<&WriteDigest>,
) -> Result<WritesChecked, WriteConfirmation> {
    check_run(target, statements, &Proven::default(), confirmed).map(|(checked, _)| checked)
}

/// Held writes make any work ask, because a batch can commit them in forms the classifier cannot see, such as
/// `IF @@TRANCOUNT > 0 COMMIT` or `PREPARE TRANSACTION` followed by `COMMIT PREPARED` from another session.
///
/// On Production PostgreSQL every other query runs read-only, so a write the classifier misses, such as a
/// function with side effects, fails instead of writing. The digest binds which statements count as writes,
/// so a digest issued before a statement was proven a write cannot run it unasked.
pub fn check_run(
    target: &Target,
    statements: &[&str],
    proven: &Proven,
    confirmed: Option<&WriteDigest>,
) -> Result<(WritesChecked, ReadOnly), WriteConfirmation> {
    let kind = target.saved.database_kind;
    let on_production = target.on_production();
    let writes: Vec<bool> = statements.iter().map(|sql| text::is_write(sql, kind) || proven.contains(sql)).collect();
    let fenced = statements
        .iter()
        .zip(&writes)
        .map(|(sql, &write)| on_production && kind == DatabaseKind::Postgres && !write && reads(sql))
        .collect();
    let read_only = ReadOnly(fenced);
    if !on_production {
        return Ok((admit(target, true), read_only));
    }
    let mut sends = WriteList::default();
    statements.iter().zip(&writes).filter(|(_, write)| **write).for_each(|(sql, _)| sends.add(sql));
    let asked = WritesChecked { on_production: true, consumes_hold: true };
    if sends.count == 0 && target.unasked.count == 0 {
        return Ok((asked, read_only));
    }
    let digest = target.digest((statements, &writes));
    if confirmed == Some(&digest) {
        return Ok((asked, read_only));
    }
    Err(confirmation(target, sends, digest))
}

/// A query, or the `COPY ... TO` form, since `COPY ... FROM` already counts as a write. Other statements change
/// session state that the fence's `ROLLBACK TO` would silently undo.
fn reads(statement: &str) -> bool {
    match sql::classify(statement) {
        StmtKind::Query => true,
        StmtKind::Other(verb) => verb == "COPY",
        _ => false,
    }
}

pub fn check_import(
    target: &Target,
    copy: &str,
    statements: &[&str],
    descriptor: &str,
    confirmed: Option<&WriteDigest>,
) -> Result<WritesChecked, WriteConfirmation> {
    if !target.on_production() {
        return Ok(admit(target, true));
    }
    let bound: Vec<&str> = statements.iter().copied().chain(std::iter::once(descriptor)).collect();
    let digest = target.digest(&bound);
    if confirmed == Some(&digest) {
        return Ok(admit(target, true));
    }
    let mut sends = WriteList::default();
    sends.add(copy);
    Err(confirmation(target, sends, digest))
}

fn confirmation(target: &Target, sends: WriteList, digest: WriteDigest) -> WriteConfirmation {
    let kind = target.saved.database_kind;
    WriteConfirmation {
        connection: target.saved.name.clone(),
        database: target.console.database.clone(),
        unit: match kind {
            DatabaseKind::Postgres => ExecutionUnitKind::Statement,
            DatabaseKind::SqlServer => ExecutionUnitKind::Batch,
        },
        sends,
        holds: target.unasked.clone(),
        digest,
        on_production: target.on_production(),
    }
}

fn shown(sql: &str) -> String {
    let sql = sql.trim();
    match sql.char_indices().nth(SHOWN_CHARS) {
        Some((end, _)) => format!("{}…", &sql[..end]),
        None => sql.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ConnectionId, SslMode, TxnMode};
    use uuid::Uuid;

    static NONE: WriteList = WriteList { count: 0, statements: Vec::new() };

    fn connection(environment: Option<Environment>) -> ConnectionConfig {
        ConnectionConfig {
            database_kind: DatabaseKind::Postgres,
            id: ConnectionId(Uuid::from_u128(1)),
            name: "prod".into(),
            host: "db".into(),
            port: 5432,
            database: "shop".into(),
            user: "app".into(),
            ssl_mode: SslMode::Prefer,
            ca_cert_path: None,
            has_password: false,
            color: None,
            ssh: None,
            environment,
            sql_server_auth: None,
        }
    }

    fn console(mode: TxnMode) -> ConsoleConfigView {
        ConsoleConfigView {
            connection_id: ConnectionId(Uuid::from_u128(1)),
            database: "shop".into(),
            schema: None,
            mode,
        }
    }

    fn on<'a>(connection: &'a ConnectionConfig, console: &'a ConsoleConfigView) -> Target<'a> {
        Target { saved: connection, opened: connection, console, txn: TxnState::Idle, unasked: &NONE }
    }

    fn list(statements: &[&str]) -> WriteList {
        let mut list = WriteList::default();
        statements.iter().for_each(|sql| list.add(sql));
        list
    }

    fn shape(list: &WriteList) -> (u32, Vec<&str>) {
        (list.count, list.statements.iter().map(String::as_str).collect())
    }

    const WORK: &[&str] = &["select 1", " insert into t values (1) ", "delete from t"];

    #[test]
    fn only_a_write_on_production_asks() {
        let production = connection(Some(Environment::Production));
        let auto = console(TxnMode::AutoCommit);
        assert!(check(&on(&production, &auto), &["select 1", "begin", "commit"], None).is_ok());
        for environment in [None, Some(Environment::Local), Some(Environment::Staging)] {
            assert!(
                check(&on(&connection(environment), &auto), WORK, None).unwrap().writes_stay_unasked(),
                "{environment:?}"
            );
        }
        let asked = check(&on(&production, &auto), WORK, None).unwrap_err();
        assert_eq!(
            (asked.connection.as_str(), asked.database.as_str(), asked.unit),
            ("prod", "shop", ExecutionUnitKind::Statement)
        );
        assert_eq!(
            (shape(&asked.sends), shape(&asked.holds)),
            ((2, vec!["insert into t values (1)", "delete from t"]), (0, vec![]))
        );
        assert!(
            !check(&on(&production, &auto), WORK, Some(&asked.digest)).unwrap().writes_stay_unasked(),
            "a confirmed write is not unasked"
        );
    }

    #[test]
    fn a_session_opened_on_production_asks_until_it_closes() {
        let auto = console(TxnMode::AutoCommit);
        let production = connection(Some(Environment::Production));
        let local = connection(Some(Environment::Local));
        let session = |saved, opened| Target { saved, opened, console: &auto, txn: TxnState::Idle, unasked: &NONE };
        assert!(check(&session(&local, &production), WORK, None).is_err(), "relabelled on the same server");
        let moved = ConnectionConfig { host: "elsewhere".into(), ..local.clone() };
        assert!(check(&session(&moved, &production), WORK, None).is_err(), "relabelled and moved");
        assert!(
            check(&session(&production, &local), WORK, None).is_err(),
            "marked Production after the session opened"
        );
        assert!(check(&session(&local, &moved), WORK, None).is_ok());
    }

    #[test]
    fn the_digest_holds_only_for_the_same_work_and_target() {
        let production = connection(Some(Environment::Production));
        let auto = console(TxnMode::AutoCommit);
        let digest = check(&on(&production, &auto), WORK, None).unwrap_err().digest;
        assert!(check(&on(&production, &auto), WORK, Some(&digest)).is_ok());

        let edited = ["select 1", " insert into t values (2) ", "delete from t"];
        assert!(check(&on(&production, &auto), &edited, Some(&digest)).is_err(), "a statement that writes changed");
        let read_changed = ["select 2", " insert into t values (1) ", "delete from t"];
        assert!(
            check(&on(&production, &auto), &read_changed, Some(&digest)).is_err(),
            "a statement that reads changed"
        );
        let manual = console(TxnMode::Manual);
        assert!(check(&on(&production, &manual), WORK, Some(&digest)).is_err(), "the mode changed");
        let other_db = ConsoleConfigView { database: "reporting".into(), ..auto.clone() };
        assert!(check(&on(&production, &other_db), WORK, Some(&digest)).is_err(), "the database changed");
        let other_schema = ConsoleConfigView { schema: Some("audit".into()), ..auto.clone() };
        assert!(check(&on(&production, &other_schema), WORK, Some(&digest)).is_err(), "the schema changed");
        let other_host = ConnectionConfig { host: "db2".into(), ..production.clone() };
        assert!(check(&on(&other_host, &auto), WORK, Some(&digest)).is_err(), "the connection changed");
        let reopened = Target { opened: &other_host, ..on(&production, &auto) };
        assert!(check(&reopened, WORK, Some(&digest)).is_err(), "the session reached another server");
        let open = Target { txn: TxnState::Active, ..on(&production, &auto) };
        assert!(check(&open, WORK, Some(&digest)).is_err(), "the transaction state changed");
        let held = list(&["update t set n = 1"]);
        assert!(
            check(&Target { unasked: &held, ..on(&production, &auto) }, WORK, Some(&digest)).is_err(),
            "the held writes changed"
        );
    }

    #[test]
    fn any_work_on_production_asks_while_the_transaction_holds_unasked_writes() {
        let manual = console(TxnMode::Manual);
        let production = connection(Some(Environment::Production));
        let staging = connection(Some(Environment::Staging));
        let held = list(&["insert into t values (1)", "delete from t"]);
        let open =
            |saved, unasked| Target { saved, opened: &staging, console: &manual, txn: TxnState::Active, unasked };

        assert!(check(&open(&staging, &held), &["commit"], None).is_ok(), "off Production nothing asks");
        assert!(check(&open(&production, &NONE), &["select 1", "commit"], None).is_ok(), "nothing ran unasked");
        for work in
            [&["COMMIT"][..], &["select 1"], &["IF @@TRANCOUNT > 0 COMMIT TRANSACTION"], &["prepare transaction 'p'"]]
        {
            let asked = check(&open(&production, &held), work, None).unwrap_err();
            assert_eq!(shape(&asked.holds), (2, vec!["insert into t values (1)", "delete from t"]), "{work:?}");
        }
        let asked = check(&open(&production, &held), WORK, None).unwrap_err();
        assert_eq!((asked.sends.count, asked.holds.count), (2, 2));

        let mut unasked = held.clone();
        check(&open(&production, &held), WORK, Some(&asked.digest)).unwrap().mark_held_writes_asked(&mut unasked);
        assert_eq!(unasked.count, 0, "the confirmed question listed them");
        let mut unasked = held.clone();
        check(&open(&staging, &held), WORK, None).unwrap().mark_held_writes_asked(&mut unasked);
        assert_eq!(unasked.count, 2, "a check off Production keeps them");
    }

    #[test]
    fn a_run_that_does_not_consume_holds_leaves_them() {
        let production = connection(Some(Environment::Production));
        let auto = console(TxnMode::AutoCommit);
        let held = list(&["update t set n = 1"]);
        let mut unasked = held.clone();
        let target = Target { unasked: &held, ..on(&production, &auto) };
        admit(&target, false).mark_held_writes_asked(&mut unasked);
        assert_eq!(unasked.count, 1);
        admit(&target, true).mark_held_writes_asked(&mut unasked);
        assert_eq!(unasked.count, 0);
    }

    fn fenced(target: &Target, statements: &[&str]) -> Vec<bool> {
        let (_, read_only) = check_run(target, statements, &Proven::default(), None).unwrap();
        (0..statements.len()).map(|i| read_only.covers(i)).collect()
    }

    #[test]
    fn only_reads_on_production_postgres_run_read_only() {
        let production = connection(Some(Environment::Production));
        let auto = console(TxnMode::AutoCommit);
        let reads = [
            "select f()",
            "with x as (select 1) select * from x",
            "show work_mem",
            "copy t to '/tmp/t.csv'",
            "copy (select f()) to program 'cat'",
        ];
        assert_eq!(fenced(&on(&production, &auto), &reads), [true; 5]);
        let others = [
            "begin",
            "commit",
            "rollback to s",
            "savepoint s",
            "release s",
            "set work_mem = '7MB'",
            "reset all",
            "declare c cursor for select 1",
            "close c",
            "move c",
            "listen x",
            "notify x",
            "prepare p as select 1",
            "deallocate p",
            "lock table t",
            "discard plans",
        ];
        assert_eq!(fenced(&on(&production, &auto), &others), [false; 16]);
        let asked = check(&on(&production, &auto), WORK, None).unwrap_err();
        let (_, read_only) = check_run(&on(&production, &auto), WORK, &Proven::default(), Some(&asked.digest)).unwrap();
        assert_eq!(
            (0..3).map(|i| read_only.covers(i)).collect::<Vec<_>>(),
            [true, false, false],
            "confirmed writes run as they are"
        );

        for environment in [None, Some(Environment::Local), Some(Environment::Staging)] {
            assert_eq!(fenced(&on(&connection(environment), &auto), &reads), [false; 5], "{environment:?}");
        }
        let sql_server = ConnectionConfig { database_kind: DatabaseKind::SqlServer, ..production.clone() };
        assert_eq!(fenced(&on(&sql_server, &auto), &["SELECT 1", "SELECT dbo.f()"]), [false; 2]);
    }

    #[test]
    fn a_proven_read_asks_and_its_old_digest_stops_working() {
        let production = connection(Some(Environment::Production));
        let auto = console(TxnMode::AutoCommit);
        let work = ["insert into t values (1)", "select purge()"];
        let mut proven = Proven::default();
        let before = check_run(&on(&production, &auto), &work, &proven, None).unwrap_err();
        assert_eq!(shape(&before.sends), (1, vec!["insert into t values (1)"]));

        proven.add("  select purge()\n");
        let after = check_run(&on(&production, &auto), &work, &proven, Some(&before.digest)).unwrap_err();
        assert_eq!(
            shape(&after.sends),
            (2, vec!["insert into t values (1)", "select purge()"]),
            "the proven read is listed"
        );
        assert_ne!(after.digest, before.digest);
        let (_, read_only) = check_run(&on(&production, &auto), &work, &proven, Some(&after.digest)).unwrap();
        assert!(!read_only.covers(1), "a confirmed proven write runs unfenced");
        assert!(
            check_run(&on(&production, &auto), &["select purge()"], &proven, None).is_err(),
            "it asks on its own too"
        );
        assert!(
            check_run(&on(&connection(Some(Environment::Local)), &auto), &["select purge()"], &proven, None).is_ok(),
            "off Production nothing asks"
        );
    }

    #[test]
    fn proven_keeps_the_newest_distinct_texts() {
        let mut proven = Proven::default();
        proven.add("select f(0)");
        proven.add(" select f(0)\n");
        assert_eq!(proven.0.len(), 1, "a repeat adds nothing");
        (1..=PROVEN).for_each(|i| proven.add(&format!("select f({i})")));
        assert!(!proven.contains("select f(0)"), "the oldest fell out");
        assert!(proven.contains(" select f(1) ") && proven.contains(&format!("select f({PROVEN})")));
    }

    #[test]
    fn a_write_list_counts_every_write_and_keeps_the_first_three() {
        let four = list(&["insert into t values (1)", "delete from t", "update t set n = 2", "truncate t"]);
        assert_eq!(shape(&four), (4, vec!["insert into t values (1)", "delete from t", "update t set n = 2"]));
        let long = format!("insert into t values ('{}')", "x".repeat(3 * SHOWN_CHARS));
        let cut = &list(&[&long]).statements[0];
        assert_eq!(cut.chars().count(), SHOWN_CHARS + 1);
        assert!(cut.ends_with('…'));
    }

    #[test]
    fn a_sql_server_write_is_a_batch() {
        let production =
            ConnectionConfig { database_kind: DatabaseKind::SqlServer, ..connection(Some(Environment::Production)) };
        let asked =
            check(&on(&production, &console(TxnMode::AutoCommit)), &["SELECT 1", "EXEC dbo.purge"], None).unwrap_err();
        assert_eq!((asked.unit, shape(&asked.sends)), (ExecutionUnitKind::Batch, (1, vec!["EXEC dbo.purge"])));
    }
}
