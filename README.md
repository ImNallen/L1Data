<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/images/l1-logo-white.svg">
  <img src="docs/images/l1-logo-black.svg" alt="L1 Data logo" width="64">
</picture>

# L1 Data

A desktop SQL IDE for PostgreSQL and Microsoft SQL Server, built with Rust, Tauri 2, and React.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/images/l1-data-dark.png">
  <img src="docs/images/l1-data.png" alt="L1 Data with SQL notebook cells, results, a split database browser, and the assistant">
</picture>

## Features

- **Both databases in one app.** PostgreSQL and SQL Server, including Azure SQL with Microsoft Entra ID sign-in and SSH tunnels.
- **A catalog you can browse.** Explore every schema, table, column, key, and index, generate `CREATE` scripts, and map foreign keys.
- **A SQL notebook that knows your schema.** Edit multiple cells with results underneath, plus completion, formatting, and query parameters. Each cell keeps its own session, so `SET`, temp tables, and transactions stay with it.
- **Results you can work with.** A streaming grid, row editing on PostgreSQL, CSV import, and export to CSV, JSON, Markdown, or SQL.
- **Production safety.** The app asks before a query, an edit, or an import writes to a Production connection.
- **A Claude assistant.** Chat about your database through your own `claude` CLI. By default it sees only your schema.

[docs/features.md](docs/features.md) describes each feature and lists the keyboard shortcuts.

## Install

Download a nightly build for macOS, Windows, or Linux from [GitHub Releases](https://github.com/ImNallen/L1Data/releases). The app updates itself, except a Linux `.deb` install.

## Quickstart

You need Rust 1.93, Node 26, pnpm 11, and Docker.

```sh
pnpm install
pnpm db:up          # Postgres 16 and SQL Server 2022, each with a seeded `shop` database
pnpm tauri dev      # the desktop app
pnpm bridge         # the real engine behind a browser UI on localhost:5173
```

## Connect to the dev databases

`pnpm db:up` starts two databases. Create a connection for each in the app.

| Field | PostgreSQL | SQL Server |
|---|---|---|
| Database kind | PostgreSQL | SQL Server |
| Host | `localhost` | `localhost` |
| Port | `5433` | `1434` |
| Database | `shop` | `shop` |
| User | `l1data` | `sa` |
| Password | `l1data` | `L1Data_dev_Str0ng` |
| TLS | Prefer (the default) | Encryption: Require |
| Name | `Dev Postgres` | `Dev SQL Server` |

Pick the database kind first. The SQL Server certificate is self-signed, so change **Encryption** from **Verify full** to **Require**. To skip straight to a filled-in PostgreSQL form, paste `postgres://l1data:l1data@localhost:5433/shop` into **Connection URL** on the first step.

## Checks

```sh
cargo test --workspace
pnpm lint && pnpm fmt:check && pnpm typecheck && pnpm test
```

[docs/development.md](docs/development.md) covers SQL Server setup, the database and smoke tests, and the environment variables.

## Layout

- `crates/engine` holds every behavior and every IPC type. It has no Tauri dependency.
- `src-tauri` is the app shell.
- `crates/dev-bridge` serves the engine over HTTP for browser-driven UI work.
- `src` is the React frontend. Do not edit `src/ipc/bindings.ts`, which is generated.
- `dev` holds the Docker databases, their seeds, and the smoke scripts.

[docs/architecture.md](docs/architecture.md) explains the design. [docs/releasing.md](docs/releasing.md) explains how releases ship. Coding agents start with [AGENTS.md](AGENTS.md).
