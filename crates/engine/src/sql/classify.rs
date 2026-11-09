use super::lex::{LexKind, Lexeme, Strings, lexemes};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmlVerb {
    Insert,
    Update,
    Delete,
    Merge,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StmtKind {
    Begin,
    Commit,
    Rollback,
    CommitAndChain,
    RollbackAndChain,
    Savepoint,
    RollbackTo,
    Release,
    Ddl(String),
    Dml(DmlVerb),
    Query,
    /// `COPY ... FROM STDIN` / `TO STDOUT`: the simple protocol cannot carry the data.
    CopyStdio,
    NoTxnBlock(String),
    Other(String),
}

/// What a successful statement makes stale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Invalidates {
    Nothing,
    /// The catalog of the database the statement ran in.
    Catalog,
    /// The instance's `pg_database` list.
    DatabaseList,
    Both,
}

impl StmtKind {
    pub fn invalidates(&self) -> Invalidates {
        match self {
            StmtKind::Ddl(tag) | StmtKind::NoTxnBlock(tag) if tag.ends_with(" DATABASE") => Invalidates::DatabaseList,
            StmtKind::Ddl(_) => Invalidates::Catalog,
            // `CREATE INDEX CONCURRENTLY`, `DROP INDEX CONCURRENTLY`, `REINDEX ... CONCURRENTLY`.
            StmtKind::NoTxnBlock(tag) if tag.contains("INDEX") || tag.starts_with("REINDEX") => Invalidates::Catalog,
            _ => Invalidates::Nothing,
        }
    }
}

pub fn classify(stmt: &str) -> StmtKind {
    let words: Vec<String> = top_level_words(stmt).take(4).map(|w| w.to_ascii_uppercase()).collect();
    let w = |i: usize| words.get(i).map(String::as_str).unwrap_or("");
    match w(0) {
        "BEGIN" | "START" => StmtKind::Begin,
        "COMMIT" | "END" if chains(&words) => StmtKind::CommitAndChain,
        "COMMIT" | "END" => StmtKind::Commit,
        "ROLLBACK" | "ABORT" if w(1) == "TO" || (matches!(w(1), "WORK" | "TRANSACTION") && w(2) == "TO") => StmtKind::RollbackTo,
        "ROLLBACK" | "ABORT" if chains(&words) => StmtKind::RollbackAndChain,
        "ROLLBACK" | "ABORT" => StmtKind::Rollback,
        "SAVEPOINT" => StmtKind::Savepoint,
        "RELEASE" => StmtKind::Release,
        "INSERT" => StmtKind::Dml(DmlVerb::Insert),
        "UPDATE" => StmtKind::Dml(DmlVerb::Update),
        "DELETE" => StmtKind::Dml(DmlVerb::Delete),
        "MERGE" => StmtKind::Dml(DmlVerb::Merge),
        "WITH" => after_with(stmt),
        "SELECT" | "TABLE" | "VALUES" | "EXPLAIN" | "SHOW" | "FETCH" | "UNION" | "INTERSECT" | "EXCEPT" => StmtKind::Query,
        "COPY" => copy_kind(stmt),
        "VACUUM" | "CLUSTER" | "CHECKPOINT" => StmtKind::NoTxnBlock(w(0).to_owned()),
        "ALTER" if w(1) == "SYSTEM" => StmtKind::NoTxnBlock("ALTER SYSTEM".into()),
        "DISCARD" if w(1) == "ALL" => StmtKind::NoTxnBlock("DISCARD ALL".into()),
        "CREATE" | "DROP" if w(1) == "DATABASE" || w(1) == "TABLESPACE" => {
            StmtKind::NoTxnBlock(format!("{} {}", w(0), w(1)))
        }
        "CREATE" | "DROP" | "REINDEX" if words.iter().any(|x| x == "CONCURRENTLY") => {
            StmtKind::NoTxnBlock(ddl_tag(&words))
        }
        "CREATE" | "ALTER" | "DROP" | "COMMENT" | "GRANT" | "REVOKE" | "REINDEX" | "RENAME" | "IMPORT" | "SECURITY" => {
            StmtKind::Ddl(ddl_tag(&words))
        }
        "TRUNCATE" => StmtKind::Other("TRUNCATE TABLE".into()),
        "LOCK" => StmtKind::Other("LOCK TABLE".into()),
        "" if stmt.contains('(') => StmtKind::Query,
        other => StmtKind::Other(other.to_owned()),
    }
}

fn chains(words: &[String]) -> bool {
    words.windows(2).any(|pair| pair[0] == "AND" && pair[1] == "CHAIN")
}

/// The source or target follows the first `FROM` or `TO`: the table name before it is any number
/// of words, and a column list or query is parenthesized. A quoted file name yields no keyword.
fn copy_kind(stmt: &str) -> StmtKind {
    let mut words = top_level_words(stmt).map(|w| w.to_ascii_uppercase());
    let target = words.by_ref().find(|w| w == "FROM" || w == "TO").and_then(|_| words.next());
    match target.as_deref() {
        Some("STDIN" | "STDOUT") => StmtKind::CopyStdio,
        _ => StmtKind::Other("COPY".into()),
    }
}

/// Postgres reports `CREATE OR REPLACE FUNCTION` as `CREATE FUNCTION` and `CREATE UNIQUE INDEX`
/// as `CREATE INDEX`: the verb plus the object word, skipping modifiers between them.
fn ddl_tag(words: &[String]) -> String {
    const MODIFIERS: [&str; 10] = [
        "OR", "REPLACE", "UNIQUE", "TEMP", "TEMPORARY", "UNLOGGED", "GLOBAL", "LOCAL", "RECURSIVE", "CONCURRENTLY",
    ];
    let verb = &words[0];
    let object = words[1..].iter().find(|w| !MODIFIERS.contains(&w.as_str()));
    match object {
        Some(object) if object == "MATERIALIZED" => format!("{verb} MATERIALIZED VIEW"),
        Some(object) if object == "FOREIGN" => format!("{verb} FOREIGN TABLE"),
        Some(object) => format!("{verb} {object}"),
        None => verb.clone(),
    }
}

/// The statement after a WITH list decides its kind, so `WITH … INSERT` reports `INSERT 0 n`. A word right after
/// `WITH`, `RECURSIVE`, or a top-level comma names a CTE, even one called `update`.
fn after_with(stmt: &str) -> StmtKind {
    let mut depth = 0usize;
    let mut names_next = false;
    for Lexeme { kind, span } in lexemes(stmt, Strings::Standard) {
        match kind {
            LexKind::Space | LexKind::LineComment | LexKind::BlockComment => continue,
            LexKind::Open => depth += 1,
            LexKind::Close => depth = depth.saturating_sub(1),
            LexKind::Comma if depth == 0 => {
                names_next = true;
                continue;
            }
            LexKind::Word if depth == 0 => {
                let word = stmt[span].to_ascii_uppercase();
                if !names_next {
                    match word.as_str() {
                        "INSERT" => return StmtKind::Dml(DmlVerb::Insert),
                        "UPDATE" => return StmtKind::Dml(DmlVerb::Update),
                        "DELETE" => return StmtKind::Dml(DmlVerb::Delete),
                        "MERGE" => return StmtKind::Dml(DmlVerb::Merge),
                        "SELECT" | "VALUES" | "TABLE" => return StmtKind::Query,
                        _ => {}
                    }
                }
                names_next = matches!(word.as_str(), "WITH" | "RECURSIVE");
                continue;
            }
            _ => {}
        }
        names_next = false;
    }
    StmtKind::Query
}

fn top_level_words(stmt: &str) -> impl Iterator<Item = &str> {
    let mut depth = 0usize;
    lexemes(stmt, Strings::Standard).filter_map(move |Lexeme { kind, span }| match kind {
        LexKind::Open => {
            depth += 1;
            None
        }
        LexKind::Close => {
            depth = depth.saturating_sub(1);
            None
        }
        LexKind::Word if depth == 0 => Some(&stmt[span]),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transaction_control() {
        assert_eq!(classify("BEGIN"), StmtKind::Begin);
        assert_eq!(classify("start transaction isolation level serializable"), StmtKind::Begin);
        assert_eq!(classify("commit"), StmtKind::Commit);
        assert_eq!(classify("END;"), StmtKind::Commit);
        assert_eq!(classify("rollback"), StmtKind::Rollback);
        assert_eq!(classify("abort work"), StmtKind::Rollback);
        assert_eq!(classify("rollback to savepoint s1"), StmtKind::RollbackTo);
        assert_eq!(classify("ROLLBACK TO s1"), StmtKind::RollbackTo);
        assert_eq!(classify("rollback work to savepoint s1"), StmtKind::RollbackTo);
        assert_eq!(classify("ROLLBACK TRANSACTION TO s1"), StmtKind::RollbackTo);
        assert_eq!(classify("savepoint s1"), StmtKind::Savepoint);
        assert_eq!(classify("release savepoint s1"), StmtKind::Release);
        assert_eq!(classify("commit and chain"), StmtKind::CommitAndChain);
        assert_eq!(classify("END TRANSACTION AND CHAIN"), StmtKind::CommitAndChain);
        assert_eq!(classify("commit work and no chain"), StmtKind::Commit);
        assert_eq!(classify("rollback and chain"), StmtKind::RollbackAndChain);
        assert_eq!(classify("abort work and chain"), StmtKind::RollbackAndChain);
        assert_eq!(classify("rollback and no chain"), StmtKind::Rollback);
    }

    #[test]
    fn comments_and_ctes_do_not_confuse_the_verb() {
        assert_eq!(classify("-- note\n/* more */ with x as (select 1) select * from x"), StmtKind::Query);
        assert_eq!(classify("with x as (insert into t values (1)) select 1"), StmtKind::Query);
        assert_eq!(classify("WITH \"{{a}}\" AS (\nSELECT 1\n)\nINSERT INTO t SELECT * FROM \"{{a}}\""), StmtKind::Dml(DmlVerb::Insert));
        assert_eq!(classify("with recursive r as (select 1), update as (select 2) update t set n = 1 from r"), StmtKind::Dml(DmlVerb::Update));
        assert_eq!(classify("with d as (select 1) select * from d for update"), StmtKind::Query);
        assert_eq!(classify("with d as (select 1) delete from t using d"), StmtKind::Dml(DmlVerb::Delete));
        assert_eq!(classify("/* update */ select 1"), StmtKind::Query);
        assert_eq!(classify("create table \"concurrently\" (id int)"), StmtKind::Ddl("CREATE TABLE".into()));
        assert_eq!(classify("select 'it''s -- not a comment'"), StmtKind::Query);
        assert_eq!(classify("explain analyze select 1"), StmtKind::Query);
        assert_eq!(classify("show search_path"), StmtKind::Query);
    }

    #[test]
    fn comments_end_where_the_splitter_ends_them() {
        for sql in [
            "/*/ header */ begin",
            "/* a /* b */ c */ begin",
            "/* x; /* nested; */ y */ begin",
            "/*\n\nselect 1 */ begin",
            "/**/begin",
            "-- c\rbegin",
            "-- a;b\nbegin",
            "-- header\n/* note */ begin",
        ] {
            assert_eq!(classify(sql), StmtKind::Begin, "{sql:?}");
        }
        for sql in ["/* a /* b */ begin", "-- begin", "/*/ begin */"] {
            assert_eq!(classify(sql), StmtKind::Other(String::new()), "{sql:?}");
        }
        assert_eq!(classify("select $$ begin $$"), StmtKind::Query);
        assert_eq!(classify("copy t from $$/tmp/x$$"), StmtKind::Other("COPY".into()));
        assert_eq!(classify("copy t (a) from E'\\'stdin'"), StmtKind::Other("COPY".into()));
    }

    #[test]
    fn dml_and_ddl_tags() {
        assert_eq!(classify("insert into t values (1)"), StmtKind::Dml(DmlVerb::Insert));
        assert_eq!(classify("UPDATE t SET a = 1"), StmtKind::Dml(DmlVerb::Update));
        assert_eq!(classify("delete from t"), StmtKind::Dml(DmlVerb::Delete));
        assert_eq!(classify("merge into t using s on true when matched then delete"), StmtKind::Dml(DmlVerb::Merge));
        assert_eq!(classify("create table t (id int)"), StmtKind::Ddl("CREATE TABLE".into()));
        assert_eq!(classify("create temp table t (id int)"), StmtKind::Ddl("CREATE TABLE".into()));
        assert_eq!(classify("create or replace function f() returns int as $$ select 1 $$ language sql"), StmtKind::Ddl("CREATE FUNCTION".into()));
        assert_eq!(classify("create unique index i on t (a)"), StmtKind::Ddl("CREATE INDEX".into()));
        assert_eq!(classify("create materialized view v as select 1"), StmtKind::Ddl("CREATE MATERIALIZED VIEW".into()));
        assert_eq!(classify("alter table t add column b int"), StmtKind::Ddl("ALTER TABLE".into()));
        assert_eq!(classify("drop table if exists t"), StmtKind::Ddl("DROP TABLE".into()));
        assert_eq!(classify("comment on table t is 'x'"), StmtKind::Ddl("COMMENT ON".into()));
    }

    #[test]
    fn copy_stdio_and_non_transactional_statements() {
        assert_eq!(classify("copy t from stdin"), StmtKind::CopyStdio);
        assert_eq!(classify("COPY (select 1) TO STDOUT CSV"), StmtKind::CopyStdio);
        assert_eq!(classify("copy t from '/tmp/x.csv'"), StmtKind::Other("COPY".into()));
        assert_eq!(classify("copy public.orders to stdout"), StmtKind::CopyStdio);
        assert_eq!(classify("copy pg_temp.t from stdin"), StmtKind::CopyStdio);
        assert_eq!(classify("copy pg_temp.t (x, y) from stdin with (format csv)"), StmtKind::CopyStdio);
        assert_eq!(classify("COPY binary \"Shop\".\"Orders\" (id) TO STDOUT"), StmtKind::CopyStdio);
        assert_eq!(classify("copy (select 'x' from t) to stdout"), StmtKind::CopyStdio);
        assert_eq!(classify("copy \"stdin\" to stdout"), StmtKind::CopyStdio);
        assert_eq!(classify("copy public.orders to '/tmp/stdout.csv'"), StmtKind::Other("COPY".into()));
        assert_eq!(classify("copy pg_temp.t from '/tmp/stdin' with (format csv)"), StmtKind::Other("COPY".into()));
        assert_eq!(classify("copy public.orders to program 'cat'"), StmtKind::Other("COPY".into()));
        assert_eq!(classify("vacuum analyze t"), StmtKind::NoTxnBlock("VACUUM".into()));
        assert_eq!(classify("create database x"), StmtKind::NoTxnBlock("CREATE DATABASE".into()));
        assert_eq!(classify("create index concurrently i on t (a)"), StmtKind::NoTxnBlock("CREATE INDEX".into()));
        assert_eq!(classify("alter system set work_mem = '1MB'"), StmtKind::NoTxnBlock("ALTER SYSTEM".into()));
    }

    #[test]
    fn other_statements_carry_their_tag() {
        assert_eq!(classify("set search_path to lab"), StmtKind::Other("SET".into()));
        assert_eq!(classify("truncate t"), StmtKind::Other("TRUNCATE TABLE".into()));
        assert_eq!(classify("lock table t"), StmtKind::Other("LOCK TABLE".into()));
        assert_eq!(classify("do $$ begin end $$"), StmtKind::Other("DO".into()));
        assert_eq!(classify("call p()"), StmtKind::Other("CALL".into()));
        assert_eq!(classify("(select 1) union (select 2)"), StmtKind::Query);
        assert_eq!(classify(""), StmtKind::Other(String::new()));
    }

    #[test]
    fn statements_name_what_they_invalidate() {
        let of = |sql: &str| classify(sql).invalidates();
        assert_eq!(of("create table t (id int)"), Invalidates::Catalog);
        assert_eq!(of("drop index concurrently i"), Invalidates::Catalog);
        assert_eq!(of("reindex table concurrently t"), Invalidates::Catalog);
        assert_eq!(of("create database x"), Invalidates::DatabaseList);
        assert_eq!(of("drop database x"), Invalidates::DatabaseList);
        assert_eq!(of("alter database x rename to y"), Invalidates::DatabaseList);
        assert_eq!(of("vacuum t"), Invalidates::Nothing);
        assert_eq!(of("insert into t values (1)"), Invalidates::Nothing);
        assert_eq!(of("truncate t"), Invalidates::Nothing);
    }
}
