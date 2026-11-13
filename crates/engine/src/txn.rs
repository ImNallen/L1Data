use crate::model::{DatabaseKind, TxnMode, TxnState};
use crate::sql::StmtKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionPlan {
    BeginIfIdle,
    Native,
}

pub fn validate_mode(database: DatabaseKind, mode: TxnMode) -> Result<(), String> {
    if database == DatabaseKind::SqlServer && mode == TxnMode::Manual {
        return Err("SQL Server Manual mode is unavailable because server-side code can end the app-managed transaction. Select Auto-commit and use explicit BEGIN TRANSACTION, COMMIT, or ROLLBACK statements.".into());
    }
    Ok(())
}

pub fn plan(database: DatabaseKind, mode: TxnMode, kind: &StmtKind) -> Result<TransactionPlan, String> {
    validate_mode(database, mode)?;
    Ok(if needs_implicit_begin(mode, TxnState::Idle, kind) {
        TransactionPlan::BeginIfIdle
    } else {
        TransactionPlan::Native
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StmtOutcome {
    Ok,
    Err,
}

/// The transaction state after Postgres processed `kind` with `outcome`, mirroring the server's
/// own rules: an error inside a block aborts it, `COMMIT` of an aborted block rolls back, and
/// transaction-control statements outside a block only warn.
pub fn txn_after(state: TxnState, kind: &StmtKind, outcome: StmtOutcome) -> TxnState {
    use StmtKind::*;
    use StmtOutcome::*;
    use TxnState::*;
    match (state, kind, outcome) {
        (Open, _, _) => Open,
        (Idle, Begin, Ok) => Active,
        (Idle, _, _) => Idle,
        (_, CommitAndChain, Ok) | (_, RollbackAndChain, Ok) => Active,
        (_, Commit, Ok) | (_, Rollback, Ok) => Idle,
        // A failed COMMIT (deferred constraint, serialization) rolls back and ends the block.
        (_, Commit, Err) | (_, CommitAndChain, Err) => Idle,
        (_, RollbackTo, Ok) => Active,
        (Active, _, Ok) => Active,
        (Active, _, Err) => Aborted,
        (Aborted, _, _) => Aborted,
    }
}

pub fn ended(kind: &StmtKind, outcome: StmtOutcome, observed: Option<TxnState>) -> bool {
    use StmtKind::*;
    match observed {
        Some(state) => state == TxnState::Idle,
        None => outcome == StmtOutcome::Ok && matches!(kind, Commit | Rollback | CommitAndChain | RollbackAndChain),
    }
}

pub fn needs_implicit_begin(mode: TxnMode, state: TxnState, kind: &StmtKind) -> bool {
    use StmtKind::*;
    mode == TxnMode::Manual
        && state == TxnState::Idle
        && !matches!(
            kind,
            Begin
                | Commit
                | Rollback
                | CommitAndChain
                | RollbackAndChain
                | Savepoint
                | RollbackTo
                | Release
                | CopyStdio
                | NoTxnBlock(_)
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use StmtKind::*;
    use StmtOutcome::*;
    use TxnState::*;

    fn all_kinds() -> Vec<StmtKind> {
        vec![
            Begin,
            Commit,
            Rollback,
            CommitAndChain,
            RollbackAndChain,
            Savepoint,
            RollbackTo,
            Release,
            Ddl("CREATE TABLE".into()),
            Dml(crate::sql::DmlVerb::Update),
            Query,
            CopyStdio,
            NoTxnBlock("VACUUM".into()),
            Other("SET".into()),
        ]
    }

    #[test]
    fn every_transition() {
        let table: Vec<(TxnState, StmtKind, StmtOutcome, TxnState)> = vec![
            (Idle, Begin, Ok, Active),
            (Idle, Begin, Err, Idle),
            (Idle, Commit, Ok, Idle),
            (Idle, Rollback, Ok, Idle),
            (Idle, RollbackTo, Err, Idle),
            (Idle, Query, Ok, Idle),
            (Idle, Query, Err, Idle),
            (Idle, Ddl("CREATE TABLE".into()), Ok, Idle),
            (Active, Begin, Ok, Active),
            (Active, Commit, Ok, Idle),
            (Active, Commit, Err, Idle),
            (Active, Rollback, Ok, Idle),
            (Idle, CommitAndChain, Err, Idle),
            (Active, CommitAndChain, Ok, Active),
            (Active, CommitAndChain, Err, Idle),
            (Active, RollbackAndChain, Ok, Active),
            (Aborted, CommitAndChain, Ok, Active),
            (Aborted, RollbackAndChain, Ok, Active),
            (Active, Savepoint, Ok, Active),
            (Active, Savepoint, Err, Aborted),
            (Active, RollbackTo, Ok, Active),
            (Active, RollbackTo, Err, Aborted),
            (Active, Release, Ok, Active),
            (Active, Query, Ok, Active),
            (Active, Query, Err, Aborted),
            (Active, Dml(crate::sql::DmlVerb::Insert), Err, Aborted),
            (Aborted, Query, Ok, Aborted),
            (Aborted, Query, Err, Aborted),
            (Aborted, Begin, Err, Aborted),
            (Aborted, Commit, Ok, Idle),
            (Aborted, Rollback, Ok, Idle),
            (Aborted, RollbackTo, Ok, Active),
            (Aborted, RollbackTo, Err, Aborted),
            (Aborted, Savepoint, Err, Aborted),
        ];
        for (from, kind, outcome, to) in table {
            assert_eq!(txn_after(from, &kind, outcome), to, "{from:?} + {kind:?}/{outcome:?}");
        }
    }

    #[test]
    fn only_a_reported_or_successful_end_ends_a_transaction() {
        for kind in all_kinds() {
            let ends = matches!(kind, Commit | Rollback | CommitAndChain | RollbackAndChain);
            assert_eq!(ended(&kind, Ok, None), ends, "{kind:?}");
            assert!(!ended(&kind, Err, None), "{kind:?}: a failed statement shows no end");
            assert!(ended(&kind, Err, Some(Idle)), "{kind:?}: the server reports no transaction");
            assert!(!ended(&kind, Ok, Some(Active)), "{kind:?}: the server reports one open");
        }
        let missed_begin = Other("HEADER".into());
        assert_eq!(txn_after(Idle, &missed_begin, Ok), Idle);
        assert!(!ended(&missed_begin, Ok, None), "an Idle the engine infers is not an end it saw");
    }

    #[test]
    fn the_machine_is_total() {
        for state in [Idle, Active, Aborted, Open] {
            for kind in all_kinds() {
                for outcome in [Ok, Err] {
                    let _ = txn_after(state, &kind, outcome);
                }
            }
        }
    }

    #[test]
    fn implicit_begin_only_in_manual_idle_for_blockable_statements() {
        for kind in all_kinds() {
            let expected = !matches!(
                kind,
                Begin
                    | Commit
                    | Rollback
                    | CommitAndChain
                    | RollbackAndChain
                    | Savepoint
                    | RollbackTo
                    | Release
                    | CopyStdio
                    | NoTxnBlock(_)
            );
            assert_eq!(needs_implicit_begin(TxnMode::Manual, Idle, &kind), expected, "{kind:?}");
            assert!(!needs_implicit_begin(TxnMode::Manual, Active, &kind));
            assert!(!needs_implicit_begin(TxnMode::Manual, Aborted, &kind));
            assert!(!needs_implicit_begin(TxnMode::AutoCommit, Idle, &kind));
        }
    }
}
