# Development

Start with the [README quickstart](../README.md#quickstart). This page covers the second database, every check, and the environment variables.

The `pnpm db:up` seed also creates `lab.type_zoo`, a table with one column for each hard-to-render type.

`pnpm db:down` stops every dev container and deletes their data, so the next `pnpm db:up` starts from a fresh seed.

## SQL Server

`pnpm db:up` also starts SQL Server 2022 Developer and seeds its `shop` and `reporting` databases. The container uses an amd64 image, so it runs under emulation on Apple silicon. `pnpm db:up` can run again without duplicating seed data.

## Checks

```sh
cargo fmt --check        # Rust format of the workspace crates; cargo fmt rewrites them
cargo test --workspace   # engine units, bindings drift test, bridge coverage test
cargo test -p engine --features pg-tests   # engine against PostgreSQL
cargo test -p engine --features mssql-tests   # engine against SQL Server; needs pnpm db:up
cargo test -p engine --features ssh-tests   # engine through an SSH bastion; needs pnpm db:up and pnpm db:ssh:up
python3 dev/mssql-tls-smoke.py   # public Engine VerifyFull CA/name checks against a disposable SQL Server
pnpm lint                # oxlint over the frontend and scripts/
pnpm fmt:check           # oxfmt check of the frontend, scripts/, and root configs; pnpm fmt rewrites them
pnpm typecheck
pnpm test                # vitest, then the release scripts' node tests
pnpm build               # production frontend bundle
pnpm self-check          # boots the engine inside the Tauri binary, connects to the Docker database, prints JSON, exits
cargo build -p dev-bridge && dev/bridge-smoke.sh   # drives the bridge over HTTP against the Docker database
cargo build -p dev-bridge -p engine && python3 dev/mssql-smoke.py   # mixed PostgreSQL/SQL Server bridge checks; needs both databases and a free bridge port
cargo test -p engine --features claude-smoke --test claude_smoke -- --nocapture   # the assistant through your installed claude CLI
pnpm bindings            # rewrites src/ipc/bindings.ts after an IPC type or command changes
pnpm icons               # rewrites src-tauri/icons from src-tauri/app-icon.svg
pnpm screenshot          # macOS: rewrites docs/images/l1-data.png from the running app
```

CI runs `pg-tests` on PostgreSQL 16, 17, and 18. To run them on another server, point `L1_DATA_TEST_PG` at a database seeded with `dev/seed.sql`. The keyword lists in `crates/engine/src/sql/keywords.rs` come from 16, so run `dev/pg-keywords.sh` against 16.

The SQL Server TLS smoke needs Docker, OpenSSL, Rust, and the cached `mcr.microsoft.com/mssql/server:2022-latest` image. It builds the engine and a temporary public-API probe, starts its own container on a free loopback port, and uses generated fixture credentials and certificates. It checks trusted `localhost`, hostname rejection on `127.0.0.1`, unrelated-CA rejection, and **Require** controls. Each probe child gets isolated `SSL_CERT_FILE` and `SSL_CERT_DIR` settings. The script removes its container and temporary files on success or failure. It does not use the seeded databases or bridge.

`pnpm db:ssh:up` starts an OpenSSH bastion on `localhost:2222` that forwards only to `postgres:5432`, and to `mssql:1433`. The bastion's host key and the client keys under `dev/sshd` are test-only fixtures.

A debug binary rewrites `src/ipc/bindings.ts` each time it starts, so `pnpm tauri dev` and `pnpm self-check` keep it current too.

The `claude-smoke` test needs the `claude` CLI installed and signed in, and the Docker database. It spends three Haiku turns on your Claude subscription, and one of them waits 75 seconds for an approval.

`pnpm screenshot` starts its own Postgres container on port 5434. It needs `swift`, `osascript`, `cliclick`, and `python3`, free ports 7077, 5173, and 5434, and Accessibility and Screen Recording permission for the terminal.

## Measure CSV import performance

To measure CSV import in a production frontend against a release engine, build `dev-bridge` with `cargo build --release -p dev-bridge`. Build the browser bundle with `L1_DATA_BRIDGE_PORT=7388 pnpm exec vite build --mode bridge --outDir /tmp/l1-import-web`, then serve it with `pnpm exec vite preview --outDir /tmp/l1-import-web --port 5485 --strictPort`. Start that bridge with `L1_DATA_BRIDGE_PORT=7388 L1_DATA_VITE_PORT=5485 L1_DATA_DATA_DIR=$(mktemp -d) target/release/dev-bridge`. Open its page in a named agent-browser session and run `python3 dev/import-smoke.py --bridge http://127.0.0.1:7388 --perf --ui-session <session>`. The script alternates five psql COPY and UI imports into fresh tables, verifies counts, and records preparation separately. Without `--ui-session`, it measures the engine request through its completed reply. Normal production builds do not include the browser bridge.

## Environment variables

| Variable | Effect |
|---|---|
| `L1_DATA_DATA_DIR` | The data directory. The app reads it only in debug builds. The dev-bridge uses a temporary directory without it. |
| `L1_DATA_SECRET_STORE=memory` | Keeps the master key in memory instead of the OS credential store. Debug builds only. |
| `L1_DATA_SELF_CHECK=1` | Runs the self-check and exits without opening a window. |
| `L1_DATA_SELF_CHECK_URL` | The database for the self-check. Default `postgres://l1data:l1data@localhost:5433/shop`. |
| `L1_DATA_TEST_PG` | The database for `pg-tests` and `claude-smoke`. Same default. |
| `L1_DATA_TEST_PG_NO_PREPARED=1` | Lets the prepared-transaction pg test pass on a server with `max_prepared_transactions = 0`. Without it, that test fails there. |
| `L1_DATA_TEST_MSSQL` | SQL Server URL for the mixed bridge smoke. Default `mssql://sa:L1Data_dev_Str0ng@localhost:1434/shop`. |
| `L1_DATA_TEST_MSSQL_HOST` | Host for the SQL Server Rust integration tests. Default `localhost`. |
| `L1_DATA_TEST_MSSQL_PORT` | Port for the SQL Server Rust integration tests. Default `1434`. |
| `L1_DATA_TEST_MSSQL_PASSWORD` | The `sa` password for the SQL Server Rust integration tests. Default is the development fixture password above. |
| `L1_DATA_BRIDGE_PORT` | The port the dev-bridge listens on, and the port vite gives the page for it. `dev/bridge-smoke.sh` and `dev/mssql-smoke.py` use it too. Default `7077`. |
| `L1_DATA_VITE_PORT` | The port vite serves the UI on, and the only port the dev-bridge accepts requests from. Default `5173`. `pnpm tauri dev` needs the default. |
| `L1_DATA_AZ_BINARY` | The program the dev-bridge's engine runs instead of `az` on the PATH for Entra ID sign-in, such as `target/debug/fake-az`. A relative path is relative to the working directory. Only the dev-bridge reads it. The app ignores it. |
| `L1_FAKE_AZ_SCENARIO`, `L1_FAKE_AZ_LOG` | The scenario and call log of `fake-az`. The header of `crates/engine/src/bin/fake_az.rs` describes them. |
| `L1_DATA_UPDATE_SCENARIO` | The scripted update feed the dev-bridge serves: `current` (the default), `available`, `nightly-build`, `offline`, `bad-signature`, `install-fails`, or `off`. `crates/engine/src/update/scripted.rs` defines each. |
| `UPDATE_BINDINGS=1` | Makes the bindings drift test rewrite `src/ipc/bindings.ts` instead of failing. |
