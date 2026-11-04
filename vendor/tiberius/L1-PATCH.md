# Tiberius session observations, messages, and exact money

This directory contains the source of the published `tiberius` 0.13.0 crate.
The registry archive SHA-256 is
`e07324791de2bdaed058af4aa433a0b5ec5c0beac407a9797419c19805df2415`.
It matches the dependency checksum in L1 Data's lockfile before this patch.
The archive's `.cargo_vcs_info.json` records commit
`7fd75bb7070fe6ea41374698abf78928bb983bda` with `dirty: true`.
The archive checksum identifies the exact source, not that commit alone.
The original MIT and Apache licenses are included unchanged.

Only `src`, the two manifests, the upstream README, licenses, and VCS record
were copied. The normalized `Cargo.toml` omits test and example targets whose
files are not included. The crate is excluded from the application workspace.

`Client::session_state` reads existing server responses without sending SQL.
It exposes the latest database ENVCHANGE value, presence of the transaction
descriptor, packet SPID, and LOGINACK major, minor, and build version.
It rejects incomplete responses, missing login fields, and untracked
distributed or bound-session transaction changes. Cancellation safety is
unchanged. The caller must discard abandoned or cancelled clients.

`QueryStream` also yields `QueryItem::Info` for each INFO token,
`QueryItem::Error` for each ERROR token, and `QueryItem::Done` for each DONE and
DONEINPROC token, in server order. Upstream dropped all three, so `PRINT`
output, low-severity `RAISERROR` messages, and affected-row counts were lost,
and an error could be reported only after the messages that followed it. The
stream still ends with the first error once its items run out, so callers that
collect results still fail. `query`, `simple_query`, and `Query::query` no
longer skip the tokens before the first result set, which held back messages
until a result set arrived. `Done` carries the row count only when the server
set DONE_COUNT. Like SqlClient, it also drops the count of a `SELECT` that sent
no result set, which `SET @x = 1` and `DECLARE @x int = 1` report as one row.
`TokenInfo` is public, with accessors like `TokenError`'s.

The implementation changes only `src/client.rs`, `src/client/connection.rs`,
`src/lib.rs`, `src/query.rs`, `src/tds/context.rs`, `src/tds/codec/header.rs`,
`src/tds/codec/token/token_done.rs`, `src/tds/codec/token/token_info.rs`,
`src/tds/stream/query.rs`, and `src/tds/stream/token.rs`. The application
regressions are in `crates/engine/tests/mssql_session_state.rs` and
`crates/engine/tests/mssql.rs`.

`money::decode` returns `ColumnData::Numeric` at scale 4. Upstream returned
`ColumnData::F64`, which cannot hold the full range of `money`. This also applies
to `money` and `smallmoney` inside a `sql_variant`. Encoding is unchanged, so an
`f64` still encodes to a money column. The change is in
`src/tds/codec/column_data/money.rs`, and in the expected values of the unit tests
in `src/tds/codec/column_data.rs` and `src/tds/codec/column_data/var_len.rs`. All
other protocol decoding and row conversion remain upstream.

SQL Server does not set DONE_INXACT, as specified in
[MS-TDS Appendix A](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-tds/135d0ebe-5c4c-4a94-99bf-1811eccb9f4a).
[ENVCHANGE](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-tds/2b3eb7e5-d43d-4d1b-bf4d-76b9e3afc791)
does not report nesting-only changes. This API cannot supply transaction depth
or committability. A login database alias may retain its supplied spelling.
LOGINACK does not contain the fourth ProductVersion revision component.

Run `python3 vendor/tiberius/verify-upstream.py /path/to/tiberius-0.13.0.crate`
to verify the archive checksum and compare every included upstream file.
The script requires the differences to equal `UPSTREAM.patch` byte for byte.
Cargo's registry cache contains the archive after installing this dependency.
