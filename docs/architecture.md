# L1 Data v1 architecture

## Problem

L1 Data is a Tauri 2 desktop IDE for PostgreSQL and Microsoft SQL Server, with a connection tree, SQL editor tabs, a results grid, an output log, and per-tab transactions. The original PostgreSQL design below is reconciled with the current database adapters in "Implementation reconciliation". The shape isn't obvious for five reasons:

- **Arbitrary PG values must cross IPC.** These include numeric, timestamptz, arrays, enums, composites, bytea, and extension types. Showing them correctly normally means one decoder per type. The long tail (ranges, tsvector, PostGIS, user composites) never ends.
- **Large results** (tens of thousands of rows, sometimes millions by accident) must stay fast without exhausting the webview's memory.
- **Sessions are stateful.** A manual-transaction tab owns an open transaction. `SET search_path` and temp tables must stick for the tab. Cancel and a dead socket must leave every tab in a state the user can reason about.
- **Two languages share one contract**, and nothing may drift between them.
- **An agent must prove the app works end to end** on macOS without a human clicking.

Verified constraints the design honors:

- `tokio-postgres` 0.7.18 has `CancelToken` and a streaming `simple_query_raw`. Its `SimpleColumn` exposes only `name()`, so no type OIDs come back from the simple protocol. `Statement::columns()` from `prepare` gives `Type` (custom enum and composite names resolved) plus `table_oid()` and `column_id()`. `SimpleQueryMessage::CommandComplete(u64)` carries a row count, not the textual tag.
- `sqlx` has no server-side cancel request.
- `tauri-specta` 2.0.0-rc.25 generates typed commands and events and supports `tauri::ipc::Channel` arguments.
- `keyring` 4.2 (default feature `v1`) provides `Entry::new(service, user)`, `set_secret`, and `get_secret`, backed by Keychain on macOS, Credential Manager on Windows, and Secret Service on Linux.
- `chacha20poly1305` 0.10 provides `XChaCha20Poly1305`, which seals saved passwords.
- `@codemirror/lang-sql` ships `PostgreSQL` and `MSSQL` dialects. The editor uses their highlighting, not their completion.
- `tauri-driver` does not support macOS. `@wdio/tauri-service` with the embedded provider (`tauri-plugin-wdio-webdriver` 1.5.0) does.

## Usage (caller's view)

### README quickstart (developer)

```sh
pnpm install
pnpm db:up              # dev/db-up.sh: postgres:16 on :5433 (seeded `shop` db + lab.type_zoo) and SQL Server 2022 on :1434 (seeded `shop` and `reporting`)
pnpm tauri dev          # app; bindings regenerate on every debug build
pnpm bridge             # dev-bridge on :7077 + vite on :5173 (L1_DATA_BRIDGE_PORT, L1_DATA_VITE_PORT): the real Engine from a plain browser
cargo test -p engine                       # pure unit tests, no DB
cargo test -p engine --features pg-tests   # engine against the Docker DB
pnpm test                                  # vitest: reducers, formatting
pnpm typecheck                             # tsc --noEmit
pnpm bindings                              # rewrite src/ipc/bindings.ts without opening a window
pnpm self-check                            # debug binary boots Engine inside Tauri, connects, prints JSON, exits
cargo build -p dev-bridge && dev/bridge-smoke.sh   # curl flow against the bridge, checks the SSE stream
cargo test -p engine --features claude-smoke --test claude_smoke -- --nocapture   # the assistant through the installed claude CLI
pnpm screenshot                            # macOS: rewrite docs/images/l1-data.png from the real app
pnpm icons                                 # rewrite src-tauri/icons from src-tauri/app-icon.svg
```

### Call site 1: frontend boot (src/boot.ts)

```ts
export async function boot(): Promise<void> {
  installRunBehavior();                            // ⌘↵, ⌘⇧↵, Escape, doc and cursor listeners for every editor
  // Listen first, then snapshot. Every event carries a full entity snapshot with a `rev`,
  // so an event that races the boot snapshot is harmless: the reducer keeps the higher rev.
  await listen<AppEvent>("app-event", (e) => useLive.getState().apply(e.payload));
  const snapshot = await commands.appBoot();       // live connections + consoles + workspace + settings
  useLive.getState().hydrate(snapshot);
  for (const tab of snapshot.workspace.tabs) seedEditor(tab.id, tab.kind === "Sql" ? tab.text : browseQuery(tab.schema, tab.table));
  useWorkspace.getState().restore(snapshot.workspace, snapshot.connections, snapshot.settings);
  startTheme();
  if (snapshot.workspace.tabs.length === 0) openSqlTab();
  startPersistence();                              // debounced workspace_save
  installCloseFlush(savesInFlight);                // desktop: a window close writes the pending save first, at most 2 s
  void reconcileFiles();                           // reads each bound file once: reload clean tabs that changed, flag missing ones
  startCatalogSync();                              // useCatalog holds exactly the Ready catalogs in the connection snapshots
}
```

### Call site 2: run the active tab (features/editor/runActions.ts)

`runActive(script?)` runs the active tab through the private `runTab`. A table tab sends `tablePage` only while its editor still holds `browseQuery(schema, table)`. Any other text runs as a script, with the explicit `RunTarget` when one is given, else the selection or the cursor. History's **Run** and the assistant's **Run in new tab** open a new tab and pass `{ kind: "all" }`, so every statement of the inserted SQL runs.

What `⌘↵` will run is always marked. On each doc or cursor change, debounced 30 ms with one request in flight, the editor calls `script_outline(text, cursor)` and draws one rounded box behind `statements[atCursor]` (`cm-run-target`, in a CodeMirror layer under the text). The box is as wide as the statement's longest line, counted in columns rather than measured from drawn lines, so it keeps its size while the editor scrolls, and it stops 8 px short of the scroller's right edge. The active line's background is translucent so the box shows through it. A reply is dropped when the doc or the cursor moved meanwhile, and a selection or a browsing table tab clears the mark. `⌘↵`, **Run**, and `⌘⇧↵` go through `runOrChoose`, which asks for the same outline. `⌘↵` and **Run** run the cursor's statement when `atCursor` names one. When it is `null`, because the cursor is on a blank line between statements, they open the statement chooser (`chooserState.ts`, `StatementChooser.tsx`) instead, even for one statement. `⌘⇧↵` also opens it whenever the outline holds two or more statements. The chooser preselects `atCursor`, or else the last statement that ends before the cursor. Its **All** row appears only for two or more statements. While the chooser is open, the highlighted row's statements take the run box's place, each in the same rounded box, so **All** boxes every statement. A statement runs as `{ kind: "selection" }` over its range, and **All** as `{ kind: "all" }`. A doc change or a tab switch closes the chooser, since its ranges would be stale. Neither path splits SQL in TypeScript, so the mark and the preselected row always name what a `Cursor` run picks.

```ts
async function runTab(tabId: TabId, target?: PageTarget, script?: RunTarget): Promise<void> {
  let request: RunRequest;
  if (tab.kind === "table" && isBrowsing(tab)) {
    request = { kind: "tablePage", schema: tab.schema, table: tab.table, page: target?.page ?? tab.page, query: tab.query };
  } else {
    const state = editorStateOf(tabId);
    const target = script ?? selectionTarget(state);
    const text = state.doc.toString();
    const found = await commands.scriptParams(text, target, textTarget(tab).databaseKind);
    if (found.status === "error") return notify(runErrorText(found.error));
    const params = found.data.length ? await promptParams(tabId, JSON.stringify(tab.console), found.data, tab.title, valid) : [];
    if (!params || !valid()) return;
    request = {
      kind: "script",
      text,
      target,
      mode: "Run",
      params,
    };
  }
  let res = await commands.consoleRun(tabId, request, null);
  while (res.status === "error" && res.error.kind === "ConfirmWrites") { // a write on Production; nothing ran
    if (!(await writesConfirmed(tab, res.error.confirmation)) || !valid()) return;
    res = await commands.consoleRun(tabId, request, res.error.confirmation.digest);
  }
  if (res.status === "error") return notify(runErrorText(res.error)); // Busy | NoStatement | BadFilter | Connect
  if (request.kind === "script") flashRanges(tabId, res.data.statements); // UTF-16 doc ranges actually executed
  // Progress, results, errors arrive as AppEvent::ConsoleChanged / ConsoleOutput.
}
```

### Call site 3: the grid pulls a window of rows (features/results/ResultGrid.tsx)

```ts
const cache = rowCacheFor(meta.ref);                       // module-level RowCache per ResultRef
useSyncExternalStore(cache.subscribe, cache.getVersion);
void cache.ensure({ start: firstRow - PREFETCH, end: lastRow + 1 + PREFETCH }, rowCount); // 256-row pages via commands.resultPage
// header: category icon (key icon for a PK column) + meta.columns[c].name; type.display in the tooltip
// align:  isRightAligned(meta.columns[c].type.category)
// cell:   cache.get(r)?.[c]; null is SQL NULL, string is PG text output
await commands.resultExport(meta.ref, "Csv", "All", path); // ExportMenu; Rust writes from its own buffer, or streams a re-run
```

### Call site 4: engine integration test (crates/engine/tests/pg.rs)

```rust
#[tokio::test]
async fn manual_tab_isolated_until_commit_and_abort_rolls_back() {
    let mut h = Harness::start().await;              // Engine on tmp dir + MemoryKeyStore + Docker PG (:5433)
    let a = h.sql_tab(TxnMode::Manual).await;
    let b = h.sql_tab(TxnMode::AutoCommit).await;
    let table = unique("public.t_iso");
    h.run_ok(b, &format!("create table {table} (id int)")).await;
    let state = h.run_ok(a, &format!("insert into {table} values (1)")).await;
    assert_eq!(state.txn, TxnState::Active);
    assert_eq!(h.scalar(b, &format!("select count(*) from {table}")).await, "0");
    h.engine.console_commit(a, None).await.unwrap();
    assert_eq!(h.scalar(b, &format!("select count(*) from {table}")).await, "1");
    // ...then the abort, rollback, and savepoint cases
}
```

## Shape

### Load-bearing decisions

1. **Values cross IPC as text, with separate column metadata.** PostgreSQL prepares each statement for its native types, then streams the server's text output through `simple_query_raw`. SQL Server receives typed TDS values and converts them once inside its private driver. A cell remains `string | null`.
   - The frontend and shared result store have no type decoders. SQL Server's driver owns canonical conversion and refuses a value it cannot represent faithfully.
   - Unknown PostgreSQL and extension types render exactly as psql shows them.
   - Type-aware rendering keys off `TypeCategory` on the column, not off the value.
   - Session `TimeZone` is set to the OS IANA zone on session open, so timestamptz text is local time.
   - `result_series` is the one exception. It parses a chart's y column from the stored text into `f64` in Rust, and sends `None` for NULL, text that does not parse, NaN, and infinity. The chart is the only reader of these numbers, and the grid still shows the text.
2. **Rust owns result rows. The webview pulls windows.** A `ResultSet` is an append-only text arena filled by the run task. The grid requests `resultPage(ref, start, len)`. Export writes the loaded rows straight from the arena, or streams a re-run of the result's source to the file (decision 4). The webview never holds more than a few pages, and the row-cap policy lives in one place. Sort and filter on a table tab do not touch the arena. They change the tab's `TableQuery` and re-run `TablePage`, so the engine sorts and filters. The value viewer is the `cell` overlay, opened from the grid. It reads a cut cell's full text with `result_cell` through `RowCache.loadFull`. The record view, the `record` overlay, reads one row at a time through the same `RowCache`.
3. **One dedicated session per tab, opened lazily, plus one metadata session per database the instance needs.** No pool. A connection is an `Instance`: its *home* session is on the configured database, and it reads the server database list on connect. Every other database gets a `DbSlot` with its own lazily opened metadata session, so each database has an independent catalog and metadata session. A slot exists exactly for the home database, the databases the stored `ConnectionView` shows, and the databases this connection's consoles target; `reconcile` converges the slots to that set after connect, `connection_edit_view`, `console_configure`, `console_close`, and a database-list re-read, and `catalog_get` refuses any other database with `NotShown`. The home database and the consoles' databases always open their slot; of the other shown databases, at most 20 do so automatically (`view::AUTO_OPEN_LIMIT`), and a shown database past that cap opens when the tree expands its row and calls `catalog_get`. `Instance::converge` computes the wanted set under the slot lock, so a reconcile that started before a view edit cannot drop the slot the edit's own reconcile created. The instance's lock order is documented on `Instance` in `connection.rs`; `Instance::state()` takes every lock under `Shared::connections`, so no path takes one of them before `slots`. The home session's death marks the connection `Lost`; another slot's death fails only that database, and the next load reopens it. A PostgreSQL slot's session also serves `exact_row_count` for its database. A SQL Server metadata session sets `DEADLOCK_PRIORITY LOW`, and it makes up to five attempts at a read that loses a deadlock (error 1205): the first run and four re-runs. Its catalog reads hold shared locks on system rows, so concurrent DDL forms lock cycles with them. The user's DDL always wins the cycle, and the refresh usually lands without a Retry. Under sustained DDL all five attempts can lose, and the database then shows Failed with the deadlock message, as it did before. SQL Server counts use the named tab's native session. Session state (search_path, temp tables, GUCs, transactions) belongs to the tab, which is what an IDE user expects.
4. **During a run, a SQL Server exact count, an all-rows export, or a CSV import, the session moves into the task.** The console's session slot stays `Busy` until the task gives it back. `Busy` carries its work (`Run`, `Count`, `Transaction`, `Edit`, `Probe`, `Export`, or `Import`); each revisioned `ConsoleState.work` derives from that ownership, and is `None` outside `Busy`. Commit, rollback, another run, or another SQL Server count while it is gone returns `Busy`. Concurrent use of one session cannot be expressed.
   - **An all-rows export runs the result's source again on the tab's session.** Each `ResultSet` keeps an immutable `Origin`, set in `run_outputs`: the SQL that may run again (a table page's browse without its window, or the statement), its position in a batch, and the schema it ran under. `export::rerun` decides once, when the statement runs, whether that SQL may run twice: a PostgreSQL statement must be a read, and one whose calls `sql::vet` cannot vouch for needs the user's confirmation; a T-SQL batch must pass `tsql::rerun_refusal`. `ResultMeta.allRows` derives from the `Origin`, so the frontend never decides. `result_export(ref, format, All, path)` takes the session as `Work::Export(progress)`, so `ConsoleState.export` is `Some` exactly while `work` is `Export`. PostgreSQL wraps the re-run in `BEGIN TRANSACTION READ ONLY`, or inside an open transaction in `SAVEPOINT` with `SET LOCAL transaction_read_only = on`, and always rolls it back, so the tab's transaction and search path are as they were. Rows go through the same `Encoder` the loaded rows use, then through a bounded channel to a blocking writer that holds a `files::Replacement`, so the target is replaced only by a complete file. A disk error cancels the statement on PostgreSQL; on SQL Server, where a cancel resets the session, the task reads the batch to its end and reports `ExportProgress.writeError`. The grid, `RunState`, and history stay untouched, and one Output line records the export.
5. **Snapshots over deltas.** The backend emits whole `ConsoleState` and `ConnectionState` values with a monotonically increasing `rev`. The frontend's live store is a pure projection: `apply(event)` keeps the higher rev. This is idempotent and tolerant of event order. Only `ConsoleOutput`, the append-only output log, uses deltas.
6. **Dialect-specific SQL lexers, in Rust.** The PostgreSQL `split_statements` and `classify` are pure functions. They handle dollar quotes, `E''` strings, nested block comments, and quoted identifiers. A statement ends at a top-level `;`, or at a blank line when the text before it is a complete statement (a `WITH`, `INSERT`, or `EXPLAIN` has reached its body, an `UPDATE` its `SET`, and the last token is not `,`, `(`, an operator, or a clause keyword such as `UNION` or `AS`) and the next word starts a statement (`SELECT`, `INSERT`, `CREATE`, and the other statement verbs, but not `END`), as DataGrip and DBeaver do. A cursor picks the statement it sits in, including a blank line inside one statement, or else a statement on its own line. A cursor on a blank line or a comment-only line between statements picks nothing. They drive statement picking, transaction-state tracking, DDL-triggered catalog refresh, and synthesized command tags. The frontend sends the whole document plus a cursor or selection. All offsets crossing IPC are UTF-16 code units (CodeMirror's unit), and the conversion happens once at the boundary.
7. **One full-catalog snapshot per database, keyed by `DatabaseRef { connectionId, database }`.** PostgreSQL loads it in eight queries, and a schema whose relations load takes its `tableCount` from those relations, so DDL between two of the queries cannot make the count disagree with the list; SQL Server uses its own system catalogs. It lists every schema except `pg_toast` and temp schemas with a `system` flag (`information_schema` or the reserved `pg_` prefix) and a `tableCount` (tables, partitioned tables, and foreign tables, the number the tree and the schema picker both show), names the database's `defaultSchema` (the first existing non-system entry of `current_schemas(false)`, else `public`, else the first non-system schema; a role whose search path starts with `pg_catalog` still defaults to a user schema), and carries relations with `reltuples`, columns with their `TypeCategory` and PK and FK flags, functions, sequences, and synonyms for the non-system schemas, and types for every schema whose relations loaded. A checked system schema loads its types with its relations. Functions, sequences, and synonyms never load for a system schema (`pg_catalog` alone has about 3,000 functions). Extensions load with the database on PostgreSQL. SQL Server loads sequences, synonyms, and user types, and its extension list stays empty. Defaults, constraints, indexes, and triggers are not in it: `relation_detail(db, oid)` reads them for one relation when the tree opens it, so a DDL refresh never scans `pg_constraint`, `pg_index`, or `pg_trigger` for the whole database, and the frontend refetches an open relation's detail when the catalog version moves. The tree and its search filter, CodeMirror completion, and the PK lookups for table pages and staged edits are all derived from the catalog of the database they address; OIDs are never compared across databases. The connection snapshot is the single source of catalog versions: `ConnStatus::Connected.instance` lists every database with its `CatalogStatus` (`Idle`, `Loading`, `Ready { version }`, `Failed`), and the frontend fetches `catalog_get(db)` when the served version is newer than what it holds. `Failed` carries the last good catalog's version, and `catalog_get` keeps serving that catalog, so a failed reload shows its error in the tree beside the schemas it already had, with a Retry; the frontend drops only views whose database serves no version (`Idle`, `Loading`, or a `Failed` first load). Versions come from one engine-wide counter, so a reconnect never reissues one. DDL schedules a refresh of the console's database; a slot's `RefreshGate` coalesces a burst into at most two loads.
8. **Contract generation.** Every IPC type lives in `crates/engine` with `#[derive(Serialize, Deserialize, specta::Type)]`. `src-tauri` registers commands with `tauri-specta` and adds `AppEvent` as a plain exported type; the shell forwards `Engine::subscribe()` with `app.emit("app-event", ev)`, which sidesteps the release-candidate event API. Debug builds export `src/ipc/bindings.ts`, and a test fails if the checked-in file differs.
9. **SQL text is interpreted only in Rust.** That includes table-browse SQL: `RunRequest::TablePage` makes the engine build `SELECT * FROM "schema"."table" WHERE <filters> ORDER BY <user order>, <pk> LIMIT n OFFSET m` with its own identifier and literal quoting and the catalog's PK columns. The user's `WHERE` and `ORDER BY` text goes in as written, and the engine refuses the page if the result isn't one statement. Staged row edits go through `table_apply`, which builds the `INSERT`, `UPDATE`, and `DELETE` statements the same way.
10. **The assistant drives the user's own agent CLI as engine-owned child processes, one per turn.** The engine starts the installed `claude` under the user's sign-in, so a chat runs on the user's Claude subscription and L1 Data holds no API key. The Usage popup reads the subscription limits left the same way, on demand: the engine asks the user's `claude` with a `get_usage` control request and never reads a token itself. The process starts with `--setting-sources=`, `--strict-mcp-config`, and `--tools=`, so the user's settings, hooks, plugins, `CLAUDE.md`, and MCP servers stay off, and so do the CLI's built-in tools. In Full access the model gets three tools from an in-process MCP server named `l1`: `describe_tables`, `search_catalog`, and `run_query`. The turn task owns its process the way a run task owns a session. `ChatInner.turn` holds the running turn, and a second send while it is set returns `SendError::Busy`, so two turns cannot share a process. When the turn ends, it publishes its End and the chat goes Idle at once; the process closes on its own task, which closes its stdin, gives it 2 s to exit before it sends `SIGKILL` to the process group, and ends once the process is reaped. `ChatInner.closing` keeps that task's handle, and the next turn waits for it, bounded at 3 s, before it starts its own process with `--resume <session id>`, so the two never overlap; delete and quit wait for it the same way. Quit stops every turn through `Engine::shutdown`, which `main.rs` calls on `RunEvent::Exit` and the dev-bridge calls on Ctrl-C. It sends Stop with a 1 s grace, waits 3 s for the turn tasks, aborts what is left, kills every registered process group on its own thread, and waits 1 s more; `main.rs` bounds the whole call at 6 s.
   - **Assistant access is global and defaults to schema only.** `Settings.assistantAccess` has a manual stored fallback to `SchemaOnly`. `Shared.access` owns a persisted mode and generation, and admission captures an `AccessLease`. Every access change replaces the generation, denies pending approvals, and stops active turns. `Full` to `SchemaOnly` to `Full` cannot revive a lease. Each provider pipe write polls under the same short synchronous lock as the setting change, rechecking the lease at every partial write. A blocked pipe releases that lock. Revocation drops the active attempt and its in-flight tools, so a queued prompt, approval, or result cannot finish delivery after a switch.
   - **Schema only restricts engine boundaries.** It offers only `describe_tables` and `search_catalog`, refuses `run_query` before ledger reuse at permission and execution, and discards attachments before reading or storing rows. Automatic context omits editor SQL, selected statements, tab titles, last-run errors, and rows. Schema formatting omits row estimates, function argument strings containing defaults, and index expressions and predicates. Connection and metadata failures return generic messages to the provider. Database and dialect, structural names, columns, types, and keys remain available. User-typed or pasted chat text is always sent. Full access preserves query vetting, approvals, isolated PostgreSQL `READ ONLY` sessions, and SQL Server query refusal.
   - **Provider context records access provenance.** Each saved chat and provider session carries its access lease. A changed or missing provenance clears the provider session and sets the chat's persisted `recapFloor` to the next entry sequence at admission. Local history stays visible, but neither resume nor recap includes entries before that boundary. The global generation is saved separately from Settings so the webview cannot restore it from a stale snapshot. Same-mode sessions can resume across engine restarts. A lost session or a provider config change recaps only entries at or above the floor.
   - **Approvals live in an engine ledger, checked at the ask and again at execution.** Each turn keeps one ledger keyed by the provider's tool-call id. The CLI's `can_use_tool` request consults it first, and that is where the user's wait for **Run** happens. The card, the ledger, and the allow reply's `updatedInput` all hold that request's input, which can differ from the earlier `tool_use` block's. The MCP `tools/call` consults it again, and a call runs only on a recorded `Run` whose input equals the approved input. A CLI that skips the ask, or calls with other arguments, still cannot run a query nobody approved, and a refused call hears why. After Stop, both consults deny at once. Catalog tools never ask. A `run_query` statement that is not a read is refused without asking. `sql::vet` decides what may run unattended and fails closed: a read is `Clear` only when it has no `U&` escape and no `SELECT … INTO`, every function it calls, bare or `pg_catalog.`-qualified, is on the vetted allowlist of built-ins, and so is every operator it uses, bare or as `OPERATOR(pg_catalog.op)`, since an operator is a function. **Ask first** asks for every query, **Auto** asks for anything not `Clear`, and both name what `vet` found through `ApprovalReason::Unvetted`. Views, functions, and casts to or from types defined in the database are not inspected, so the user's role is the last limit. A permission change applies from the next tool call.
   - **The model's queries run on their own READ ONLY sessions.** `assistant::tools::run_query` opens a fresh session for each call, with `default_transaction_read_only = on`. It runs `BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY` with a 30 s `statement_timeout` and a 5 s `lock_timeout`, and `prepare` refuses a second statement. It reads text rows through the simple protocol, keeps at most 50 rows and 8 KB of cells with each cell cut at 200 chars, stops before a row that would cross either cap, cuts the cells of a first row that would cross the bytes alone to an equal share of them, and drops the session without a commit. It never borrows a tab's session or a metadata session, so a tab's open transaction, `SET`s, and temp tables stay out of reach, and a session-level `set_config` or advisory lock dies with the call. `describe_tables` runs no model text. It reads keys and indexes with the engine's own OID-scoped detail queries on the database's metadata session, as the tree does.
   - **Claude's protocol stays behind a sans-IO codec in `provider/`.** `provider/claude.rs` turns stream-json lines into provider-neutral `AgentEvent`s and builds the lines to write back, with no pipes, tasks, or clock. `provider/agent.rs` drives the process. `assistant/` sees only the `Provider` and `Codec` traits, so a second provider is a new `ProviderKind` and a new file in `provider/`. One structure test fails if a file under `assistant/` names a provider or its wire format. Another fails if any file there except `tools.rs` calls a session's client.
11. **The engine runs the updater, and the shell only moves bytes.** `crates/engine/src/update/` owns the update state machine, the check schedule, the offer policy, the saved track, and the restart impact. `update/machine.rs` is pure. It holds one `Phase` enum and derives the wire `UpdateState` from it with a rising `rev`, so a change cannot forget to publish. The shell implements `UpdateTransport` in `src-tauri/src/updater.rs`, the only file that names `tauri_plugin_updater`. It reports whatever a feed offers, downloads and verifies, installs, and relaunches through `request_restart`, so `RunEvent::Exit` runs `Engine::shutdown` as a quit does. Three trait objects make the order a type fact. `fetch` returns a `Release`, only a `Release` downloads into a `Verified`, and only a `Verified` installs, and each step consumes its input. The transport's version comparator accepts every version, which turns off the plugin's only guard against a feed that pairs a new version number with an old, validly signed build. `tauri.conf.json` therefore sets `requireSignedVersion`, so the version the engine judges is the version the signature binds. The webview gets no updater permission and no updater JS package. Structure tests in `src-tauri` check all three.
   - **The track is saved in kv `updates`, not in `Settings`.** The webview writes `Settings` whole, so a stale copy of the track there would switch the track back on the next unrelated settings change. Only `update_set_track` writes it, which also lets a switch answer `Busy` during a download or an install. When kv `updates` is missing, a build that can update saves the track its version implies, Nightly for a version with `-nightly.`, so a nightly install that the Nightly feed later moves onto a stable release stays on Nightly. A build that cannot update saves nothing, because a debug build shares the installed app's data directory. A saved track this build cannot read, such as one a newer build wrote, is logged and left as saved.
   - **The downgrade rule is derived on every check.** Only a nightly build on the Stable track takes an older version, so switching to Stable installs the latest stable. Every other pair of build and track takes only a strictly newer version by semver precedence, which orders `0.1.1-nightly.20261023.5` below `0.1.1-nightly.20261024.6` below `0.1.1`. Nothing is remembered, so a check that failed or was ignored after the switch, or a restart in between, cannot leave the user on the old track.
   - **Consent is checked again where the install runs.** `update_install(version, accepted)` first answers `Confirm` with a `RestartImpact`: the tabs with work in flight other than the engine's own `has_writes` probe, the open transactions probed as `disconnect` probes them, and the chats with a running turn. When the confirm comes back, the engine computes the impact again and installs only if the acceptance still covers it. Otherwise it answers `Confirm` with the longer list. This is the approval ledger's rule applied to a restart. Staged table edits live only in the webview, so `updateActions.ts` adds the tabs that hold them to the dialog. On confirm it reads them again, and if a tab gained edits it sends no acceptance, so the engine answers `Confirm` and the dialog reopens. After the install is accepted, the installer can wait on a macOS admin prompt. While the status is `Installing`, `InstallingOverlay` covers the window, holds focus, and swallows every keydown in a window capture listener, so no edit, query, or transaction starts that the relaunch would end unasked. The engine has already stopped the assistant's turns, and all other new work starts in the UI, so the engine has no gate of its own. The overlay leaves when the status does, so a `Failed` install hands the window back. The install stops the assistant's turns first, because Windows exits inside the plugin's `install` without running the shell's exit handler.
   - **Every step names the version the user saw.** `update_download` and `update_install` take the version. A repeat of a step already under way returns Ok and does nothing, and a step the update has moved past returns `Stale`, so a click that lands after a background change never acts on a state the user did not see. A check is bounded at 30 s and a download at 300 s. A track switch during a check is allowed, and the check's ticket drops its late result. Nothing downloads without a click, and a held release is never replaced by a background check.
   - **The change list is display only.** The release pipeline adds `changes` to each `latest.json`, beside `version` and `notes`: this release and up to 29 earlier releases on its channel, newest first, each a `version` and the `items` parsed from that release's notes, each item a `title` with an `author` and a `pr` number that can be null. The plugin does not read the field, so `Release::feed` hands the engine the feed's whole JSON and `TauriRelease` returns the plugin's `raw_json`. `machine::feed_changes` reads it leniently: a missing or malformed field yields no changes, and an entry that fails to parse or has no semver version is dropped. It never fails a check or blocks an update. The machine keeps the releases with the running version < version <= the offered version, newest first, so a downgrade lists none, and the `Offer` carries them through `Available`, `Downloading`, and `Downloaded`. The update button's hover card shows them as plain text, with no links.
   - **Debug builds report `UpdatesOff::DevBuild` and never check.** The dev-bridge and the tests pass a `ScriptedTransport`, which serves scripted feeds, so they run the real machine and policy rather than canned states.

### Rust types (crates/engine/src/model.rs, the IPC contract)

```rust
// ---- identity ---------------------------------------------------------------
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
pub struct ConnectionId(pub Uuid);
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
pub struct TabId(pub Uuid);           // minted by the frontend (crypto.randomUUID)
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
pub struct RunId(pub u64);            // engine-wide monotonic
/// UTF-16 code-unit offset into the editor document as sent in RunRequest.text.
#[derive(Clone, Copy, Serialize, Deserialize, Type)] pub struct DocOffset(pub u32);
#[derive(Clone, Copy, Serialize, Deserialize, Type)] pub struct DocRange { pub from: DocOffset, pub to: DocOffset }

// ---- connection config --------------------------------------------------------
#[derive(Clone, Serialize, Deserialize, Type)]
pub enum SslMode { Disable, Prefer, Require, VerifyCa, VerifyFull }

/// Persisted (SQLite). Never contains the password.
#[derive(Clone, Serialize, Type)]
pub struct ConnectionConfig {
    pub id: ConnectionId, pub name: String, pub host: String, pub port: u16,
    pub database: String, pub user: String, pub ssl_mode: SslMode,
    pub ca_cert_path: Option<String>, pub has_password: bool, pub color: Option<String>,
}
/// Edit semantics for the secret are explicit; the frontend never reads a saved password back.
/// `connection_parse_url` serializes a draft, so its `Set` only echoes a password the user typed in a URL.
#[derive(Serialize, Deserialize, Type)]
pub enum PasswordInput { Keep, Set(String), Clear }
#[derive(Serialize, Deserialize, Type)]
pub struct ConnectionDraft {
    pub id: Option<ConnectionId>,      // None = create
    pub name: String, pub host: String, pub port: u16, pub database: String,
    pub user: String, pub ssl_mode: SslMode, pub ca_cert_path: Option<String>,
    pub password: PasswordInput, pub color: Option<String>,
}

// ---- live connection --------------------------------------------------------
#[derive(Clone, Serialize, Type)]
pub struct ServerInfo { pub version: String /* "16.4" */, pub version_num: u32, pub encoding: String }

#[derive(Clone, Serialize, Type)]
#[serde(tag = "kind")]
pub enum ConnStatus {
    Disconnected,
    Connecting,
    Connected { server: ServerInfo, instance: InstanceState },
    Failed { error: ConnectError },          // connect attempt failed
    Lost { reason: String },                 // the home database's metadata session died after being Connected
}
/// Every `pg_database` row with the state of its catalog. The single source of catalog versions.
#[derive(Clone, Serialize, Type)]
pub struct InstanceState { pub home: String, pub show_system_databases: bool, pub databases: Vec<DatabaseState> }
#[derive(Clone, Serialize, Type)]
pub struct DatabaseState { pub name: String, pub system: bool, pub access: DbAccess, pub shown: bool, pub show_system_schemas: bool, pub shown_schemas: Vec<String>, pub schema_count: SchemaCount, pub catalog: CatalogStatus }
#[derive(Clone, Serialize, Type)]
#[serde(tag = "kind")]
pub enum SchemaCount { Unknown, Probing, Known { count: u32 }, Unavailable { message: String } }  // non-system schemas, from the catalog or a probe
#[derive(Deserialize, Type)]
#[serde(tag = "kind")]
pub enum ViewEdit {  // picker intents; the engine resolves them and is the only writer of the stored view
    Databases { preset: Preset }, Database { name: String, shown: bool }, ShowSystemDatabases { show: bool },
    Schemas { database: String, preset: Preset }, Schema { database: String, name: String, shown: bool }, ShowSystemSchemas { database: String, show: bool },
}
#[derive(Clone, Copy, Deserialize, Type)] pub enum Preset { All, DefaultOnly, None }
#[derive(Clone, Copy, Serialize, Type)] pub enum DbAccess { Ok, NoConnections, NoPrivilege }
#[derive(Clone, Serialize, Type)]
#[serde(tag = "kind")]
pub enum CatalogStatus { Idle, Loading, Ready { version: u64 }, Failed { message: String, version: Option<u64> } }  // Failed.version: the last good catalog, still served
#[derive(Clone, Serialize, Type)]
pub struct ConnectionState { pub id: ConnectionId, pub rev: u64, pub status: ConnStatus }

#[derive(Clone, Serialize, Type)]
#[serde(tag = "kind")]
pub enum ConnectError {
    Network { message: String }, Tls { message: String },
    Auth { message: String }, MissingPassword, Server(QueryError), Keychain { message: String },
}

// ---- catalog (one snapshot per database) ---------------------------------------
/// The key for every catalog read, refresh, and count. `database` is `pg_database.datname`.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
pub struct DatabaseRef { pub connection_id: ConnectionId, pub database: String }
#[derive(Clone, Serialize, Type)]
pub struct Catalog { pub db: DatabaseRef, pub version: u64, pub schemas: Vec<SchemaInfo> }
#[derive(Clone, Serialize, Type)]
pub struct SchemaInfo { pub name: String, pub relations: Vec<Relation>, pub functions: Vec<FunctionInfo> }
#[derive(Clone, Serialize, Type)]
pub enum RelKind { Table, PartitionedTable, View, MaterializedView, ForeignTable }
#[derive(Clone, Serialize, Type)]
pub struct Relation {
    pub oid: u32, pub name: String, pub kind: RelKind,
    pub row_estimate: Option<i64>,         // None when reltuples = -1 (never analyzed)
    pub columns: Vec<ColumnInfo>,          // attnum order
}
#[derive(Clone, Serialize, Type)]
pub struct ColumnInfo { pub attnum: i16, pub name: String, pub type_display: String, pub category: TypeCategory, pub not_null: bool, pub pk: bool, pub fk: bool }
#[derive(Clone, Serialize, Type)]
pub struct FunctionInfo { pub oid: u32, pub name: String, pub args: String, pub returns: String, pub kind: FnKind }
#[derive(Clone, Serialize, Type)] pub enum FnKind { Function, Procedure, Aggregate, Window }

// ---- console (one per tab) -------------------------------------------------------
#[derive(Clone, Copy, PartialEq, Serialize, Deserialize, Type)]
pub enum TxnMode { AutoCommit, Manual }

#[derive(Clone, Deserialize, Type)]
pub struct ConsoleConfig { pub connection_id: ConnectionId, pub database: String, pub schema: Option<String>, pub mode: TxnMode }

#[derive(Clone, Copy, PartialEq, Serialize, Type)]
pub enum TxnState { Idle, Active, Open, Aborted }   // Aborted: only ROLLBACK [TO SAVEPOINT] is accepted

#[derive(Clone, Serialize, Type)]
#[serde(tag = "kind")]
pub enum SessionState { None, Open { backend_pid: i32 }, Lost { reason: String, transaction: TransactionLoss } }
pub enum TransactionLoss { None, RolledBack, Unknown }

#[derive(Clone, Serialize, Type)]
pub struct ConsoleState {
    pub tab_id: TabId, pub rev: u64, pub config: ConsoleConfigView,
    pub session: SessionState, pub txn: TxnState, pub run: RunState,
}
#[derive(Clone, Serialize, Type)]
pub struct ConsoleConfigView { pub connection_id: ConnectionId, pub database: String, pub schema: Option<String>, pub mode: TxnMode }

#[derive(Clone, Serialize, Type)]
#[serde(tag = "kind")]
pub enum RunState {
    Idle,
    Running  { run_id: RunId, started_at_ms: i64, statements: Vec<StatementRun> },
    Finished { run_id: RunId, elapsed_ms: u32, statements: Vec<StatementRun>, outcome: RunOutcome },
}
#[derive(Clone, Copy, Serialize, Type)] pub enum RunOutcome { Ok, Failed, Cancelled }

#[derive(Clone, Serialize, Type)]
pub struct StatementRun { pub index: u16, pub range: DocRange, pub status: StmtStatus }

#[derive(Clone, Serialize, Type)]
#[serde(tag = "kind")]
pub enum StmtStatus {
    Pending,
    Preparing,
    Rows { result: ResultMeta, done: bool },             // done=false while streaming
    Command { tag: String /* "UPDATE 3" */, elapsed_ms: u32 },
    Failed { error: QueryError },
    Cancelled,
    Skipped,                                             // an earlier statement failed or was cancelled
}

// ---- results ------------------------------------------------------------------
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
pub struct ResultRef { pub tab_id: TabId, pub run_id: RunId, pub index: u16 }

#[derive(Clone, Serialize, Type)]
pub struct ResultMeta {
    pub r#ref: ResultRef,
    pub db: DatabaseRef,         // the database the rows came from; the grid's PK lookup keys on it, not on the tab's current target
    pub columns: Vec<ColumnMeta>,
    pub row_count: u32,          // rows stored and pageable
    pub rows_seen: u64,          // rows the server sent (> row_count when truncated)
    pub truncated: Option<TruncateReason>,
    pub elapsed_ms: u32,
}
#[derive(Clone, Copy, Serialize, Type)] pub enum TruncateReason { RowLimit, ByteLimit }

#[derive(Clone, Serialize, Type)]
pub struct ColumnMeta {
    pub name: String,
    pub r#type: PgTypeRef,
    pub source: Option<ColumnSource>,  // table_oid + attnum: the grid's PK icon (isPrimaryKey in state/catalog.ts) and foreign key follow; set on PostgreSQL results that read a table column and on SQL Server table pages
}
#[derive(Clone, Serialize, Type)]
pub struct PgTypeRef { pub oid: u32, pub display: String /* "bigint", "integer[]", "order_status" */, pub category: TypeCategory }
#[derive(Clone, Copy, Serialize, Type)]
pub enum TypeCategory { Integer /* int2, int4, int8, oid */, Numeric, Text, Bool, Temporal, Json, Binary, Uuid, Network, Array, Enum, Composite, Range, Other }
#[derive(Clone, Copy, Serialize, Type)] pub struct ColumnSource { pub table_oid: u32, pub attnum: i16 }

/// Row-major page. Cells longer than `cell_char_limit` (default 2,000) are cut; their
/// coordinates are listed so the UI can mark them and fetch the full value on demand.
#[derive(Serialize, Type)]
pub struct RowPage { pub start: u32, pub rows: Vec<Vec<Option<String>>>, pub cut: Vec<(u32, u16)> }

#[derive(Clone, Serialize, Type)]
pub struct QueryError {
    pub severity: String, pub sqlstate: Option<String>, pub message: String,
    pub detail: Option<String>, pub hint: Option<String>, pub context: Option<String>,
    pub position: Option<DocOffset>,   // mapped from PG's 1-based char position into the document
}

// ---- output log (append-only) -------------------------------------------------
// Timestamped history of what the tab's session ran. Statement outcomes also live in
// ConsoleState.run: the snapshot is the current status, the log is the history.
#[derive(Clone, Serialize, Type)]
pub struct OutputEntry { pub at_ms: i64, pub run_id: RunId, pub body: OutputBody }  // the run, commit, rollback, or apply that held the session
#[derive(Clone, Serialize, Type)]
#[serde(tag = "kind")]
pub enum OutputBody {
    Statement { statement: u16, text: String },   // at statement start; text capped at 2,000 chars + "…"
    Rows { statement: u16, rows: u64, elapsed_ms: u32, truncated: bool },
    Command { statement: u16, tag: String, elapsed_ms: u32 },
    Notice { severity: String, message: String, detail: Option<String> },
    Error { statement: Option<u16>, error: QueryError },  // None: search_path, table_apply's transaction
    Info { message: String },          // "BEGIN (implicit)", "Session reset: connection lost (…)"
}

// ---- history, workspace, settings -------------------------------------------------
#[derive(Clone, Serialize, Type)]
pub struct HistoryEntry {
    pub id: i64, pub at_ms: i64, pub connection_id: ConnectionId, pub connection_name: String,
    pub database: String, pub sql: String, pub elapsed_ms: u32, pub outcome: HistoryOutcome,
}
#[derive(Clone, Serialize, Type)]
#[serde(tag = "kind")]
pub enum HistoryOutcome { Rows { rows: u64 }, Command { tag: String }, Error { message: String }, Cancelled }
#[derive(Deserialize, Type)]
pub struct HistoryQuery { pub text: Option<String>, pub connection_id: Option<ConnectionId>, pub before_id: Option<i64>, pub limit: u16 }
#[derive(Serialize, Type)] pub struct HistoryPage { pub entries: Vec<HistoryEntry>, pub has_more: bool }

/// Owned by the frontend, stored verbatim by the engine (single writer: the UI).
#[derive(Clone, Serialize, Deserialize, Type)]
pub struct Workspace { pub tabs: Vec<TabSnapshot>, pub active: Option<TabId>, pub layout: Layout }
#[derive(Clone, Serialize, Type)]
pub struct TabSnapshot { pub id: TabId, pub title: String, pub text: Option<String>, pub console: ConsoleConfigSnapshot, pub table: Option<TableSnapshot> }
#[derive(Clone, Serialize, Deserialize, Type)]
pub struct TableSnapshot { pub schema: String, pub table: String, pub page: u32, pub query: TableQuery }
// TabSnapshot has a hand-written Deserialize. It also loads the older `{"kind":"Sql"}` and `{"kind":"Table"}` shapes.
// An old table tab loads as an AutoCommit tab titled with its table, with `text: None`. Boot seeds that editor with `browseQuery`.
#[derive(Clone, Serialize, Deserialize, Type)]
pub struct ConsoleConfigSnapshot { pub connection_id: Option<ConnectionId>, pub database: Option<String>, pub schema: Option<String>, pub mode: TxnMode }
// `database` is `None` only in workspaces saved before tabs carried one; `tabFromSnapshot` resolves it to the connection's configured database.
#[derive(Clone, Serialize, Type)]
pub struct Layout {
    pub sidebar: bool, pub results: bool, pub right_panel: Option<RightPanel>, pub results_height: u16,
    pub sidebar_width: u16 /* 256 */, pub right_panel_width: u16 /* 341 */,
    pub assistant_chat: Option<ChatId>,   // the chat the assistant panel shows, reopened after a restart
}
// Layout has a hand-written Deserialize: workspaces saved before the widths or `assistant_chat` existed load with the defaults.
#[derive(Clone, Serialize, Deserialize, Type)] pub enum RightPanel { Assistant }
// Layout's deserializer reads a saved Schema or History panel, from older builds, as Assistant.

#[derive(Clone, Serialize, Type)]
pub struct Settings {
    pub assistant_access: AssistantAccess, // SchemaOnly by default, including older stored settings
    pub max_rows: u32 /* 200_000 */, pub max_result_bytes: u64 /* 512 MiB */, pub table_page_size: u32 /* 500 */, pub stop_on_error: bool, pub theme: Theme /* System */,
    pub providers: Vec<ProviderConfig>,    // one per ProviderKind::ALL, in that order
    pub provider_health_check_secs: u32,   // 300; 0 turns the background check off
}
#[derive(Clone, Copy, Default, Serialize, Deserialize, Type)] pub enum AssistantAccess { #[default] SchemaOnly, Full }
#[derive(Clone, Copy, Default, Serialize, Deserialize, Type)] pub enum Theme { #[default] System, Light, Dark }
// Settings has a hand-written Deserialize: settings saved before `theme` existed load with Theme::System,
// and settings saved before `providers` existed load the Claude default.

#[derive(Serialize, Type)]
pub struct BootSnapshot {
    pub connections: Vec<ConnectionConfig>, pub live: Vec<ConnectionState>,
    pub unreadable_connections: u32,                                // saved rows that do not parse, left out of connections
    pub consoles: Vec<ConsoleState>, pub workspace: Workspace, pub settings: Settings,
    pub providers: Vec<ProviderState>, pub chats: Vec<ChatState>,   // chats: stored, newest first, at most 200
    pub updates: UpdateState,
    pub saved_queries: SavedQueries,                                // every readable saved query, most recently used first
}

// ---- the one event (emitted as "app-event") -------------------------------------
#[derive(Clone, Serialize, Type)]
#[serde(tag = "kind")]
pub enum AppEvent {
    ConnectionChanged(ConnectionState),                 // also carries every catalog version, in InstanceState
    ConsoleChanged(ConsoleState),                       // throttled to <= 10/s per console while streaming
    ConsoleOutput { tab_id: TabId, entries: Vec<OutputEntry> },
    HistoryAppended(HistoryEntry),
    ProviderChanged(ProviderState),
    ChatChanged(ChatState),
    ChatEntryChanged(EntrySnapshot),                    // coalesced to <= 20/s per chat while a text block streams
    UpdateChanged(UpdateState),
    SavedQueriesChanged(SavedQueries),                  // the whole list, after a write that changed a row
}
```

#### Assistant types

The assistant's contract is in the same file. This excerpt shows the shapes a reader needs. `model.rs` also defines the id newtypes (`ChatId`, `ApprovalId`, `ToolCallId`, `ModelId`, `EffortId`), `ProviderInfo`, `ModelOption`, the reply and tool payloads, and the error enums. The panel follows the three assistant variants of `AppEvent`: `ProviderChanged`, `ChatChanged`, and `ChatEntryChanged`. Each carries a whole snapshot with a rising `rev`. Chats and entries share one engine-wide counter.

```rust
// ---- providers ------------------------------------------------------------------
pub enum ProviderKind { Claude }      // ProviderKind::ALL; provider(kind) in provider/mod.rs matches it exhaustively
/// Saved in Settings.providers, one per kind. Hand-written Deserialize: a missing field takes default_config(provider).
pub struct ProviderConfig {
    pub provider: ProviderKind, pub enabled: bool, pub display_name: String,
    pub binary_path: String,          // "" = `claude` on the agent PATH; `~` expands
    pub config_dir: String,           // "" = the inherited CLAUDE_CONFIG_DIR, else ~/.claude
    pub launch_args: String,          // as typed; the engine drops the flags it owns
}
pub struct ProviderState {
    pub provider: ProviderKind,       // not `kind`: AppEvent is tagged on kind (model::tests::provider_changed_keeps_its_own_tag)
    pub rev: u64,
    pub info: ProviderInfo,           // default name and binary, config variable, sign-in command, owned flags
    pub ignored_flags: Vec<String>,   // the owned flags found in the saved launch arguments, as typed
    pub checking: bool,               // a probe runs; `outcome` stays on screen meanwhile
    pub checked_at_ms: Option<i64>,
    pub outcome: Option<ProbeOutcome>,   // None until the first probe finishes; kept in memory only
}
#[serde(tag = "kind")]
pub enum ProbeOutcome {
    Ready { version: String, plan: Option<String> /* "Claude Max" */, models: Vec<ModelOption> },   // each model lists its effort levels
    SignedOut { version: String },
    NotFound { binary: String },      // what was looked up, and where
    Broken { version: Option<String>, message: String },
}

// ---- usage limits (pulled by usage_limits, never saved) ------------------------
pub struct LimitsReport { pub checked_at_ms: i64, pub sources: Vec<SourceLimits> /* one per ProviderKind::ALL */ }
pub struct SourceLimits { pub source: ProviderKind, pub name: String /* the provider's display name */, pub outcome: LimitsOutcome }
#[serde(tag = "kind")]
pub enum LimitsOutcome {
    Ready { plan: Option<String> /* "Max" */, windows: Vec<LimitWindow> },
    NoLimits { plan: Option<String> },   // signed in with an API key or a cloud provider
    SignedOut { sign_in_command: String },
    NotFound { binary: String },
    Failed { message: String },          // started, but the exchange failed, timed out at 8 s, or answered a shape the parser cannot read
}
pub struct LimitWindow {
    pub id: String,                   // "session", "weekly_scoped:Opus"
    pub label: String,                // "Session", "Weekly · Opus"
    pub left_percent: u8,             // the engine inverts the CLIs' used percent once
    pub resets_at_ms: Option<i64>,
    pub level: LimitLevel,            // Normal | Low (20% left or less, or Claude's own severity) | Exhausted
}

// ---- chats ----------------------------------------------------------------------
pub struct ChatOptions { pub provider: ProviderKind, pub model: ModelId, pub effort: Option<EffortId>, pub permission: Permission /* AskFirst | Auto */ }
pub struct ChatState {
    pub id: ChatId,                   // minted by the frontend, like TabId
    pub rev: u64,
    pub title: String,                // the first line of the first message, cut at 60 chars; "" for a draft
    pub updated_at_ms: i64, pub options: ChatOptions, pub status: ChatStatus,
    pub entry_count: u32,             // entries are 0..entry_count; the live counter, bumped as entries are appended
}
#[serde(tag = "kind")]
pub enum ChatStatus {
    Draft,                            // opened, never sent to; not stored, not listed
    Idle,
    Running { turn: u32, started_at_ms: i64, phase: TurnPhase /* Starting | Working | AwaitingApproval */ },
    Deleted,                          // a tombstone, so the live store drops the chat by the keep-higher rule
}

/// One transcript row. `turn` is the seq of the user entry that opened the turn.
pub struct EntrySnapshot { pub chat_id: ChatId, pub seq: u32, pub rev: u64, pub turn: u32, pub entry: Entry }
/// Stored verbatim in chat_entries.json. A row this build cannot parse loads as Unreadable.
#[serde(tag = "kind")]
pub enum Entry {
    User { at_ms: i64, text: String, context: Option<SentContext>, attachment: Option<Attachment> },
    Text { blocks: Vec<ReplyBlock> /* Markdown { text } | Sql { sql, relations, vetted, complete } */, done: bool },
    Tool { call: ToolCallId, tool: ToolUse /* DescribeTables | SearchCatalog | RunQuery { sql, purpose, relations } | Other */, status: ToolStatus },
    Notice { text: String },          // the engine changed course, such as a provider session it could not resume
    End { at_ms: i64, outcome: TurnOutcome /* Completed | Stopped | Lost | Failed { error: AgentError } */, suggestions: Vec<String> },
    Unreadable,
}
#[serde(tag = "kind")]
pub enum ToolStatus {
    Running,
    AwaitingApproval { approval: ApprovalId, reason: ApprovalReason /* AskFirst | Unvetted { functions } */ },
    Done { result: ToolResult },
    Failed { message: String },       // what the model got
    Denied { stopped: bool },         // true when Stop denied it, false when the user pressed Deny
    Expired,                          // still open when the turn ended
}

// ---- requests -------------------------------------------------------------------
/// The active tab at Send. The engine reads the SQL out of `text` itself, at the UTF-16 `target`.
pub struct SendRequest { pub text: String, pub tab: Option<TabContext> }
pub struct TabContext { pub tab_id: TabId, pub title: String, pub db: Option<DatabaseRef>, pub text: String, pub target: RunTarget, pub attach: Option<ResultRef> }
```

#### Update types

The sidebar's update button and **Settings** > **General** follow `AppEvent::UpdateChanged`, a whole `UpdateState` with a rising `rev`, as the other panels do.

```rust
pub enum UpdateTrack { Stable, Nightly }               // saved in kv `updates`, never in Settings
pub struct UpdateState {                               // no `kind` field: AppEvent is tagged on kind
    pub rev: u64, pub track: UpdateTrack,
    pub current_version: String,                       // the version the shell reports
    pub status: UpdateStatus,
    pub checked_at_ms: Option<i64>,                    // a failed check leaves it as it was
}
#[serde(tag = "kind")]
pub enum UpdateStatus {
    Disabled { reason: UpdatesOff }, Idle, Checking, UpToDate,
    Available { version: String, downgrade: bool, changes: Vec<ReleaseChanges> },   // changes: (running, version], newest first
    Downloading { version: String, downgrade: bool, percent: Option<u8>, changes: Vec<ReleaseChanges> },
    Downloaded { version: String, downgrade: bool, changes: Vec<ReleaseChanges> },   // verified; the engine holds the bytes
    Installing { version: String },
    Failed { failure: UpdateFailure },
}
pub struct ReleaseChanges { pub version: String, pub items: Vec<ChangeItem> }   // read from the feed's `changes`, display only
pub struct ChangeItem { pub title: String, pub author: Option<String>, pub pr: Option<u32> }
pub enum UpdatesOff { DevBuild, NotAppImage, UnsupportedTarget, UnknownVersion }
#[serde(tag = "kind")]
pub enum UpdateFailure { Offline, BadSignature, ReadOnlyLocation, PermissionDenied, Other { message: String } }   // worded in src/ipc/errors.ts
pub struct RestartImpact { pub running: Vec<TabId>, pub transactions: Vec<OpenTxn>, pub chats: Vec<ChatId> }   // also the acceptance the frontend sends back
#[serde(tag = "kind")]
pub enum InstallReply { Confirm { impact: RestartImpact }, Restarting }
#[serde(tag = "kind")]
pub enum UpdateError { Stale, Busy }
```

### IPC command list (src-tauri/src/ipc.rs)

Each command is one line delegating to `Engine`. tauri-specta uses `ErrorHandlingMode::Result`, so TypeScript sees `{status:"ok",data} | {status:"error",error}`.

| Command | Request | Response | Notes |
|---|---|---|---|
| `app_boot` | – | `BootSnapshot` | Idempotent. Safe after a webview reload because live consoles survive. |
| `connection_save` | `ConnectionDraft` | `Result<ConnectionConfig, SaveError>` | Upsert. Seals a set password and writes it before the config row. Reads the keychain only when a password is set. |
| `connection_delete` | `ConnectionId` | `Result<(), DisconnectError>` | Disconnects (refuses on open txns), deletes the config row and sealed password together. Never touches the keychain. |
| `connection_test` | `ConnectionDraft` | `Result<ServerInfo, ConnectError>` | `PasswordInput::Keep` opens the sealed password, if one is stored. |
| `connection_parse_url` | `url: String` | `Result<ConnectionDraft, String>` | Pure parse of a `postgres://` URL or a `key=value` string into a draft with an empty name. A password in the URL comes back as `PasswordInput::Set`, otherwise `Clear`. The connection dialog's URL field calls it on each edit. Sync, and it never touches the runtime. |
| `ssh_trust_host_key` | `key: HostKeyId` | `Result<(), String>` | Appends the SSH host key a dial reported as `SshError::HostKeyUnknown` to `<data_dir>/known_hosts`. The engine holds each observed key for ten minutes, at most 32 at a time, and each id trusts once, so the webview can trust only a key the engine saw. Sync and std-only: it never touches the runtime. |
| `connection_connect` | `ConnectionId` | `Result<ServerInfo, ConnectError>` | Idempotent. Opens the home session and reads `pg_database`; catalogs load afterwards, each with its own status. |
| `connection_disconnect` | `ConnectionId, force: bool` | `Result<(), DisconnectError>` | `OpenTransactions { tabs: Vec<OpenTxn> }` unless forced. |
| `catalog_get` | `DatabaseRef` | `Result<Catalog, CatalogError>` | Whole snapshot of one database, loaded on first use. `NotShown` for a database neither shown nor targeted by a console. |
| `connection_refresh` | `ConnectionId` | `Result<(), CatalogError>` | The connection menu's Refresh: re-reads `pg_database`, clears the probe cache, reconciles the slots, and refreshes every loaded catalog. |
| `connection_edit_view` | `ConnectionId, ViewEdit` | `Result<(), CatalogError>` | One picker intent. Applied against the instance's current lists, persisted in `connection_views`, then the slots reconcile and the snapshot republishes. `NotConnected` while the connection has no instance, since a checkbox edit materializes the current selection from those lists. |
| `connection_probe_databases` | `ConnectionId, databases: Vec<String>` | `()` | Fire-and-forget. For each named database with no slot and no cached count, a short-lived session (at most 4 at a time, 3 s timeout) counts its non-system schemas; the snapshot shows `Probing`, then `Known` or `Unavailable`. The picker sends the rows it renders. A probe still waiting for a permit at disconnect opens no session. The command is sync, so it runs on the main thread in the desktop app: the engine spawns on the runtime handle it captured in `Engine::open`, never through `tokio::spawn`. |
| `catalog_refresh` | `DatabaseRef` | `Result<u64, CatalogError>` | Returns the new version. The connection snapshot carries it too. |
| `database_map_neighborhood` | `db: DatabaseRef, oid: u32, catalogVersion: u64, after: MapCursor?` | `Result<MapNeighborhood, MapError>` | Root and incident declared FKs, ordered column pairs, endpoint identities and participating columns. Pages 32 constraints before expanding pairs. Cursor binds database, root, refresh epoch and last constraint ID. Gone when the root is missing or invisible. |
| `database_map_query` | `db: DatabaseRef, text: String, cursor: DocOffset, catalogVersion: u64` | `Result<MapQuery, MapError>` | Lexer-backed cursor-statement analysis, UTF-16 ranges, distinct relation occurrences, catalog/tentative/unresolved matches and supported direct JOIN ON equality predicates. Executes no editor SQL. |
| `relation_detail` | `DatabaseRef, oid: u32` | `Result<RelationDetail, DetailError>` | Defaults, keys, indexes, and triggers of one relation, read on that database's metadata session with OID-scoped queries. Carries the catalog version it is at least as new as. `Gone` when the OID names no relation. |
| `object_script` | `DatabaseRef, target: ScriptTarget` | `Result<ObjectScript, DetailError>` | The DDL that creates a relation, routine, index, or trigger, read on that database's metadata session. `title` is the object's name and `.sql`. `warnings` names each property the text does not recreate, and the text opens with the same list as comments. `Gone` when the target names nothing. `Failed` names what cannot be scripted yet, such as an aggregate. |
| `exact_row_count` | `TabId, DatabaseRef, schema: String, table: String, TableQuery` | `Result<u64, CountError>` | Counts with the query's filters for the pager's Last button. PostgreSQL uses the metadata session. SQL Server uses the named tab's native session and checks the expected database target. |
| `console_configure` | `TabId, ConsoleConfig` | `Result<ConsoleState, ConfigureError>` | Upsert. `TransactionOpen` if mode, connection, or database change with a live txn. A connection or database change resets the session. A schema change issues `SET search_path` (applied on next run if busy). |
| `script_params` | `text, target, DatabaseKind` | `Result<Vec<Param>, RunError>` | Pure lexical discovery in the selected original units. No session or type inference. |
| `script_outline` | `text: String, cursor: DocOffset` | `ScriptOutline` | The UTF-16 range of each statement `split_statements` finds, and `atCursor`, the index of the one `pick` returns for a `Cursor` run at `cursor` (`null` only when there are none). The editor's run mark and the statement chooser call it, so neither splits SQL in TypeScript. Pure and sync, and it never touches the runtime. |
| `completion_context` | `text: String, cursor: DocOffset, databaseKind: DatabaseKind` | `CompletionContext` | `None` inside a string, a comment, a quoted name, a number, or a parameter. Otherwise where the typed word starts, what the clause expects (`Keyword`, `Alias`, `Relation`, `Expression`, or `JoinCondition` with the joined relation's index), the qualifier before a `.` resolved to a relation in scope or left as folded names, the FROM items of the query at the cursor and of each enclosing query with their aliases as written, the CTE names, the keywords that can follow, and the dialect's built-in functions where an expression stands. Pure and sync, and it never touches the runtime. |
| `editor_format` | `text: String, target: RunTarget` | `Result<FormatEdit, FormatError>` | Formats the statement a `Cursor` run would pick, every statement a selection touches, or the whole document, each statement on its own, and keeps the text between them. `FormatEdit { from, to, insert }` is in UTF-16 units. `NothingToFormat` when no statement is picked, `Unsafe` when the output would lex differently. Pure and sync, and it never touches the runtime. |
| `console_run` | `TabId, RunRequest, confirmed: Option<WriteDigest>` | `Result<RunAccepted, RunError>` | Returns once statements are picked. Progress arrives as events. `BadFilter` when a table page's SQL isn't one statement. On Production, a run with a statement that writes returns `ConfirmWrites` and runs nothing unless `confirmed` is the digest the engine computes for it now. While the tab holds writes that no Production question covered and whose transaction the engine has not seen end, every run asks, whatever its text, because any statement or batch can commit them. A tab whose session opened on Production keeps asking after a relabel until the session closes. `RunRequest::Script` carries `RunMode`. Explain never enters `run_task` and never emits `BEGIN (implicit)`. On PostgreSQL, idle Explain is `BEGIN READ ONLY` then `ROLLBACK`. Idle Analyze is `BEGIN` then `ROLLBACK`. Inside an open transaction both use savepoint `l1_explain`, and plain Explain also sets `transaction_read_only`. An aborted transaction is refused. Plan JSON is one cell. On SQL Server, Explain sends `SET SHOWPLAN_XML ON`, the batch, and `SET SHOWPLAN_XML OFF` as three batches. Explain Analyze wraps the batch in `SET STATISTICS XML ON` and `OFF` and runs it, so a batch that may write, begin a transaction, or read another server asks first on every environment. A failed OFF retires the session. Plain Explain does not ask and does not clear held writes. The showplan XML is one cell. |
| `console_cancel` | `TabId` | `()` | Idempotent. Sets the stop flag and sends `CancelRequest`. |
| `console_commit` | `TabId, confirmed: Option<WriteDigest>` | `Result<(), TxnError>` | `Busy`, `NoTransaction`. Commit in `Aborted` reports `RolledBack`. On Production, a commit that would keep writes no Production question covered returns `ConfirmWrites` that lists them and sends nothing unless `confirmed` matches. |
| `console_rollback` | `TabId` | `Result<(), TxnError>` | |
| `table_apply` | `TabId, TableChanges, confirmed: Option<WriteDigest>` | `Result<ApplySummary, ApplyError>` | Applies staged row edits on the tab's session, all or nothing. On a Production connection it applies only with a matching `confirmed`, as `console_run` does. |
| `console_close` | `TabId, force: bool` | `Result<(), DisconnectError>` | Drops session and results. |
| `result_page` | `ResultRef, start: u32, len: u32` | `Result<RowPage, ResultError>` | `Gone` if the tab re-ran or closed. `len` ≤ 2,000. |
| `result_cell` | `ResultRef, row: u32, col: u16` | `Result<Option<String>, ResultError>` | Full value for cut cells. |
| `result_series` | `ResultRef, x: Option<u16>, y: u16, maxPoints: u32` | `Result<ChartSeries, ResultError>` | Reads the first `min(maxPoints, 2,000)` stored rows for a chart. `labels` holds the x column's text, cut like a page cell, or the 1-based row number when `x` is `None`. `values` holds the y column as `f64`, `None` for NULL or text that is not a finite number. `capped` says more rows are stored. `OutOfRange` for a column outside the result. |
| `result_plan` | `ResultRef` | `Result<Plan, ResultError>` | Parses JSON or showplan XML from the format stored with the cell. `NotAPlan` when the format or the cell is missing, the cell is truncated, or the document is not a plan. |
| `result_export` | `ResultRef, format: ExportFormat, scope: ExportScope, path: String` | `Result<ExportSummary, ExportError>` | Path comes from `@tauri-apps/plugin-dialog` `save()`. `Loaded` writes the arena; `All` re-runs the result's source on the tab's session (decision 4) and resolves when the file is in place. Every error leaves `path` as it was. |
| `file_open` | `path: String` | `Result<FileOpened, FileReadError>` | Reads a `.sql` file for a new tab or for boot's reconcile. Returns LF text with no BOM, the file's name, and a `FileBinding` with the canonical path, a content stamp, and the BOM and CRLF format. UTF-8 only, at most 4 MiB. Async, on the blocking pool. |
| `file_save` | `target: SaveTarget, text: String` | `Result<FileSaved, FileWriteError>` | `Bound` writes the tab's file and answers `Changed { disk }`, writing nothing, when the disk no longer matches the binding's stamp or the `overwrite` stamp. `New` writes a path the save dialog returned, and answers `OpenInAnotherTab`, writing nothing, when that path is the same file as one of `taken`, the paths the other tabs are bound to. Atomic replace, permission bits kept, symlinks written through, read-only files refused. Async, on the blocking pool. |
| `history_list` | `HistoryQuery` | `HistoryPage` | Keyset pagination on id. |
| `history_browse` | `HistoryBrowseQuery` | `Result<HistoryBrowsePage, HistoryError>` | Paged exact SQL groups, matching group counts, and a store revision. |
| `history_group` | `HistoryGroupQuery` | `Result<HistoryGroupPage, HistoryError>` | Full SQL and paged runs for one group at the requested revision. |
| `history_clear` | – | `()` | |
| `saved_query_save` | `text: String, target: RunTarget, databaseKind: DatabaseKind` | `Result<SaveQueryReply, SavedQueryError>` | Saves the selection, or what a `Cursor` run would pick, or the whole text for `All`. Text already saved in the same dialect returns that entry with `alreadySaved` and writes nothing. `NothingToSave` for blank text, `TooLong` past 64 KiB. Sync. |
| `saved_query_update` | `id: SavedQueryId, edit: SavedQueryEdit` | `Result<SavedQueries, SavedQueryError>` | Renames and rewrites the SQL in one call. A blank name falls back to the SQL's first comment or first 72 characters. `NotFound` for a deleted id. Sync. |
| `saved_query_delete` | `id: SavedQueryId` | `Result<SavedQueries, SavedQueryError>` | Idempotent. Sync. |
| `saved_query_used` | `id: SavedQueryId` | `Result<SavedQueries, SavedQueryError>` | Moves the entry to the top. A missing id is not an error. Sync. |
| `workspace_save` | `Workspace` | `()` | Debounced 500 ms by the UI. Last write wins (single writer). |
| `settings_set` | `Settings` | `()` | Re-probes each enabled provider whose `enabled` flag, binary path, config directory, or launch arguments changed. |
| `assistant_open` | `chatId: ChatId` | `Result<ChatView, ChatError>` | Every entry, with the running turn's as of now. An unknown id opens a draft with default options, held in memory until its first send. Closes what a crash left open, with a `Lost` end and `Expired` for waiting tools. Idempotent, and mints no rev: stored rows come at the chat's own rev, so two opens of an unchanged chat return the same view. |
| `assistant_configure` | `chatId, options: ChatOptions` | `Result<ChatState, ChatError>` | Sync. Checks the model and effort against the last `Ready` probe, and keeps them as they are when there is none. Applies at the next turn. Works on drafts. |
| `assistant_send` | `chatId, request: SendRequest` | `Result<TurnAccepted, SendError>` | Stores the chat row on the first send and the `User` entry, starts the turn, and returns. `Busy` while a turn runs or the chat is being deleted. A missing binary or a signed-out CLI ends the turn with a typed `End`, never a send error. |
| `assistant_stop` | `chatId` | `()` | Sync, idempotent. Denies waiting approvals, cancels running tool calls, sends `interrupt`, waits up to 5 s, then kills the process group. A later ask or call from the process is denied at once. |
| `assistant_approve` | `chatId, approval: ApprovalId, decision: Decision` | `Result<(), ApproveError>` | Sync. `NotPending` once the approval was answered, denied by Stop, or expired, and for another chat's approval. |
| `assistant_delete` | `chatId` | `()` | Takes `AppHandle`, like `console_cancel`. Marks the chat deleted under its lock, so the turn task stores and publishes nothing more, stops a running turn and waits up to 5 s, aborts it past that, deletes the rows, and emits one `ChatStatus::Deleted` tombstone with a fresh rev, also for a stored chat not loaded this session. Leaves the CLI's transcript file alone. |
| `provider_refresh` | – | `()` | Sync, fire and forget. A full probe of every enabled provider. |
| `usage_limits` | – | `LimitsReport` | Takes `AppHandle`, like `console_cancel`. Asks the Claude CLI, bounded at 8 s. Never fails: a failed read is its own `LimitsOutcome`. Reads Claude even while its provider is switched off. |
| `update_check` | – | `()` | Sync, fire and forget. Starts a check unless one runs, a release is held, or one downloads or installs. |
| `update_download` | `version` | `Result<(), UpdateError>` | Sync. Starts the download of the offered `version`. Ok and nothing more when that version already downloads, is downloaded, or installs. `Stale` otherwise. |
| `update_install` | `version, accepted: Option<RestartImpact>` | `Result<InstallReply, UpdateError>` | Takes `AppHandle`, like `console_cancel`. Answers `Confirm { impact }` until `accepted` covers what a restart would end now. Then it stops the assistant's turns, installs on a blocking thread, asks the shell to relaunch, and answers `Restarting`. `Stale` unless `version` is downloaded or installing. |
| `update_set_track` | `track: UpdateTrack` | `Result<(), UpdateError>` | Sync. Saves the track and checks it at once. The same track again does nothing. `Busy` while a download or an install runs. |

```rust
#[derive(Deserialize, Type)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum RunRequest {
    Script { text: String, target: RunTarget },
    /// Engine builds quoted SQL: the query's ORDER BY text first, then the catalog's PK columns as a tiebreak.
    TablePage { schema: String, table: String, page: u32, #[serde(default)] query: TableQuery },
}
#[derive(Clone, Default, PartialEq, Serialize, Type)]
pub struct TableQuery {
    pub order_by: String,              // SQL after ORDER BY; empty means primary key order
    pub filters: Vec<ColumnFilter>,
    pub r#where: String,               // a boolean SQL expression ANDed with `filters`; empty means none
}
// TableQuery has a hand-written Deserialize so saved tabs missing a field still load, while specta keeps every field required.
#[derive(Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct ColumnFilter { pub column: String, pub op: FilterOp, pub value: String /* a PG literal; ignored by IsNull, NotNull */ }
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
pub enum FilterOp { Eq, Ne, Lt, Le, Gt, Ge, Contains, StartsWith, IsNull, NotNull }
#[derive(Deserialize, Type)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum RunTarget { Cursor { offset: DocOffset }, Selection { from: DocOffset, to: DocOffset }, All, }
#[derive(Serialize, Type)]
pub struct RunAccepted { pub run_id: RunId, pub statements: Vec<DocRange> }
/// The statements of a script and the index of the one a `Cursor` run at the given offset would pick.
#[derive(Serialize, Type)]
pub struct ScriptOutline { pub statements: Vec<DocRange>, pub at_cursor: Option<u16> }
#[derive(Serialize, Type)]
#[serde(tag = "kind")]
pub enum RunError {
    Busy, NoStatement, NotConfigured, Unsupported { what: String /* COPY FROM STDIN / TO STDOUT */ }, Connect { error: ConnectError },
    BadFilter,                         // WHERE or ORDER BY isn't a single expression
    ConfirmWrites { confirmation: WriteConfirmation }, // a write on Production; nothing ran
}
/// Opaque. Names the exact statements of a guarded action, the connection as saved and as the tab's session opened it, and the tab's database, schema, mode, transaction state, and held writes.
#[derive(Serialize, Deserialize, Type)] pub struct WriteDigest(String);  // private field: only writes::check makes one
#[derive(Serialize, Type)]
pub struct WriteConfirmation { pub connection: String, pub database: String, pub unit: ExecutionUnitKind, pub sends: WriteList, pub holds: WriteList, pub digest: WriteDigest }
#[derive(Default, Serialize, Type)] pub struct WriteList { pub count: u32, pub statements: Vec<String> /* first three, cut at 2,000 characters */ }
#[derive(Serialize, Type)] #[serde(tag = "kind")] pub enum TxnError { Busy, NoTransaction, RolledBack, Query(QueryError), ConfirmWrites { confirmation: WriteConfirmation } }
#[derive(Serialize, Type)] #[serde(tag = "kind")] pub enum ResultError { Gone, OutOfRange }
#[derive(Serialize, Type)] pub struct ExportSummary { pub rows: u64, pub bytes: u64, pub truncated: bool }
#[derive(Serialize, Type)] #[serde(tag = "kind")] pub enum CountError { NotConnected, Failed { message: String } }

/// Staged edits to one table. Rows to update or delete are found by primary key.
#[derive(Deserialize, Type)]
pub struct TableChanges {
    pub schema: String, pub table: String,
    pub inserts: Vec<Vec<CellWrite>>,  // columns left out take their default
    pub updates: Vec<RowUpdate>,
    pub deletes: Vec<Vec<CellWrite>>,  // each entry is the primary key of a row to delete
}
#[derive(Clone, PartialEq, Deserialize, Type)]
pub struct CellWrite { pub column: String, pub value: Option<String> /* PG text input; None is NULL */ }
#[derive(Deserialize, Type)] pub struct RowUpdate { pub key: Vec<CellWrite>, pub set: Vec<CellWrite> }
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Type)] pub enum ChangeKind { Insert, Update, Delete }
/// Points at one entry of `TableChanges`, by its list and index there.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Type)] pub struct ChangeRef { pub kind: ChangeKind, pub index: u32 }
/// `committed` is false when the changes wait in the tab's open transaction (manual mode) for Commit.
#[derive(Serialize, Type)] pub struct ApplySummary { pub committed: bool }
/// Nothing is applied when any of these comes back.
#[derive(Serialize, Type)]
#[serde(tag = "kind")]
pub enum ApplyError {
    Busy, NotConfigured, Connect { error: ConnectError },
    TransactionAborted,                              // roll the tab's failed transaction back first
    NoPrimaryKey,                                    // updates and deletes need the full primary key
    Failed { change: ChangeRef, error: QueryError },
    RowMismatch { change: ChangeRef, rows: u64 },    // matched `rows` rows instead of one
    Transaction { error: QueryError },               // opening or committing the transaction failed
    ConfirmWrites { confirmation: WriteConfirmation }, // edits on Production; nothing ran
}
```

Table data tabs need no extra command to read. A table tab sends `RunRequest::TablePage` with its `TableQuery` (`where`, per-column `filters`, `orderBy`) to `console_run` on its own console. The engine builds `SELECT * FROM "public"."orders" WHERE … ORDER BY <user order>, <pk cols> LIMIT 500 OFFSET 1000` with a pure, tested `browse_sql` function and then follows the same run path as a script. One execution path serves every tab, and identifier quoting has one owner.

Writing uses one command, `table_apply(tab, TableChanges)`. It borrows the tab's session the way Commit and Rollback do, and runs deletes, then updates, then inserts. With no transaction open, auto-commit mode wraps them in `BEGIN … COMMIT`, and manual mode leaves them in a new open transaction. Inside an open transaction they run under a savepoint, so a failure leaves that transaction as it was. Rows to update or delete are found by their full primary key, and each statement must affect exactly one row. Otherwise everything rolls back, and the error names the change that failed.

### Engine (crates/engine/src/lib.rs, the deep module)

```rust
pub struct Engine { shared: Arc<Shared> }
pub(crate) struct Shared { /* data_dir, vault, store, settings, connections, consoles, events, next_run, timezone */ }

pub struct EngineConfig { pub data_dir: PathBuf, pub keys: Arc<dyn KeyStore>, pub agent_path: AgentPath, pub az_binary: Option<PathBuf>, pub user_known_hosts: Option<PathBuf>, pub updates: Arc<dyn UpdateTransport> }
// AgentPath::LoginShell (the app and the dev-bridge) or AgentPath::Fixed(path) (tests pass an empty one,
// so no installed agent CLI ever runs under `cargo test`)
// az_binary: the dev-bridge passes $L1_DATA_AZ_BINARY; the app and most tests pass None
// updates: the app passes src-tauri's TauriTransport; the dev-bridge passes ScriptedTransport::named($L1_DATA_UPDATE_SCENARIO),
// and unrelated tests pass the `off` scenario

impl Engine {
    pub async fn open(cfg: EngineConfig) -> Result<Arc<Self>, StoreError>;   // opens SQLite, runs migrations
    pub fn data_dir(&self) -> &Path;                                         // self-check report, bridge log
    pub fn subscribe(&self) -> broadcast::Receiver<AppEvent>;                // Tauri shell + tests
    pub fn boot(&self) -> BootSnapshot;

    pub async fn save_connection(&self, d: ConnectionDraft) -> Result<ConnectionConfig, SaveError>;
    pub async fn delete_connection(&self, id: ConnectionId) -> Result<(), DisconnectError>;
    pub async fn test_connection(&self, d: ConnectionDraft) -> Result<ServerInfo, ConnectError>;
    pub async fn connect(&self, id: ConnectionId) -> Result<ServerInfo, ConnectError>;
    pub async fn disconnect(&self, id: ConnectionId, force: bool) -> Result<(), DisconnectError>;
    pub async fn catalog(&self, db: DatabaseRef) -> Result<Arc<Catalog>, CatalogError>;
    pub async fn refresh_catalog(&self, db: DatabaseRef) -> Result<u64, CatalogError>;
    pub async fn relation_detail(&self, db: DatabaseRef, oid: u32) -> Result<RelationDetail, DetailError>;
    pub async fn object_script(&self, db: DatabaseRef, target: ScriptTarget) -> Result<ObjectScript, DetailError>;
    pub async fn exact_row_count(&self, tab_id: TabId, db: DatabaseRef, schema: String, table: String, query: TableQuery) -> Result<u64, CountError>;

    pub async fn console_configure(&self, tab: TabId, c: ConsoleConfig) -> Result<ConsoleState, ConfigureError>;
    pub fn script_outline(&self, text: &str, cursor: DocOffset) -> ScriptOutline;     // pure; the run mark and the chooser
    pub fn completion_context(&self, text: &str, cursor: DocOffset, kind: DatabaseKind) -> CompletionContext; // pure; the editor's completion
    pub fn editor_format(&self, text: &str, target: RunTarget) -> Result<FormatEdit, FormatError>; // pure; ⌘⌥L
    pub async fn console_run(&self, tab: TabId, r: RunRequest, confirmed: Option<WriteDigest>) -> Result<RunAccepted, RunError>;
    pub async fn console_cancel(&self, tab: TabId);
    pub async fn console_commit(&self, tab: TabId, confirmed: Option<WriteDigest>) -> Result<(), TxnError>;
    pub async fn console_rollback(&self, tab: TabId) -> Result<(), TxnError>;
    pub async fn table_apply(&self, tab: TabId, changes: TableChanges, confirmed: Option<WriteDigest>) -> Result<ApplySummary, ApplyError>;
    pub async fn console_close(&self, tab: TabId, force: bool) -> Result<(), DisconnectError>;

    pub fn result_page(&self, r: ResultRef, start: u32, len: u32) -> Result<RowPage, ResultError>;
    pub fn result_cell(&self, r: ResultRef, row: u32, col: u16) -> Result<Option<String>, ResultError>;
    pub async fn result_export(&self, r: ResultRef, format: ExportFormat, scope: ExportScope, path: String) -> Result<ExportSummary, ExportError>;

    pub fn history(&self, q: HistoryQuery) -> HistoryPage;
    pub fn clear_history(&self);
    pub fn save_workspace(&self, w: Workspace);
    pub fn settings(&self) -> Settings;                                      // main.rs: the window's first theme
    pub fn set_settings(&self, s: Settings);

    pub async fn assistant_open(&self, chat: ChatId) -> Result<ChatView, ChatError>;
    pub fn assistant_configure(&self, chat: ChatId, options: ChatOptions) -> Result<ChatState, ChatError>;
    pub async fn assistant_send(&self, chat: ChatId, request: SendRequest) -> Result<TurnAccepted, SendError>;
    pub fn assistant_stop(&self, chat: ChatId);
    pub fn assistant_approve(&self, chat: ChatId, approval: ApprovalId, decision: Decision) -> Result<(), ApproveError>;
    pub async fn assistant_delete(&self, chat: ChatId);
    pub fn provider_refresh(&self);
    pub async fn usage_limits(&self) -> LimitsReport;
    pub fn update_check(&self);
    pub fn update_download(&self, version: String) -> Result<(), UpdateError>;
    pub async fn update_install(&self, version: String, accepted: Option<RestartImpact>) -> Result<InstallReply, UpdateError>;
    pub fn update_set_track(&self, track: UpdateTrack) -> Result<(), UpdateError>;
    pub async fn shutdown(&self);                                            // quit: stops every turn, then kills any agent process still alive, within engine::SHUTDOWN_BOUND (4 s); the shell's timeout is that plus a margin
}
```

The block above is an excerpt. `Engine` has 53 public methods, and 47 of them match the 47 registered commands one to one. `open`, `data_dir`, `runtime`, `subscribe`, `settings`, and `shutdown` serve the shell, the dev-bridge, and tests. Behind it the engine hides:

- TLS mode mapping and keychain reads
- Session lifecycle, lazy reconnects, and `search_path` / `TimeZone` setup
- Statement splitting and picking
- Implicit `BEGIN`
- Transaction-state tracking
- Prepare/describe and text streaming
- The row cap with drain
- Throttled snapshot emission
- History recording
- DDL-triggered catalog refresh, per database and coalesced
- UTF-16 offset mapping
- Table-browse, count, and edit SQL with its own quoting
- Staged edits applied under one transaction or savepoint
- Agent CLI processes, their protocol, the approval ledger, and the assistant's read-only query sessions
- The update schedule, the offer policy, the saved track, and what a restart would end

The frontend sequences two calls for one operation in two places only. `runTab` configures a tab's console before its first run, and the pager's Last button counts rows with `exact_row_count` before it loads the page.

### Internal structure

```rust
// console.rs
pub struct Console {
    pub tab: TabId,
    pub inner: Mutex<ConsoleInner>,                       // tokio Mutex, held only for state edits, never across a query
    pub results: std::sync::Mutex<Vec<Arc<ResultSet>>>,   // results of the current/last run only; dropped on re-run
    pub latest: std::sync::Mutex<ConsoleState>,           // the last published snapshot, for boot()
    pub closed: AtomicBool,
}
pub struct ConsoleInner {
    pub config: ConsoleConfigView,
    pub session: SessionSlot,
    pub txn: TxnState,
    pub run: RunState,
    pub rev: u64,
    pub owed: Owed,                                       // refreshes (catalog, database list) owed by statements whose transaction has not committed
    pub unasked: WriteList,                               // writes no Production question covered, until the engine sees their transaction end
}
pub enum SessionSlot {
    Empty,
    Idle(IdleSession),                                    // private fields: the Session and the ConnectionConfig it opened from
    Busy { opened: ConnectionConfig, run_id: RunId, work: Work, backend_pid: i32, cancel: CancelHandle, stop: Arc<AtomicBool> },  // a run task or a commit owns the Session
    Lost { reason: String, transaction: TransactionLoss },
}
pub enum Work { Run(WritesChecked), Count, Commit(WritesChecked), Rollback, Edit(WritesChecked), Probe, Export(ExportProgress) }  // only writes::check makes a WritesChecked

// session.rs
pub struct Session {
    pub client: tokio_postgres::Client,
    pub backend_pid: i32,
    pub server: ServerInfo,
    pub search_path: Option<String>,              // the schema last set, compared with the console config before each run
    cancel: CancelToken,
    tls: MakeTlsConnector,
    notices: mpsc::UnboundedReceiver<DbError>,   // fed by the Connection driver task (poll_message)
    closed: watch::Receiver<Option<String>>,     // Some(reason) once the driver task exits
}
#[derive(Clone)] pub struct CancelHandle { token: CancelToken, tls: MakeTlsConnector }
impl Session {
    /// Connects, sets TimeZone, application_name='L1 Data', search_path; spawns the driver task.
    pub async fn open(p: &ConnectParams, schema: Option<&str>, timezone: &str) -> Result<Session, ConnectError>;
}

// results.rs: single writer (run task), many readers (result_page); append-only.
pub struct ResultSet { pub r#ref: ResultRef, pub columns: Vec<ColumnMeta>, data: RwLock<Arena> }
struct Arena { text: String, cells: Vec<(u32, u32)> /* (start, len); len == u32::MAX = NULL */, ncols: usize, rows_seen: u64, truncated: Option<TruncateReason>, elapsed_ms: u32 }

// sql/split.rs, sql/classify.rs: pure
pub struct StmtSpan { pub start: usize, pub end: usize }        // UTF-8 byte offsets, trimmed
pub fn split_statements(sql: &str) -> Vec<StmtSpan>;
pub fn pick(spans: &[StmtSpan], sql: &str, target: ByteTarget) -> Vec<StmtSpan>;
pub enum StmtKind {
    Begin, Commit, Rollback, Savepoint, RollbackTo, Release,
    Ddl(String), Dml(DmlVerb), Query, CopyStdio, NoTxnBlock(String), Other(String),   // String: the command tag
}
pub fn classify(stmt: &str) -> StmtKind;

// catalog.rs
impl Catalog { pub fn relation(&self, schema: &str, table: &str) -> Option<&Relation>; }
impl Relation { pub fn pk_columns(&self) -> Vec<String>; }

// txn.rs: pure, exhaustively tested
pub enum StmtOutcome { Ok, Err }
pub fn txn_after(state: TxnState, kind: &StmtKind, outcome: StmtOutcome) -> TxnState;
pub fn needs_implicit_begin(mode: TxnMode, state: TxnState, kind: &StmtKind) -> bool;

// text.rs: pure boundary conversions
pub struct OffsetMap<'a> { /* sql text */ }
impl OffsetMap<'_> {
    pub fn to_utf16(&self, byte: usize) -> DocOffset;
    pub fn from_utf16(&self, off: DocOffset) -> usize;
    pub fn range(&self, span: &StmtSpan) -> DocRange;
    /// PG ErrorPosition::Original(n) is a 1-based character index within the statement.
    pub fn pg_position(&self, stmt: &StmtSpan, n: u32) -> DocOffset;
}
pub fn type_ref(t: &tokio_postgres::types::Type) -> PgTypeRef;       // int8 -> bigint, _int4 -> integer[], Kind -> category
pub fn synthesize_tag(kind: &StmtKind, rows: u64) -> String;          // "INSERT 0 3", "UPDATE 3", "CREATE TABLE"
pub fn quote_ident(name: &str) -> String;
pub fn quote_literal(value: &str) -> String;
pub fn browse_sql(schema: &str, table: &str, pk: &[String], q: &TableQuery, limit: u32, offset: u64) -> String;
pub fn count_sql(schema: &str, table: &str, q: &TableQuery) -> String;
pub fn insert_sql(schema: &str, table: &str, cells: &[CellWrite]) -> String;
pub fn update_sql(schema: &str, table: &str, key: &[CellWrite], set: &[CellWrite]) -> String;
pub fn delete_sql(schema: &str, table: &str, key: &[CellWrite]) -> String;
```

### Run loop (console.rs, pseudocode)

```rust
async fn run_task(console: Arc<Console>, shared: Arc<Shared>, mut session: Session, plan: RunPlan, stop: Arc<AtomicBool>) -> Result<(), Orphaned> {
    // if session.search_path != plan.schema => session.set_search_path(plan.schema)
    for (i, stmt) in plan.statements.iter().enumerate() {
        if stop.load() { mark rest Cancelled/Skipped; break }
        let kind = stmt.kind;                            // classified by console_run
        output(Statement { i, text })                    // TablePage logs the SQL the engine built
        if needs_implicit_begin(mode, txn, kind) { session.client.batch_execute("BEGIN"); txn = Active; Info("BEGIN (implicit)") }
        set_status(i, Preparing);
        let outcome = match session.client.prepare(stmt.sql).await {
            Err(e) => Failed(map_err(e, stmt)),
            Ok(p) if p.columns().is_empty() => {
                // simple_query: collect CommandComplete(n) -> synthesize_tag(kind, n)
            }
            Ok(p) => {
                // result = ResultSet::new(ref, p.columns().map(column_meta))
                // stream = simple_query_raw(stmt.sql); for msg in stream:
                //   Row(r)              => if arena under caps { append text cells } else { rows_seen += 1 }  // drain, never cancel
                //   CommandComplete(n)  => done
                //   every 100ms         => emit ConsoleChanged (row_count grows)
            }
        };
        output(drained notices ++ [Rows | Command | Error])  // one ConsoleOutput: notices, then the result line
        txn = txn_after(txn, &kind, outcome.ok_or_err());
        shared.record_history(...);                      // single writer: run tasks, via store mutex
        if outcome.failed() && settings.stop_on_error { mark rest Skipped; break }
        if ok { owed.add(kind.invalidates()) }; if txn == Idle { shared.schedule_refreshes(console db, owed.take()) }  // inside a txn: owed until commit
    }
    // if session.closed has Some(reason): slot = Lost { reason, transaction: session.transaction_loss(txn) }; txn = Idle
    // else slot = Idle(session)
    // run = Finished { .. }; emit ConsoleChanged
}
```

Session-loss rules:

- **Death detection.** Driver-task exit, or TCP keepalive (`keepalives_idle = 30s`), sets `closed`.
- **Lost while idle with no transaction.** The next run opens a fresh session transparently and logs `Info("Session reset")`.
- **Lost with a transaction open.** The slot records `TransactionLoss::RolledBack` for a PostgreSQL transaction and the UI shows "Transaction rolled back: connection lost". SQL Server retirement records `TransactionLoss::Unknown`: the UI says "session reset", preserves the fact that earlier commits remain committed, and warns that the last transaction outcome may be unknown. Idle PostgreSQL loss records `None`. The next run starts a fresh session and clears the notice.
- **Metadata session death.** The home database's session marks `ConnectionState` `Lost` and closes the instance, which drops every slot and its session. Another database's session marks only that database's `CatalogStatus` `Failed`, keeping the last catalog's version; the next load on it reopens the session. Consoles are independent. A disconnect closes the instance the same way, and a slot's watch task holds the instance weakly so it cannot keep a session alive past that.

`disconnect(force=false)` checks every console of that connection that is `Active` or `Aborted`. To compute `has_writes` it runs `SELECT pg_current_xact_id_if_assigned() IS NOT NULL` (PG13+, else `true`) on the idle session. The probes for all consoles run together, each for at most 3 s, and a probe with no answer counts as `has_writes`, so a stalled network cannot hang a disconnect, a tab close, or a restart confirm. The session goes back to the tab unchanged, because a slow answer is not a lost transaction.

### Frontend (React 19, TypeScript, Vite)

- **Framework: React 19.** Query history uses a Radix dialog with a selectable list and SQL preview. Radix covers dialogs, dropdown menus, and context menus. TanStack Virtual covers the grid, and `lucide-react` supplies the icons. react-resizable-panels holds only the editor and results split. The right panel holds the assistant chat from `features/assistant/`. Agents write React most reliably, which speeds the verify loop.
- **Editor: CodeMirror 6, used directly** (no wrapper library). One shared `EditorView` is mounted for the tab editor, and the `EditorState` is swapped per tab, so undo history survives tab switches. Query history mounts a separate read-only view with the same SQL highlighting and theme. A table tab's editor starts with `browseQuery(schema, table)`. The dialect's lang-sql language provides highlighting only. Completion is one async source, `engineCompletion` in `features/editor/completion.ts`, and the frontend parses no SQL for it. The source calls `completion_context` with the document and the cursor when a word is typed, after a `.`, after whitespace, or on an explicit request, and opens a list after whitespace only when the engine answers `JoinCondition`. Right after a relation that can take an alias there, the engine answers `Alias` with the keywords that can follow. That is a FROM, JOIN, or USING item, including the FROM items after a T-SQL `UPDATE` or `DELETE` target, a PostgreSQL `UPDATE` or `DELETE` target, a `MERGE` target with or without `INTO`, or an `INSERT` target after `AS`. After the bare target of a T-SQL `UPDATE` or `DELETE`, which may follow `TOP (n) [PERCENT]`, and after any other `INSERT` target, it answers `Keyword`, so the keywords keep their order. The list opens unasked once the word is two or more letters of one of them, such as `wh` of `WHERE`, and the word as typed is the first entry, so `Enter` keeps it and only an arrow key picks a keyword. A reserved word right after a relation, such as `FOR` or `OPTION`, is never read as its alias. The engine's `sql/scope.rs` takes the statement `split_statements` puts the cursor in, or for T-SQL the statement around the cursor inside its `GO` batch, which starts at a `;` or at a statement verb that cannot continue the statement before it, or at a `WITH` after `BEGIN`, `ELSE`, or a view's or procedure's `AS`. T-SQL finds that statement with one pass over the bytes (`sql/tsql/around.rs`), with no tokens and no upper-case copies, so only the statement is lexed. It lexes that statement with the dialect's own lexer (T-SQL's leniently, so an unclosed quote still lexes) and walks one frame per parenthesis. It names the clause at the cursor, the FROM items the server lets the cursor see with their aliases, and each CTE's or subquery's output columns as the server names them. Only a bare or qualified column or an alias names an output column, and a bare alias counts only after a column, a call, a parenthesized expression, a literal, or a cast. Any other expression stays unnamed, since the server's name for it, such as `btrim` for `trim(...)` or `rank` for `rank() over w`, is not in the text. A column alias list over a `*` of a `USING` or `NATURAL` join offers no columns, since those joins merge columns in an order the reply cannot say. A T-SQL `PIVOT` or `UNPIVOT` is one relation with no known columns, which hides the whole table source before it. A `RETURNING` after an `INSERT`'s query sees only the target. A T-SQL `OUTPUT` offers only `inserted` and `deleted`, as the statement allows, with no bare column, and `inserted.` or `deleted.` lists the target's columns. A CTE body and a FROM subquery do not see the FROM items of the query they stand in, but do see those of any query that query is correlated with. A `LATERAL` subquery also sees the FROM items to its left, and a T-SQL `APPLY` those to its left in its own join tree. A correlated subquery sees the enclosing FROM items. A join's `ON` sees the relations of its own join tree up to its own, so not an earlier comma-separated item, an `UPDATE` target, or a `DELETE` target. A parenthesized join adds its relations to the query. Inside a CTE's body its own name resolves to the CTE after `WITH RECURSIVE`, and always in T-SQL. A column alias list, as in `orders o(a, b)`, renames the first columns. PostgreSQL folds only ASCII letters of an unquoted name, and the frontend matches a name in another case only on SQL Server. An `INSERT`, `SELECT INTO`, or `CREATE TABLE AS` target is out of sight outside its column list, so `VALUES` offers none of its columns, and a T-SQL `UPDATE` target that a FROM item stands for is hidden. Where a column alias list meets a `*` whose column order the reply cannot carry, such as `(select *, 1 as x from orders) r(a)`, the relation offers no columns rather than wrong ones. A T-SQL `SET` option such as `SET NOCOUNT ON` ends its statement. A word that starts its line after a lexeme a statement can end on also gets the statement verbs, since a T-SQL statement needs no `;`. The frontend only looks names up in the catalog, defaulting a bare table to the catalog's `defaultSchema`. Join conditions read foreign keys through `useRelationDetail` for the joined table and each other table in scope, awaited inside the source for at most 250 ms, so the catalog refresh still never reads `pg_constraint` for the whole database and a slow server only drops the conditions from the list. `ensure` resolves once a fetch another caller started has arrived. `qualifiedTables` still offers every table as `schema.table` and inserts the engine's `qualifiedName`. Every other name the list inserts comes quoted from the engine: a catalog schema, relation, column, or function's `quoted`, a function's `insertText`, and a derived column or column alias as the reply writes it. The frontend quotes nothing. The list opens with no typing delay, because CodeMirror's default 100 ms delay alone would spend twice the 50 ms budget from keystroke to list. Past one million characters, an unasked request waits instead for 150 ms without typing, because each request copies the whole document across IPC, which on a 4 MB file costs about 50 ms of the main thread. CodeMirror runs one request per source at a time, and the source keeps one `completion_context` in flight per editor, so a request CodeMirror drops while it is out waits behind it, only the newest waiting one is sent, and one CodeMirror has cancelled is never sent. Each request copies the document in a task of its own, and the run mark and completion share one copy of the text per document version. When the source declines to open, it returns an empty result instead of `null`, because CodeMirror never asks again after a `null` for a word typed while the request was out. That result, like an alias list, answers more typing in the same word from the same reply, since the engine skips the word being typed. After a failed script run, `showDiagnostics` calls `setDiagnostics` to mark `QueryError.position`.
- **Grid: a hand-built DOM grid on `@tanstack/react-virtual`.** Every row has the same height, `round((code font size + 11) × density factor)`, which is 24 px at the defaults. Column widths are measured in the chosen code and interface fonts. Rows and columns are always virtualized. The header is one line with a category icon (a key icon for a primary-key column) and the name. The type shows in the tooltip. On a table tab a header click sorts and Shift-click adds a sort key; `⌥S` and `⇧⌥S` do the same for the active cell's column, and `⌥→`/`⌥←` resize it. Rows and cells are memoised components in `GridRows.tsx`; pointer events are delegated to the grid, so a drag-select re-renders only the rows whose selection changed. A sticky row-number column is on the left. Numeric categories are right aligned with digit grouping, inserted textually so precision is never lost. NULL renders as a dim italic `NULL` in a solid outline and the empty string as `empty` in a dashed outline, both in the interface font, so neither reads as the text `'NULL'` or as a blank cell. The grid is an ARIA `grid` whose `aria-activedescendant` names the focused cell. Cut cells show an ellipsis button that opens the `cell` overlay with the full value. Selection of a cell range is copied as TSV, up to 50,000 rows. On an editable table tab the grid also edits cells, adds rows, and marks rows deleted (see Frontend phase in Implementation reconciliation).
- **Styling: Tailwind v4** with CSS variables for every colour. `index.css` holds the dark values from the mockup and a `:root[data-theme="light"]` block that overrides each one. The CodeMirror theme and syntax colours read the same variables. `shell/theme.ts` sets `data-theme` and `color-scheme` on the root element before the first render. It follows `prefers-color-scheme` while the setting is System. `shell/appearance.ts` paints the font, density, and motion settings the same way, as root custom properties. Tailwind's `--spacing` is `0.25rem × --density`, so every spacing utility follows the density. Interface text uses `text-ui-N` tokens, defined as `N px × --ui-scale`, instead of `text-[Npx]`. The SQL editor and the result grid cells use `--code-font` and `--code-size`. The side columns, Radix dialogs, and menus animate over `--panel-duration`. It also sets `data-motion="reduced"` on the root while Reduce motion is On, or is System and `prefers-reduced-motion` asks for it. Unlayered rules in `index.css` then fade menus and dialogs without moving them, snap transform transitions, and stop `animate-pulse`. Spinners keep turning, because they show that work is still running. The side columns read the same state through `useReducedMotion` and open and close without a transition.
- **Settings is a centered dialog over the workspace.** The workspace store holds `screen: { kind: "workspace" } | { kind: "settings"; section; query }`, which is not persisted and scopes commands rather than choosing which workspace panels render. `AppLayout` keeps the sidebar, top bar, tabs, editor, results, and configured right panel mounted. `SettingsDialog` places a 960 × 640 Radix dialog above them, constrained to the viewport, with `SettingsSidebarBody` in its own 200-pixel navigation column. Navigation and section content scroll independently. Settings has no window controls, drag regions, or workspace sidebar toggle. `features/settings/settingsIndex.ts` lists the sections and their settings as plain data: each setting's id, label, search keywords, and `group`, the card it sits in. `sections.tsx` maps every setting id to its component, so each setting renders alone. A section's page renders its settings in order, puts a run that shares a group in one card titled by the group, and draws a setting with a null group bare. A setting's row reads its label from the index. The settings search box filters in place: a setting matches when its label, section, group, and keywords contain every word of `query`. While `query` is set, the page lists the matches with their live controls under a heading per section and group, the breadcrumb reads "Search results", and the nav lists only the sections with matches and their counts. Each change applies and persists at once. Radix handles Escape on the active dialog. Settings defers its capture-phase dismissal and handles keys bubbling from its own DOM, so a field can cancel a draft first. Keys from portaled child dialogs stay with the child. After those controls, Escape clears a nonempty search, then closes Settings on the next press. The Back and close buttons, `⌘W`, and `⌘,` also close Settings. Other shortcuts that open an overlay or a tab return to the workspace first. Entering or leaving Settings closes the current store overlay. Questions opened inside Settings render once within its React dialog subtree, preserving Radix's nested focus scopes. Closing Settings restores its connected opener unless another store overlay or a newly active tab is taking focus.
- **Dialogs share their frame and stacking.** `DialogSurface` forwards Radix content props and its ref, and shares the centered placement, viewport limits, border, background, shadow, dimming, and 6-pixel backdrop blur across Settings, History, Saved queries, connections, and compact `Modal` dialogs. Each portal's content sits inside its own overlay stacking context, so a nested dialog's backdrop covers its parent's content. Menus and popovers sit above dialog groups. The installing blocker is also a Radix dialog, above them all, so its focus scope pauses the Settings trap rather than competing with it through a second focus listener.

Stores (Zustand v5). Each has one owner, and derived data is never stored:

```ts
// state/live.ts: pure projection of backend events. No component writes here.
type LiveData = {
  connections: Record<ConnectionId, ConnectionState>;
  consoles: Record<TabId, ConsoleState>;
  output: Record<TabId, OutputEntry[]>;                // ring buffer, 5,000 per tab; the Output panel renders all of it
  historyTail: HistoryEntry[];                         // last 200 HistoryAppended entries, newest first
  providers: Partial<Record<ProviderKind, ProviderState>>;
  chats: Record<ChatId, ChatState>;
  transcripts: Record<ChatId, Record<number, EntrySnapshot>>;   // by seq, only for chats the panel opened; other chats' entry events are dropped
  updates: UpdateState | null;                                 // null only before the boot snapshot
  savedQueries: SavedQueries;                                  // rev 0 until the boot snapshot
};
// wantedCatalogs(connections): every Ready catalog of every Connected instance, keyed by DbKey (connection id + NUL + database).
// boot.ts prunes useCatalog to that set and calls ensure for each entry.
type LiveState = LiveData & {
  hydrate(b: BootSnapshot): void;
  hydrateChat(view: ChatView): void;                   // assistant_open's whole chat; a newer streamed entry wins
  hydrateSaved(queries: SavedQueries): void;           // a saved_query_* reply; the higher rev wins, as for the event
  apply(e: AppEvent): void;                            // keep-higher-rev; exhaustive switch on e.kind
};
// export const useLive = create<LiveState>()(...)

// state/workspace.ts: UI-owned; tabs, order, active, and layout persist via workspace_save (debounced)
type ConsoleTarget = { db: DatabaseRef | null; mode: TxnMode };   // a tab never names a connection without a database
type TableBrowse = { schema: string; table: string; page: number; query: TableQuery;
  exactRowCount?: number };                            // exactRowCount: set by the pager's Last button
type Tab = { id: TabId; title: string; console: ConsoleTarget; table: TableBrowse | null };
type TableTab = Tab & { table: TableBrowse; console: ConsoleTarget & { db: DatabaseRef } };   // narrowed by isTableTab
type WorkspaceData = {
  tabs: Record<TabId, Tab>; order: TabId[]; active: TabId | null; layout: Layout;
  configs: Record<ConnectionId, ConnectionConfig>;
  activeResult: Record<TabId, number>;                 // which statement's result is shown
  settings: Settings;
  selected: RelationRef | null;                        // highlighted relation in the tree: { db, schema, relation }
  overlay: Overlay;                                    // none | navigation | history | usage | saved | connection | cell | ask
  screen: Screen;                                      // workspace | settings, with the settings search query
  treeQuery: string;                                   // the sidebar search; not saved, kept across a trip to settings
  toast: { id: number; text: string } | null;
  cursor: { line: number; col: number };               // results header Ln/Col
};
// Editor text lives only in CodeMirror: editorStates: Map<TabId, EditorState> (module-level).
// workspaceSnapshot() reads every tab's text from there. No second copy of the text is kept.

// state/catalog.ts: per database, rebuilt when its catalog version changes
type CatalogView = {
  catalog: Catalog;
  qualifiedTables: Completion[];                       // "schema.table", applied as "schema"."table"
  functions: Completion[];                             // name (args), bare in the default schema; no procedures or trigger functions
  relationsById: Map<number, { schema: string; rel: Relation }>;   // OIDs are per database, so per view
};
// useCatalog: { views: Record<DbKey, CatalogView>; ensure(ref, version): Promise<void>; prune(keep: Set<DbKey>): void }

// state/relationDetail.ts: relation_detail results keyed by (DbKey, oid), each tagged with its catalog version.
// The tree calls ensure(db, catalogVersion, oid) when a relation opens and again when the version moves;
// the old detail stays on screen until the new one arrives. prune(views) drops entries whose relation
// no held catalog view lists, and boot calls it whenever the catalog views change.

// state/rowCache.ts: outside React, per ResultRef, LRU of 256-row pages (max 64 pages)
class RowCache {
  constructor(ref: ResultRef, source?: RowSource);    // source defaults to result_page / result_cell
  get(row: number): Row | undefined;
  isCut(row: number, col: number): boolean;
  ensure(range: RowRange, rowCount: number): Promise<void>;
  loadFull(row: number, col: number): Promise<string | null | undefined>;   // result_cell
  subscribe(listener: () => void): () => void;        // with getVersion, for useSyncExternalStore
}
function rowCacheFor(ref: ResultRef): RowCache;        // module-level map; a new runId for a tab evicts its old caches
function dropRowCaches(tabId: string): void;

// state/tableEdits.ts: staged row edits per table tab, not persisted
// useTableEdits: { byTab: Record<TabId, TableEdits>; submitting: Record<TabId, true>; edit; addRow; clear; beginSubmit; endSubmit }
type TableEdits = {
  inserts: NewRow[];
  updates: Record<RowKey, { key: CellValue[]; values: Record<string, CellValue> }>;  // RowKey = JSON of the PK values
  deletes: Record<RowKey, CellValue[]>;
  failedAt: { insert: number } | { key: RowKey } | null;
};

// features/results/tableQuery.ts: useFilterDrafts, filter rows the user is still filling in, per tab
// state/status.ts: pure derivations (isRunning, hasUncommittedWork, connectionTone, shownResult, runSummary)
// state/transcript.ts: pure. groupTurns(entries) groups a chat's entries into turns by their `turn` stamp.
```

The UI derives its status values instead of storing them:

| Derived status | Source |
|---|---|
| Run button: Run or Cancel | `isRunning(consoles[tab])`, from `run.kind` |
| Transaction mode in the TopBar | A menu on every tab: `config.mode`, `txn` in manual mode, and a rollback or session-reset notice from `session.transaction` while the session is `Lost` |
| Ln/Col in the results header | Every tab. `cursor` in the workspace store, set by the editor's update listener and again by `SqlEditor` on a tab switch, because `setState` runs no update listeners |
| Commit/Rollback visibility | Every tab, while `hasUncommittedWork` (`config.mode === "Manual" && txn !== "Idle"`) |
| "6 rows · 38 ms" in the results toolbar (`result-status`) | `runSummary(run, picked, now)` from `ResultMeta` |
| Transaction dot (`tab-dirty-<tab>`) | Shown only while `txn !== "Idle"` in manual mode ("uncommitted work"). A scratch tab's text autosaves to the workspace, so it has no unsaved state. |
| Unsaved ring (`tab-unsaved-<tab>`) | File-bound tabs only. `isDirty(id)` in `editorStates.ts`: the doc differs from the baseline, the doc the file held at its last read or write, or the baseline is unknown. A hollow ring beside the title, apart from the transaction dot in the close slot. |

### Persistence

Everything lives under `app_data_dir()` (override with `L1_DATA_DATA_DIR` in debug builds), in one SQLite file `l1-data.sqlite3` (rusqlite, bundled, WAL journal) with `PRAGMA user_version` migrations:

- `connections(id TEXT PK, json TEXT)`. Configs only.
- `history(id INTEGER PK, at_ms, connection_id, connection_name, database, sql, elapsed_ms, outcome_json, is_write)`, indexed on `(connection_id, id)`. Legacy `history_list` search is `LIKE`. Grouped browsing uses literal substring search across SQL, recorded connection name, database, and error message. Pruned to the newest 10,000 entries on insert. Migration 4 adds and backfills `is_write` in the same transaction as its version bump.
- `kv(key TEXT PK, json TEXT)`. Holds `workspace`, `settings`, and `updates`. `updates` holds only the update track. The update state stays in memory. A file-bound tab's `TabSnapshot.file` holds its `FileBinding` and a `dirty` bit taken in the same pass as `text`. The reader parses `file` on its own and drops a shape it cannot read, so a later build's binding never fails the tab, and with it the workspace that `boot()` would replace with an empty one.
- `secrets(connection_id TEXT PK, nonce BLOB, ciphertext BLOB)`. One sealed password per connection. Migration 2 adds it.
- `ssh_secrets(connection_id TEXT PK, nonce BLOB, ciphertext BLOB)`. One sealed SSH password or key passphrase per connection. Migration 7 adds it. Deleting a connection deletes both rows.
- `saved_queries(id INTEGER PK AUTOINCREMENT, name, sql, database_kind, created_ms, used_ms)`. One typed row per saved query, not JSON. Migration 8 adds it. History pruning and **Clear history** never touch it.
- `chats(id TEXT PK, updated_ms, json)`, indexed on `updated_ms`, and `chat_entries(chat_id, seq, turn, json)` with the key `(chat_id, seq)`. Migration 5 adds both. Deleting a chat deletes its entries. A chat's `json` is a `ChatRecord`, which holds the title, the options, the entry count, and the provider session the next turn resumes, with the config directory the CLI made it under. Each entry's `json` is one `Entry`. A chat row that fails to parse is logged and left out of the list, and an entry row that fails to parse loads as `Entry::Unreadable`.

Provider configs and the health check interval are part of `settings` in `kv`. Probe results stay in memory, so a launch never shows a stale sign-in. An update can install an older build, so a `settings` row that this build cannot parse loads as the defaults and stays as saved until the user changes a setting, rather than stopping `Engine::open`.

The OS credential store holds one item, a random 32-byte master key, with service `dev.l1data.app` and user `master-key`. macOS therefore asks for keychain access once, not once per connection. Each password is sealed with XChaCha20-Poly1305 under that key, with a random nonce and the connection id as associated data, and the result is stored in the `secrets` table. An SSH secret is sealed the same way with `{id}/ssh` as associated data and stored in `ssh_secrets`, so a blob moved between the tables does not open. All passwords do not go into one keychain item because Windows Credential Manager caps an item at 2560 bytes. The first saved password creates the key. Reading a password never creates one. If the key is missing or a password fails to open (wiped keychain, copied data dir, memory store restart), the connection behaves as if it had no password, and the user enters it again at the `MissingPassword` prompt. `trait KeyStore { load, save }` has two implementations: `KeyringKeyStore` and `MemoryKeyStore`. The second exists for tests and e2e, selected by `L1_DATA_SECRET_STORE=memory` in debug builds only. SQLite access is a `std::sync::Mutex<rusqlite::Connection>`. Writes are single-row and sub-millisecond, so they run inline.

### Repo layout

```
Cargo.toml                    # workspace: crates/engine, crates/dev-bridge, src-tauri
package.json  vite.config.ts  tsconfig.json  index.html
README.md  AGENTS.md  CLAUDE.md   # AGENTS.md holds instructions for coding agents; CLAUDE.md imports it
crates/engine/
  src/lib.rs                  # Engine facade
  src/model.rs                # every IPC type (single source)
  src/connection.rs           # ConnectParams, TLS (postgres-native-tls), Live/ConnSlot/ConnEntry, metadata session, draft_from_url
  src/session.rs              # Session: client + driver task + notices + death watch; CancelHandle
  src/console.rs              # Console, SessionSlot, run task
  src/txn.rs                  # pure transaction state machine
  src/sql/{mod,lex,split,classify,relations,format}.rs  # pure lexer, picker, classifier, relation names and read checks, formatter
  src/text.rs                 # UTF-16 mapping, type_ref, synthesize_tag, quoting, browse/count/edit SQL
  src/results.rs              # Arena, pages, each result's Origin
  src/export/{mod,encode}.rs  # export: CSV, JSON, Markdown, and INSERT encoders, the all-rows re-run, its fence and writer
  src/catalog.rs              # 4 catalog queries -> Catalog
  src/store.rs                # SQLite: migrations, connections, history, kv, secrets, ssh_secrets, saved_queries
  src/saved.rs                # saved queries: what a save takes, default names, limits, dedupe, the revision, the list
  src/script/                 # Script as CREATE: pure PostgreSQL and T-SQL DDL assemblers over facts the session drivers read
  src/session/{postgres,mssql}/source.rs   # the catalog facts each Script as CREATE reads
  src/secrets.rs              # Vault + KeyStore (Keyring/Memory), one sealing slot per secret kind
  src/session/stream.rs       # DbStream: a driver's own socket or a channel through the SSH tunnel
  src/tunnel/{mod,auth,known_hosts}.rs   # SSH tunnels: the shared-connection registry, auth methods, host-key trust
  src/entra.rs                # AzCli: Entra ID tokens from `az account get-access-token`, cached until five minutes before expiry
  src/shell_path.rs           # AgentPath and the login-shell PATH that agent CLIs and az are found on and run with
  src/provider/mod.rs         # Provider and sans-IO Codec traits, HostEnv, probes, health loop
  src/provider/agent.rs       # one CLI process (Pipes): its own process group, stdout reader, stderr tail, close and kill; Agent and the one-shot exchange() run on it
  src/provider/claude.rs      # Claude: argv, child env, launch arguments, probe parsing, stream-json codec, the get_usage exchange
  src/provider/limits.rs      # usage limits: reads every provider at once and the Running low rule
  src/assistant/{mod,chat,turn}.rs   # chats, the approval ledger, the turn loop
  src/assistant/tools.rs      # the three tools, the approval policy, run_query; the only file here that calls a session's client
  src/assistant/{context,reply}.rs   # pure: the per-turn context block, reply parsing, recover, recap
  src/assistant/system_prompt.md
  src/update/mod.rs           # self-update: the UpdateTransport seam, the saved track, the async steps, the schedule, the restart impact
  src/update/machine.rs       # pure: the Phase machine, the offer policy, consent, the schedule's due rule
  src/update/scripted.rs      # ScriptedTransport: the named scenarios the dev-bridge and the tests use
  src/bin/fake_claude.rs      # fake-claude: a stand-in CLI with named scenarios, not shipped
  src/bin/fake_az.rs          # fake-az: a stand-in for `az account get-access-token`, not shipped
  tests/pg.rs                 # pg-tests feature: Harness + every scenario
  tests/assistant.rs          # Engine with fake-claude; its pg module runs under pg-tests
  tests/claude_smoke.rs       # claude-smoke feature: the installed claude CLI
  tests/entra.rs              # Entra sign-in with fake-az; under mssql-tests, the dev server's certificate fails it before login
  tests/fixtures/claude/*.jsonl   # protocol lines captured from claude 2.1.301
src-tauri/
  src/main.rs                 # builder, specta export, event forwarder task, plugins, macOS menu, self-check
  src/ipc.rs                  # #[tauri::command] one-liners
  src/updater.rs              # TauriTransport over tauri-plugin-updater; the only file that names the plugin
  build.rs  tauri.conf.json  capabilities/default.json
  app-icon.svg  icons/        # icons/ is generated from app-icon.svg by dev/icons.sh
src/                          # frontend
  main.tsx  boot.ts  index.css
  ipc/bindings.ts             # GENERATED, checked in
  ipc/errors.ts               # typed IPC errors -> user-facing text
  state/{live,workspace,catalog,relationDetail,rowCache,status,tableEdits,transcript}.ts
  shell/{AppLayout,TopBar,TabBar,Overlays,RightPanel,PixelColumn,ResizeHandle,WindowChrome,WindowEdgeActions}.tsx
  shell/{theme,hostedWindow,commands}.ts
  shell/lazySurfaces.tsx      # Settings, the connection dialog, the database map, and the assistant load as separate chunks, preloaded on idle
  ui/primitives.tsx           # shared buttons, menus, dialogs, context menus
  ui/textMatch.ts             # substring matching and highlight ranges for the search boxes
  features/settings/{sections,SettingsScreen,SettingsSidebarBody,SettingLayout,AppearanceSection,QueriesSection,ProvidersSection,AboutSection}.tsx  features/settings/settingsIndex.ts
  features/assistant/{AssistantPanel,Transcript,ReplyText,SqlCard,SqlHighlight,ToolCard,Composer,ChatHistory,providerIcons}.tsx  features/assistant/{assistantActions,assistantText}.ts
  features/sidebar/{Sidebar,SidebarSearch,ConnectionTree,VirtualTree}.tsx  features/sidebar/{treeFilter,detailText}.ts   # the tree flattens its visible rows and virtualizes them, with one shared context menu
  features/connections/{ConnectionDialog,connectionColors}.tsx  features/connections/connectionActions.ts
  features/editor/{SqlEditor.tsx,editorStates.ts,runActions.ts,persistence.ts,sqlIdent.ts,theme.ts}
  features/updates/{UpdateButton.tsx,updateActions.ts,updateText.ts}   # the sidebar update button and its change list card, the install confirm loop, every update string
  features/results/{ResultsPane,ResultGrid,FilterBar}.tsx  features/results/{format,tableQuery}.ts
  features/history/{HistoryPalette.tsx,historyList.ts}
  features/saved/{SavedPalette.tsx,savedActions.ts,savedList.ts,savedPane.ts}   # the Saved queries palette, its pure ranking and keys, and its preview reducer
  testing/shopCatalog.ts      # catalog fixture for vitest
  **/*.test.ts(x)             # vitest, next to the code under test
crates/dev-bridge/             # bin (not shipped): axum on 127.0.0.1:$L1_DATA_BRIDGE_PORT (default 7077), POST /invoke/:cmd + GET /events (SSE)
src/dev/bridgeShim.ts         # loaded only in vite dev outside Tauri: mockIPC -> fetch, SSE -> emit("app-event")
dev/{docker-compose.yml,seed.sql}   # shop + lab.type_zoo
dev/sshd/                     # the ssh-tests bastion: Dockerfile, sshd_config, and test-only keys
dev/bridge-smoke.sh           # curl flow against the dev-bridge
dev/screenshot.sh             # macOS: captures the real app into docs/images/l1-data.png
dev/icons.sh                  # regenerates src-tauri/icons
docs/architecture.md  docs/images/
```

Tracing a run from the button to the grid takes four files: `runActions.ts` → `ipc.rs` → `console.rs` → `live.ts`.

### Verification strategy (what an agent runs)

1. **Pure Rust units** (`cargo test -p engine`, no DB):
   - splitter: dollar quotes, `E'\''`, nested comments, `;` in identifiers, `BEGIN ATOMIC` bodies
   - `browse_sql` quoting of `"we""ird"` identifiers
   - picker: cursor in gaps or at end of doc
   - `txn_after`: every transition in a table test
   - `OffsetMap`: emoji and CJK before the error
   - `type_ref` alias table, `synthesize_tag`, arena paging, CSV quoting
   - classifier: transaction control, CTEs and comments before the verb, `CopyStdio` and `NoTxnBlock`, command tags
   - `browse_sql`: the ORDER BY text before the PK tiebreak, the WHERE text ANDed with column filters, every `FilterOp` with `LIKE` wildcards escaped
   - edit SQL (`insert_sql`, `update_sql`, `delete_sql`): quoted names and values, NULL written as `NULL` or `IS NULL`
   - secrets: seal and open, a reopened vault, one key read per vault, a password sealed for another connection, a lost key, concurrent first seals
   - store: migrations run once, history paging and pruning, migration 8 on a version-7 file
   - saved queries through `Engine`: the cursor's statement, a SQL Server batch, UTF-16 offsets past an emoji, blank and oversized text, default names, dedupe without a new `rev`, which writes raise `rev` and emit, use order, a restart, a row with an unknown `database_kind`, and a defaulted column a newer build added
   - model: wire casing, `RunRequest` tags, the nested `ConnectError` tag, settings and layouts saved by older versions still load
   - `Engine` without a DB: boot on an empty dir, passwordless saves never touch the keychain, persistence across `open`, draft validation
   - updates: the machine's phase tables, the offer policy for each pair of build and track, the schedule's due rule, consent, and the scripted scenarios. Through `Engine` with `ScriptedTransport` on a paused clock: the first check at 15 s, a nightly build switched to Stable offered the older stable and back, the install and relaunch, the implied track saved at first launch and kept by a later stable build, a track saved by a newer build left alone, a failed install, a bad signature, `Busy` during a download, and each sync update command called from a thread with no runtime
   - `src-tauri`: plugin errors sort into `UpdateFailure`, only `updater.rs` names the plugin, `requireSignedVersion` is on, and the webview cannot reach the updater
   - assistant units: the Claude codec against the lines in `tests/fixtures/claude/`, argv, child env, launch-argument parsing, reply parsing into Markdown and SQL blocks, the context block's budgets, the tool policy, `relation_names` and `vet` with the cases the review raised, `recover`, chats and entries in the store, and the two structure tests
   - `Engine` with `fake-claude` (`tests/assistant.rs`), picked through `binaryPath` and `launchArgs = --fake-scenario <name>` as a user would: the probe, a streamed reply with suggestions, `Busy`, Ask first and Deny, a `tools/call` whose arguments differ from the approved input, a `tools/call` with other arguments while its ask is unanswered, an ask whose input differs from its `tool_use` block, Auto, Stop denying a pending approval and a later ask and call, Stop and shutdown killing a process that ignores interrupt and EOF, a follow-up accepted at the End and started only after the last process exited, delete and shutdown waiting for that exit, a crash's stderr tail, the recap after a session lost at start or at the first result or after a changed config directory, a resume that fails for another reason, a restart whose process never starts, a sign-in failure at start, a missing assistant directory, the default options rule, a resumed session and a changed model in argv, recovery after a mid-turn crash, shutdown, delete during a turn and of an unloaded chat, and two opens with one rev
2. **Engine against real Postgres** (`pnpm db:up && cargo test -p engine --features pg-tests`). `Harness` uses `MemoryKeyStore`, a temp data dir, and `L1_DATA_TEST_PG` (default `postgres://l1data:l1data@localhost:5433/shop`). Scenarios:
   - connect reports a 16.x server version
   - catalog of `shop` contains the mockup's tables with PKs and estimates after `ANALYZE`
   - every `lab.type_zoo` cell equals Postgres's own text output: numeric, timestamptz, interval, json/jsonb, int[], text[], bytea, uuid, inet, enum, point, range, NULL
   - mockup query returns 6 rows with `orders: bigint`
   - multi-statement run yields `UPDATE n` tags, and the output log reads Statement, notices, result line for each statement, all with the run's id
   - a notice a deferred trigger raises at `COMMIT` is logged between the commit's `Statement` and `Command` lines, with the commit's run id
   - a statement's logged text is cut at 2,000 chars with `…`
   - error position maps to the right UTF-16 offset
   - cancel `pg_sleep(30)` finishes `Cancelled` in under 2 s
   - manual isolation and commit (call site 4); error leads to `Aborted`, then rollback leads to `Idle`
   - `pg_terminate_backend` of a console pid gives `Lost { transaction }`
   - `generate_series(1, 300000)` truncates at `max_rows` with `rows_seen = 300000`
   - CSV export equals `COPY ... TO STDOUT CSV` output; a SQL `INSERT` export of `lab.type_zoo` rebuilds every value
   - an all-rows export streams past the row cap inside an open manual transaction, leaves that transaction writable, keeps the grid's result, and records no history; Esc leaves the target file and no temp file; a write is refused; a `nextval` re-run fails read-only without advancing the sequence; a table tab's all rows is the whole filtered table in page order
   - history persists across `Engine::open` on the same dir
   - DDL refreshes the catalog and the connection snapshot carries the new version; a DDL burst behind a busy metadata session causes exactly two loads
   - a table page on a second database (`reporting`) orders by that database's primary key; catalog versions keep rising across a reconnect; history records the console's database; a database no tab or tree row uses gets `NotShown`
   - `exact_row_count` is exact, reports `NotConnected` before connect, and fails on a missing table
   - an update install asks again when a transaction opens or gains writes after the confirm, and a running query is in the restart impact while the impact's own `has_writes` probe is not
   - a session behind a frozen relay holds neither the restart confirm nor a disconnect past the probe deadline, and its unanswered probe counts as writes
   - a forced disconnect during a manual run leaves the tab idle, and a run orphaned by disconnect never writes into the next run
   - the console's schema sets `search_path`, `COPY ... FROM STDIN` and `TO STDOUT` are rejected as `Unsupported`, and the session survives
   - bad credentials fail with `Auth`, and `test_connection` works
   - `table_apply` commits every change or none, and in manual mode waits for Commit while a failure keeps the open transaction
   - table pages sort and filter in the engine
   - SQL comments in WHERE and ORDER BY keep the page limit
   - assistant queries: Postgres refuses each write the lexer let through, a session advisory lock dies with the call, the statement timeout and the row and cell caps hold, no row crosses the byte budget and a first row that would is cut to fit, a wide row stays within the budget in the stored result and the model text, a manual tab's uncommitted insert stays untouched and unseen by an approved count, `describe_tables` lists a foreign key and an index, the context sends the schema outline once per provider session, every vetted name is a `pg_catalog` function with no volatile overload, every vetted operator symbol is a `pg_catalog` operator with no volatile implementation, the keyword lists match the server's, and Auto asks for a `U&`-escaped call
3. **Frontend units** (`pnpm test`, vitest, 38 files):
   - `live.apply` rev ordering and exhaustiveness
   - numeric grouping on text (`-1234567.890` → `-1,234,567.890`), page labels, elapsed time, TSV
   - the catalog view's qualified tables and functions, substring matching, the tree filter, and the settings filter
   - completion: the options for each engine reply, column alias lists, case matching per dialect, the 250 ms limit on join details, one request in flight, and a request asked again after a declined one
   - `RowCache` page math
   - status derivations, workspace snapshot round trips (a table tab's text and mode, old table tabs, dropped schemas)
   - staged edits → `TableChanges`, and edits held still while a submit is in flight
   - header sort cycling and the `ORDER BY` text round trip
   - browse-query quoting and serialized settings writes in `runActions`
   - grid header icons and key navigation, history list paging, settings screen, window chrome, theme, global shortcuts, the bridge shim
   - the assistant: `live.apply` for providers, chats, entries, and tombstones, turn grouping, tool lines, provider status text, ignored launch arguments, send requests with UTF-16 offsets and an attached result, Replace tab as one undo, the resync check, SQL cards, and the panel's pickers, approval card, and failure states
4. **Contract drift**: a `src-tauri` test (`export_bindings_up_to_date`) renders bindings to a string and asserts equality with `src/ipc/bindings.ts`. A second test asserts the dev-bridge dispatch table covers exactly the registered command names. The dev-bridge also tests that `app_boot` replies with the snapshot and that bad requests are bridge errors, not command errors.
5. **UI against the real engine** (`pnpm bridge`). `crates/dev-bridge` hosts the same `Engine` (MemoryKeyStore, temp `L1_DATA_DATA_DIR`) behind `POST /invoke/:cmd` and an SSE stream of `AppEvent`. It listens on 127.0.0.1 at `L1_DATA_BRIDGE_PORT` (default 7077) and accepts requests only from the vite origin at `L1_DATA_VITE_PORT` (default 5173). Each variable takes only ASCII digits for a port from 1 to 65535. Vite checks both only when it serves, and passes the bridge port to the page, so separate port pairs run separate bridges side by side. In vite dev outside Tauri, `src/dev/bridgeShim.ts` installs `mockIPC` from `@tauri-apps/api/mocks` (with `shouldMockEvents`) that forwards every command to the bridge and re-emits SSE frames as `app-event`. The generated bindings run unchanged. An agent drives it with agent-browser or Playwright, selecting by `data-testid`:
   - create connection via the dialog, test it, save it, and see "Live" plus `customers 18.4k`-style estimates in the tree
   - ⌘P, type "ord it", and Enter opens `order_items`; the table tab shows 500 rows
   - new tab: type the mockup query, ⌘↵, see status "6 rows ·", the `orders` header tooltip `orders bigint`, right-aligned numbers
   - run `select slow_echo(30)`, click Cancel, see the results status show `Cancelled · …`
   - switch to Manual: insert, then a second tab's count is unchanged; Commit, and the count changes
   - reload the page: tabs, text, and history are restored
   - **Settings** > **Providers** with the real `claude` reads `v2.1.301 · Signed in · Claude Max`, a bad binary path reads Not found, and Reset restores it
   - the mockup question streams a reply. In Ask first, `run_query` waits for Run, the SQL card reads "Uses orders, customers", and Run in new tab, Insert at cursor, Replace tab with ⌘Z, a suggestion chip, and Stop each work. Auto runs a query without asking, and a `DELETE` is refused and comes back as an Open in new tab card
   - after a bridge restart, the chat reopens from history and a resumed turn answers from memory
   - with `L1_DATA_UPDATE_SCENARIO` (`update/scripted.rs` defines the scenarios): `available` checks, downloads with a progress ring, and confirms a restart that names a running query and an open transaction, then shows Restarting; `nightly-build` switched to Stable in **Settings** > **General** offers 0.1.0 as an older build; `offline` and `install-fails` show their failure text; `off` shows no button
   - with no signed-in CLI, `fake-claude` stands in. Build it with `cargo build -p engine --bin fake-claude`, then set **Binary path** to `target/debug/fake-claude` and **Launch arguments** to `--fake-scenario <name>`; the comment at the top of `crates/engine/src/bin/fake_claude.rs` lists every scenario, and `card`, `query`, `slow`, `wedged`, `crash`, and `resume-lost` are the ones to start with
6. **Real shell.** `pnpm self-check` launches the debug binary with `L1_DATA_SELF_CHECK=1`: it boots `Engine` inside Tauri, connects to the dev DB, runs `SELECT 1` and loads the catalog, prints JSON, and exits 0. `pnpm screenshot` covers WKWebView rendering on macOS. It starts a throwaway Postgres, seeds a workspace and an assistant chat that `fake-claude` answers through the dev-bridge, launches the debug app on that data dir, and captures its window with `screencapture` into `docs/images/l1-data.png`. Driving the real webview with `@wdio/tauri-service` (embedded provider, verified to support macOS) is not built.
7. **Real CLI.** `cargo test -p engine --features claude-smoke --test claude_smoke -- --nocapture` needs a signed-in `claude` and `pnpm db:up`. It spends three Haiku turns at low effort. It checks what `fake-claude` cannot: the CLI accepts the argv and the child env, a resumed session remembers the earlier turn, and a `can_use_tool` ask approved after 75 s still runs its query.

## Synthesis decision

Arena over three runners (opus, fable, sonnet), cross-judged by fable. Base: the opus candidate, scored 29/30 by the judge against 21 for each of the others, and picked independently by the orchestrator. It was the only candidate right on all three load-bearing questions: server text output for every value (no per-type decoders), the session moving into the run task so concurrent use cannot be expressed, and a row cap that drains instead of cancelling so a big SELECT cannot abort a manual transaction.

Grafted:
- From sonnet: the dev-bridge plus `mockIPC` shim, so an agent drives the real UI against the real engine from a browser; the `L1_DATA_SELF_CHECK` shell smoke; history pruning at 10,000 entries; splitter cases for `BEGIN ATOMIC` bodies and a cursor in trailing whitespace; events as a plain specta type sent with `app.emit`.
- From fable: table-page SQL built in Rust (`RunRequest::TablePage`), so SQL text has one interpreter; elapsed time measured around execution only, excluding the describe round trip.

Rejected:
- Sonnet's binary decoder: an open-ended surface with hex fallback for unknown types. Its cap-hit `cancel_query` also aborts a manual transaction while reporting success.
- Fable's rows-into-JS streaming: it holds every result twice, puts 50k rows in the WebKit heap, and lets cap and export policy leak into the frontend.
- Fable's character offsets: CodeMirror sends UTF-16 units, so positions need the explicit `OffsetMap` mapping.
- `@wdio/tauri-service` as the v1 e2e path: kept as the next step. The bridge works with the browser tooling agents already have, and the shell smoke plus a screenshot cover the Tauri-specific gap.

## Tradeoffs accepted

- **Prepare costs a round trip.** We accept two round trips per statement (Parse/Describe, then simple-protocol execute) in exchange for exact PG text output plus full column type metadata, with zero per-type decoders. Displayed elapsed time excludes the describe.
- **Values are text, not typed JSON.** Future client-side sort or charts must parse text by `TypeCategory`. Numeric precision is never silently lost, and unknown types always render.
- **One connection per open tab.** Ten tabs means ten backends. We accept that in exchange for faithful per-tab session semantics. An idle-session reaper can come later.
- **The row cap drains instead of cancelling.** This avoids aborting a manual transaction because a SELECT was large. A huge accidental result keeps the network busy until it finishes or the user presses Cancel.
- **Command tags are synthesized** from the classified verb and the row count, because tokio-postgres doesn't expose the textual tag. Exotic tags (`SELECT 0` from `CREATE TABLE AS`) may read slightly differently from psql.
- **The catalog loads eagerly per connection.** Search, completion, and the tree become trivial. A database with hundreds of thousands of columns makes a multi-MB snapshot (see risks).
- **Snapshots cost more bytes per event than deltas.** We accept that for an idempotent, order-tolerant frontend projection with no merge logic. Streaming snapshots are throttled to 10/s.
- **`ipc.rs` is a pass-through layer.** It is kept only as the IPC adapter: State extraction, specta registration, and error typing. The engine crate builds and tests without Tauri or WebKit.
- **Results die on re-run.** No pinned result tabs in v1. `ResultRef.run_id` makes stale page requests return `Gone` rather than wrong rows.

## Alternatives considered

- **Ship every row to the webview** (JSON or Arrow, columnar, capped). This makes the backend simpler. But the cap, memory, and export policy all leak into the frontend, export round-trips the data back through IPC, and a 200k-row result lives in WebKit's heap. The interface looks smaller, but the caller ends up owning the hard part.
- **Binary protocol with tagged typed values** (`{ kind: "numeric", value: "..." } | { kind: "array", items: [...] }`). This needs a decoder per PG type with a never-ending tail, and every type renders differently from psql. It exposes PG's wire representation on the public contract.
- **sqlx with `raw_sql`.** This gives text values and type info in one round trip, but sqlx cannot send a CancelRequest, and Cancel is a v1 requirement.
- **A shared pool (deadpool) for auto-commit tabs, with dedicated connections only for manual tabs.** Fewer connections, but `SET`, temp tables, and `search_path` silently vanish between runs. It also creates two session models that callers must understand.
- **Server-side cursors with fetch-on-scroll** (DataGrip style). This keeps memory bounded for arbitrary results, but it holds a transaction open in auto-commit tabs and conflicts with manual-mode semantics. Worth revisiting behind the same `result_page` API if the cap proves too small.
- **Per-run Tauri `Channel` for progress, with global events for status.** This would give two delivery mechanisms and two reducers. One global `AppEvent` with snapshots is simpler, and nothing is high-frequency enough to need a channel because rows never travel in events.
- **Monaco, glide-data-grid, Solid.** Monaco is heavy, its SQL support is generic, and it needs worker setup in Tauri. glide-data-grid is canvas-based: it is harder to theme to the mockup and has weaker text selection, and DOM virtualization is already fast enough because rows arrive windowed. Solid's fine-grained reactivity buys little when the grid is window-fetched, and it gives up Radix and agent fluency.

## Implementation reconciliation

- **Command panel.** `shell/commands.ts` owns the exported `COMMANDS` registry shared by global keyboard lookup and `shell/CommandPalette.tsx`. The panel opens on `⌘⇧P`, leaving `⌘P` quick navigation unchanged. Its transient TypeScript `commands` overlay changes no Rust, IPC, or persisted workspace types. Existing shell actions own execution. Optional registry predicates hide unavailable actions and are checked again before activation. Workspace and editor-buffer subscriptions update those rows as context changes. Save and Save As require an active workspace tab. Save query also excludes unchanged table-browse text through `isBrowsing`. Context labels describe closing Settings. The opener preserves every unrelated overlay and focuses the existing panel without resetting its search. Above Settings, cancellation preserves the screen and filter. The Radix combobox uses case-insensitive query words, wrapping arrow selection, Enter activation, and composition guards. Activation closes the panel before calling the action synchronously, so file actions can open their dialogs. Cancellation restores a connected opener only without a replacement overlay. After activation, focus returns to the workspace only when no successor dialog owns it.

- **Database map.** Two stateless engine commands in `database_map.rs` use a database's metadata slot. Both validate the requested catalog version under its metadata lock and check that the instance and slot are still current before returning. A neighborhood uses one root lookup and one bounded incident-constraint query in each private driver, rather than table-detail calls or a full-catalog relationship load. PostgreSQL suppresses the same referenced-partition clones as relation detail. SQL Server uses its metadata visibility and reports disabled and untrusted declarations. Endpoint descriptors carry only identity and participating FK column names. Composite pairs retain constraint order; constraint IDs preserve parallel and self-referential keys. Catalog versions are application refresh epochs, not frozen external-DDL snapshots. Refresh the catalog after outside DDL.
- **Map query analysis.** `sql/map.rs` shares the dialect lexers and cursor-statement choice. Occurrence IDs differ from relation OIDs, so self-joins retain separate aliases and source ranges. A schema-qualified exact identifier match means a catalog match, not proof of a tab's runtime resolution. A unique bare name is tentative. SQL Server comparisons retain exact case because the engine does not assume a collation. CTE references, derived tables, temporary and cross-database forms are never invented as catalog tables. Supported joins require complete direct `alias.column = alias.column` ON conjunctions, unique bindings and known columns on catalog-matched endpoints. OR, casts, subqueries and unsupported source syntax suppress guessed joins and report partial analysis. The overlay analyzes the cursor statement even when text is selected.
- **Transient map dock.** `features/databaseMap/controller.ts` holds a discriminated lifecycle and a normalized graph inside one `DatabaseRef`, keyed by relation OID and constraint ID. No stored workspace, layout or tab variant changes. `graph.ts` merges complete pages atomically, retains dragged positions, and rejects pages beyond 160 nodes or 512 edges with a visible limit and focus/reset actions. The request queue allows two neighborhood calls in flight, including superseded generations. Query replies must match map generation, database, catalog version, tab, document identity and selection. Catalog refresh or disconnect invalidates the graph. The active query's different database clears its overlay. HTML cards and SVG edges live beside the existing editor/results split in a resizable dock. Solid edges are declared keys; dashed edges are supported SQL equalities. Name matches and partial analysis are labeled. `editorStates.ts` owns a dedicated map decoration effect and a document/selection subscription. Highlighting revalidates its captured context and changes neither text, history, selection nor run target. Edits, context changes and closure clear the effect independently of chooser previews and run flashes.
The map uses the last good catalog after a failed refresh; cursor changes do not retry the full catalog load. Its opening viewport keeps the focus readable, while Fit shows the whole explored graph. Declared keys and SQL equalities share the 512-edge display budget, with a notice when SQL predicates are hidden. EXPLAIN wrappers retain CTE shadowing in source analysis.

These items record where the current implementation differs from or adds to the original PostgreSQL sketches above.

- **Completion reads SQL in the engine.** lang-sql's alias and keyword completion is gone, so the frontend no longer parses SQL for completion. `completion_context` names the clause and the relations in scope, and the frontend only matches names against the catalog and the relation details. The engine also owns quoting. `text::quote_if_needed` quotes a name only when the bare word would not read back as that name: PostgreSQL quotes anything but a lower-case identifier that is not a reserved or type-or-function-name keyword, and SQL Server brackets anything but a plain identifier that is not a reserved keyword. Each catalog `SchemaInfo`, `Relation`, `ColumnInfo`, and `FunctionInfo` carries that form as `quoted`, PostgreSQL's `insertText` is the quoted call, and a reply's derived columns and column aliases come in that form. `OffsetMap` records whether the text is ASCII and resumes each conversion from the last one, so converting every statement of a long script stays linear.

- **Connected database names come from the server.** `ConnectionConfig.database` remains the saved login locator. `InstanceState.configuredDatabase` records the exact locator accepted by that connected instance, and `home` records the server's canonical name. Engine boundaries and the frontend resolve only that exact alias to `home`. All other database names keep exact identity. A saved-config edit does not alter the running instance's alias. Once connected, console snapshots, catalogs, result metadata, history, and restored workspace targets use the resolved name. Only the metadata home session reads `DB_NAME()` to resolve the accepted login locator. Console sessions log in using that canonical target and check exact protocol database changes against it. No global case folding is used, so case-sensitive database names remain distinct.

- **Connection kind selects the driver and dialect.** `DatabaseKind::{Postgres, SqlServer}` is saved as `ConnectionConfig.databaseKind`. A hand-written reader loads older connections as PostgreSQL. An existing connection ID cannot change kind, preventing a saved edit from disagreeing with open sessions or database-scoped catalogs. Editors select CodeMirror's PostgreSQL or MSSQL dialect from that same kind, including before a catalog loads.
- **Native database clients stay inside private session drivers.** One enum-backed `Session` completes connection, metadata, execution, transaction, count and staged-edit operations. PostgreSQL retains prepare plus simple-protocol text streaming and its transaction reducer. SQL Server uses a narrow vendored Tiberius extension with rustls, SQL login or a Microsoft Entra ID token over TCP, and SQL Server catalogs. Login acknowledgements and response ENVCHANGE tokens supply session identity and transaction presence without executing observation SQL on a tab session. The protocol exposes neither transaction nesting nor committability, so an open transaction is `TxnState::Open` with `txnDepth: null`; a closed transaction is `Idle` with depth zero. Selecting a schema never issues `ALTER USER` or changes the SQL Server login's default schema.
- **Entra ID tokens come from the user's Azure CLI.** `ConnectionConfig.sqlServerAuth` is `SqlLogin`, `EntraAzureCli`, or `None`, which is a SQL login and is what every row saved before the field reads as. It is a new optional field, not a new variant of an existing enum, so the previous stable loads an Entra row as a SQL login with no password and fails to connect with an `Auth` error. If the user edits and saves that connection in the previous stable, it rewrites the row without `sqlServerAuth`, so this build then loads it as a SQL login and the user must choose Entra again. `environment` survives that save, because every build since #50 reads and writes it. A value this build does not know, such as one a newer build saved, reads as `Unknown`. Connect, Test, and save refuse it with text that names the newer version, so the row keeps its method until the user picks one. A save of that row with `sqlServerAuth: null` is refused too, because it names no method. `Store::connections` skips a row that does not parse, so one unreadable row cannot hide the others. It logs each such row once per process, and `BootSnapshot.unreadableConnections` counts them, so the app shows one toast at boot that says how many saved connections are hidden. A connect looks up its one row with `Store::connection` and does not scan the rest. A save stores `None` for PostgreSQL. An Entra save deletes the sealed password and saves no user, so the previous stable, which reads the row as a SQL login, has no user to try with an empty password. The dialog sends no user for Entra either, and sends `Clear` for the password, even one typed under SQL login before the switch. It shows neither field for Entra or for an unknown method. `entra::AzCli` runs `az account get-access-token --resource https://database.windows.net/` and keeps the token in memory until five minutes before its `expires_on`. A reply without `expires_on` is used once. `expiresOn` is local time with no zone, so it is not read. The lock is held across `az`, and a caller that waited on a call in flight takes that call's result, success or failure. So concurrent session opens make one call, and a hung `az` fails them all together at its 30 s timeout. `ConnectParams.entra` carries the shared `AzCli` to `MsSession::open`. The token's audience is every Azure SQL database, so `MsSession::open` refuses Entra with `Require` and no CA, the mode that skips the certificate check, before it runs `az`. Azure SQL certificates are publicly trusted, so `VerifyFull` works there. The dialog explains this and disables **Test** and **Save**. A certificate the app cannot verify fails with text that says what the user can do: for Entra, use a server with a publicly trusted certificate, and for a SQL login, fix the certificate or choose `Require`. The text ends with the driver's reason, and its note that a too-old certificate is often SQL Server's self-signed one, and drops the driver's advice to developers, such as `Config::trust_cert()`. `MsSession::open` dials first, so a host it cannot reach costs no `az` call, then takes the token and logs in with `AuthMethod::AADToken`. Error 18456, or 18452 (a login from an untrusted domain) on an Entra connection, drops that token, so a user who runs `az login` as another account gets a new token at the next try. A missing or signed-out `az` fails as `ConnectError::Auth` with text that names `az login`. The program is `az` on the PATH that `shell_path::search_path` builds for agent CLIs. That PATH keeps only absolute entries, including the install directories it adds under `HOME`, which it skips when `HOME` is unset or relative, and the child runs with it, so neither the lookup nor the `#!/usr/bin/env bash` line of `az` can pick a program from the working directory. The same holds for agent CLIs. `EngineConfig.az_binary` replaces it, and only the dev-bridge sets that, from `L1_DATA_AZ_BINARY`. The app passes `None`, so a release build never runs another program as `az`. A pasted `sqlserver://` URL reads `Authentication=ActiveDirectoryAzCli` from its query or from JDBC-style `;key=value` properties. The properties start at the first `;` that has an `=` before the next `;`. The scan covers the user info and the host, and when they hold no such `;`, the path, as in `host/shop;encrypt=true`. A `;` without one, such as `pa;ss` in a password or `my;db` in a database name, reads as part of it, as before. An empty property right before the first one, as in `host;;database=x` or `host/shop;;encrypt=true`, is refused. The user info ends at the last `@` before the properties. When the properties start in the user info or the host, an `@` after the first property is refused unless it is in the `user` value, as in the `user=admin@server` of Azure portal strings, because the URL could also read with that `@` ending the user info and moving the host. The message asks for a `;` in a password as `%3B` and an `@` in another value as `%40`. The properties run to the end of the URL, so a `/`, `?`, or `#` after them is refused with a message that names that character and its percent-encoding. The properties `encrypt=true` and `trustServerCertificate` that Azure portal strings carry map to `VerifyFull`, or to `Require` when the certificate is trusted. The engine stores no Azure credential. Tenants other than the CLI's default, device-code sign-in, and Windows authentication are not supported.
- **SSH tunnels run inside the engine.** A connection with `ssh` reaches its database through an SSH server. The engine's russh client opens a `direct-tcpip` channel for each session, and the driver speaks over it as a `DbStream`. No local port opens, so no other process can use the tunnel. The SSH server resolves and dials the database host, so a name only the bastion knows works. PostgreSQL logs in over the channel with `connect_raw` and still verifies TLS against the database host. `connect_raw` leaves the cancel token without a socket, so a cancel opens a fresh channel and sends `cancel_query_raw`. SQL Server speaks TDS over the same stream. russh's client starts its connection task with `tokio::spawn`, so only async paths dial, and `ssh_trust_host_key` is sync and uses std alone. The timeouts are 10 s for TCP and key exchange, 60 s for authentication, which covers a hardware key's touch, and 10 s for each channel. Keepalives go every 15 s, and three unanswered ones close the connection.
  - **One SSH connection per connection and spec, shared by reference count.** `Tunnels` keys a route by the connection id and the whole spec, secret included, so an edited config dials anew while open sessions keep the old connection. The registry holds each connection weakly and each stream holds it strongly, so it closes with its last stream. Dials are single-flight. Concurrent opens make one TCP connection, one key exchange, and one authentication, and a failure goes only to the callers that waited on that dial, so the next open dials again. A dead connection is redialed at the next open, and an open that raced its death is retried once. Nothing reconnects in the background. The existing death watches turn a closed channel into `Lost` or a failed slot. Test gets a route nobody shares, so it always dials and authenticates. Disconnect and delete forget the connection's routes.
  - **The SSH secret has its own slot.** It is a password for `SshAuth::Password` or a passphrase for `SshAuth::Key`. A save keeps it under `Keep` only while the auth kind is unchanged, so a password is never tried as a passphrase, and agent auth stores none. `Shared::params` fails with `SshError::MissingSecret` before dialing when password auth has no saved password.
  - **Host-key trust covers only keys the engine observed.** The engine reads `~/.ssh/known_hosts` and never writes it, and it reads and appends `<data_dir>/known_hosts`. Tests pass `EngineConfig.user_known_hosts = None`, so they never read the developer's file. A key on an `@revoked` line fails. A key that either file lists for the server is trusted, as OpenSSH trusts a host name shared by several servers. When neither file lists it but one lists another key of the same algorithm for the server, it fails as `HostKeyChanged`, and its text names the `file:line` and the `ssh-keygen -R` fix. A file that exists but cannot be read fails as `SshError::KnownHosts`. An unknown key fails as `HostKeyUnknown` and waits under a `HostKeyId` for ten minutes, at most 32 at a time. `ssh_trust_host_key` appends only a waiting key, once. `SshError` adds `HostKeyRevoked` and `KnownHosts` to the variants the design named.
  - **Head-of-line limit.** russh grants a channel more window as data arrives and then waits for room in that channel's buffer inside the connection's one loop. A stream nobody reads therefore stalls every session on its SSH connection once 256 packets queue for it. The run task drains its results, so this takes a reader that stopped.
  - **Not supported.** `~/.ssh/config`, `ProxyJump`, host certificates, and known_hosts patterns with wildcards or negation, which russh's matcher treats as no match. Keyboard-interactive auth answers one hidden prompt with the password and fails on anything more.
- **An execution unit can produce several results.** `StatementRun` holds terminal progress separately from its `RunOutput` values. Each row output owns its metadata and streaming/completion state. `ResultRef.index` is a result ordinal within the run, independent of the execution-unit index. Empty result sets and rows received before a later error remain inspectable. Rows stay in Rust arenas, and IPC still transfers only metadata and requested windows.
- **T-SQL runs as batches.** Its separate lexer recognizes bracket identifiers, Unicode strings, variables, nested comments and standalone `GO` separators. It preserves semicolons, procedure bodies and variable scope within a batch. Selected text runs exactly as selected. PostgreSQL retains its existing statement splitter and formatter. `script_outline` and `editor_format` receive `databaseKind`; stale frontend replies also check the target. Unsupported formatting, GO repetition, sqlcmd directives and explicit `USE` are refused. The driver checks the actual execution database after a batch.
- **SQL construction stays in Rust.** `table_script` returns `TableScript` with canonical `text` and `acceptedTexts` for browse recognition. PostgreSQL accepts its canonical text with or without one trailing semicolon. The frontend trims outer whitespace and compares only these engine-supplied strings, preserving saved document bytes and table paging, filters, order and editing. Its template cache is scoped by connection and dialect, and stale dialect replies are discarded. The template is independent of database spelling, so resolving a saved login locator to the server's canonical database name preserves browsing. `table_order` inspects or cycles header sorting while preserving advanced raw expressions. SQL Server uses bracket quoting, escaped `LIKE`, `COUNT_BIG` and `OFFSET/FETCH`. Raw filter and order expressions pass conservative validation before being placed in generated SQL. Checking only that text has one GO batch cannot exclude a second T-SQL statement. General query results have no guessed table provenance. SQL Server table pages attach catalog column sources and types only after checking their returned shape against the database's catalog. PostgreSQL table pages preserve their native result types and column sources, including fully unwrapped domain types.
- **Script as CREATE reads definitions on the metadata session (PR: script objects as CREATE and open their source).** `object_script` takes a `ScriptTarget` (`Relation`, `Routine`, `Index`, or `Trigger`) and returns an `ObjectScript`. The session drivers read catalog facts in `session/postgres/source.rs` and `session/mssql/source.rs`, new files beside each driver's `detail.rs` rather than additions to it, and the pure assemblers in `crates/engine/src/script/` write the DDL, so native driver types stay in `session/`. A script's coverage is a fixed list of property kinds, kept in the Script as CREATE entry of `docs/features.md`: it writes each kind in the written list, and for each kind in the warned list it queries whether the object has it and, when it does, `ObjectScript.warnings` names it and the text opens with one `-- L1 Data could not script:` comment line per warning, which the tree also shows in a toast. A kind in neither list is not covered. The hostile round-trip fixtures in `crates/engine/tests/fixtures/`, with the SELECT-only login and partition tests, hold an object for each listed kind the dev images can create, and the round-trip tests assert that each was replayed or warned. Kinds the images cannot create are covered by unit tests only: PostgreSQL tablespaces, which need a server directory, and SQL Server CLR triggers, FILESTREAM, Always Encrypted, full-text indexes (not installed), hash indexes (only on memory-optimized tables, which are refused), and `SUPPRESS_DUP_KEY_MESSAGES` (no statement sets it). The partition name restore identifies each index it renames by its partition and its parent index, never by name alone, so a replay never renames an index the script did not create. A foreign key from one table of a partition tree to another is added after the whole tree, because the referenced partition may come later. PostgreSQL reads inside `BEGIN READ ONLY` with `SET LOCAL search_path = pg_catalog`, so `pg_get_viewdef`, `pg_get_constraintdef`, `pg_get_indexdef`, `pg_get_triggerdef`, `pg_get_ruledef`, and `pg_get_expr` qualify the names they print. `pg_get_functiondef` qualifies the routine's own name and argument types, but a routine body is the stored source text, so its names stay as written. The transaction is a driver transaction whose drop rolls back, so a failed or dropped read cannot leave the metadata session inside it. Names the engine prints itself are quoted only when needed, as the server does. A column prints as `serial` only when its sequence has the name, type, and options `serial` gives; any other owned sequence is created by name and attached with `OWNED BY`. A partitioned table's script carries its whole partition tree, read with one query per kind over every partition's OID (`= ANY($1)`) rather than one round of queries per partition, each partition after its parent with the defaults, `NOT NULL`, and storage settings that differ from what `PARTITION OF` copies, the enabled state and comment of each trigger it cloned from its parent, a generation expression that differs from its parent's, and a replica identity that uses an index it attached. An inheritance child names its parents with `INHERITS` and declares only its own columns and constraints. An index that backs a constraint scripts as `ALTER TABLE ... ADD CONSTRAINT` on both servers, because a unique index would replay without the constraint. A typed table scripts as `OF` its type. On both servers, a foreign key to its own table through a unique index that no constraint owns is added after the indexes, because the index does not exist yet inside `CREATE TABLE`. The queries read `attcompression` only on PostgreSQL 14 and later and `stxstattarget` only on 13 and later, by `server_version_num`. Privileges are warned, never granted, because the grantees may not exist where the script runs: each warning lists the privileges granted, revoked, or denied on the object, its columns, and the sequences a PostgreSQL table script creates. On SQL Server, a login without VIEW DEFINITION on the database sees only some grants, and one without VIEW ANY SENSITIVITY CLASSIFICATION may not see every classification, so the script reads `HAS_PERMS_BY_NAME` for both in the same batch and warns that its list may be incomplete. The database-level check is deliberate: an object-level VIEW DEFINITION grant still hides other principals' grants. PostgreSQL compares each ACL with `acldefault`, so a revoked default such as `EXECUTE` from `PUBLIC` is listed too. Security labels, publication membership, an invalid index, and a partitioned index not attached to every partition are warned as well. SQL Server modules keep their `OBJECT_DEFINITION` text verbatim under the `SET ANSI_NULLS` and `SET QUOTED_IDENTIFIER` values from `sys.sql_modules`, with `GO` between batches; only the name in the `CREATE` header is rewritten. A one-part name always becomes the module's current schema and name, because it would otherwise replay in the default schema of whoever runs the script, and a two-part name is rewritten when `sp_rename` or `ALTER SCHEMA ... TRANSFER` left it stale. Tables are assembled from the system catalogs, view scripts carry their indexes and triggers, and a filtered index, an index on a view, and an index or key on a computed column get the `SET` options they need. A SQL Server script reads its catalog facts in one multi-statement batch, because each round trip costs more than the catalog query it carries, and reads which catalog columns the server has (`COL_LENGTH`) once per session, so SQL Server 2016 reads constants where 2017 and later read `history_retention_period` and the other new columns. Sensitivity classifications replay with their table. A CLR or encrypted trigger is warned in its table's or view's script, with its extended properties, and **Open source** on it returns `Failed`. For a login without VIEW DEFINITION, SQL Server hides computed expressions, default, check, and filter definitions, and module text while it still shows the objects, so the script warns about each one instead of writing a plain column or dropping it. `STATISTICS_INCREMENTAL` is warned, because it fails on the unpartitioned index the script creates, and so is `SUPPRESS_DUP_KEY_MESSAGES`, which no T-SQL statement sets on SQL Server 2022. Aggregates and SQL Server memory-optimized, ledger, graph, external, and FileTable tables return `Failed` with a message.
- **SQL Server metadata visibility is optional description, not object identity.** Readable columns and executable routines remain in the catalog when a declared alias type is hidden. Column metadata resolves the independently visible system type; routine annotations prefer a visible declared type, then its system type, then `unknown`. Hidden default, computed and CHECK expressions are omitted while available identities and structure remain. Native value conversion still refuses unsupported representations.
- **Routine insertion text belongs to the catalog.** Each nonpersisted `FunctionInfo` carries required `insertText` built by its Rust catalog driver. SQL Server quotes schema and routine names separately with bracket quoting. Functions insert expressions, and procedures insert `EXEC` calls without parentheses. Ordered `sys.parameters` rows supply editable `NULL` placeholders, excluding the return parameter. Read-only table-valued arguments use `@table_argument_N` placeholders that require user declarations. Output capture requires user variables and `OUTPUT`. These are templates based on visible metadata, not a promise that `NULL` is a valid argument value or that catalog access grants execution permission. PostgreSQL inserts `name()` for every routine kind and overload, with the name quoted when the bare word would not read back as it, such as `"MyFunc"()`. Both tree paths pass the source `DatabaseRef` to insertion. At double-click, the source and active tab's target must resolve to saved connections of the same kind. An unresolved source or target, or a kind mismatch, shows a toast without changing editor text, cursor, undo history, or the tab's target. Same-kind insertion uses the template verbatim at the active cursor without interpreting SQL. Building a catalog never executes its routine templates.
- **Header sorting resolves names in its table's catalog.** `table_order(db, schema, table, order_by, edit) -> Result<TableOrderView, String>` reads the SQL Server database slot's last catalog synchronously, without opening a slot or starting a refresh. Additive header edits require parsed names to match that table's catalog columns exactly. Unresolved names produce no header indicators, and an additive edit returns an error without changing the query or running it again. A nonadditive click can replace an unresolved simple name with the clicked canonical column. This preserves distinct `id` and `ID` columns on case-sensitive databases without guessing identifier equivalence from case or collation. PostgreSQL header sorting needs no catalog. Frontend inspection keys include the table and catalog version, and queued edits discard replies after the target table or query changes.
- **SQL Server conversion preserves exact values or fails.** Decimal text uses the driver's integer and scale, without a floating-point intermediate. Stock Tiberius decodes money through `f64`, which is lossy, so the vendored driver returns it as a decimal of scale 4. A `sql_variant` shows its base value, and a CLR value shows as hexadecimal. The catalog marks every column with `system_type_id` 240 as CLR, because `TYPE_NAME(240)` is NULL and a page's result and catalog categories must match. Unsupported values fail instead of returning debug strings. `SqlTypeRef` replaces `PgTypeRef` and carries display text and category. Existing relation OID and attribute-number wire fields hold database-local object and column IDs with checked conversions. SQL Server triggers identify a SQL module rather than a PostgreSQL trigger function.
- **SQL Server table browsing is read-only.** The engine refuses `table_apply` for SQL Server before catalog, session, or mutation work. The grid disables add, update, delete and submit controls, while paging, filtering, sorting, copy and export remain available. Users can write native SQL scripts. A trigger can commit or replace a transaction, including through cascades or DDL, so a client-side wrapper cannot establish atomic staged edits. The unsupported driver edit lowering and live-lock validation were removed instead of retaining unreachable write machinery. PostgreSQL staged edits keep their existing transaction and savepoint behavior.
- **SQL Server Explain is a sibling of the PostgreSQL plan path.** `console_run` dispatches `DatabaseKind::SqlServer` to `showplan` before `explain::prepare`, which still refuses SQL Server. Explain sends `SET SHOWPLAN_XML ON`, the user batch, and `SET SHOWPLAN_XML OFF` as three batches, because SQL Server accepts that setting only as a whole batch. Explain Analyze uses `SET STATISTICS XML` and runs the batch. `results::plan_output` is the only writer of `ResultSet.plan_format` and `RunOutput::Plan.format`. `result_plan` parses JSON or showplan XML from that tag. A missing tag is `NotAPlan`. The cell is not sniffed. `StatementPlan.statement_text` is required and empty for PostgreSQL. The plan view draws it above each statement tree and not as a tree item. Rows view copy is the stored cell, so an analyzed batch drops its executed rows. Plain Explain admits the run without asking and does not clear held writes. Explain Analyze of a batch `tsql::rerun_refusal` rejects asks on every environment, and the digest covers the ON batch, the user batch, and the OFF batch. A safe batch uses `writes::check`, so a Production hold still asks. An OFF error retires the session. Cancel does not send OFF. When the client is still alive, cancel retires it so the next batch returns rows. History records the user batch.
- **Cancellation retires SQL Server sessions.** The task requests Attention within a short bound, then discards the client. A dropped metadata operation also cannot return a partly consumed client for reuse. The console reports a lost session and any lost open transaction. Committed earlier batches remain committed. A fully drained response updates transaction presence from TDS transaction descriptors. Observation never issues SQL that would overwrite `@@ROWCOUNT` or `@@ERROR`. An incomplete or unsupported protocol state retires the session instead of fabricating transaction state. The Commit button uses native `WHILE @@TRANCOUNT > 0 COMMIT` to finish every nesting level, while typed COMMIT keeps SQL Server semantics.
- **SQL Server messages, errors, and counts stream from the driver in server order.** The vendored Tiberius yields each INFO token, each ERROR token, and each DONE or DONEINPROC row count, and `native_query` turns them into `QueryEvent::Notice`, `QueryEvent::ServerError`, and `QueryEvent::Complete`. `PRINT 'a'; RAISERROR('boom', 16, 1); PRINT 'b'` logs a, boom, b, as SSMS shows it. Many errors, such as a `RAISERROR` at severity 11 to 16 or a constraint violation, do not end the batch, so the log shows every error the server sends, and the statement fails with the first. A conversion error, a compile error such as a missing table, a `THROW`, and, under `SET XACT_ABORT ON`, a run-time error other than `RAISERROR` end the batch, so the log stops at that error. The driver still ends its stream with that first error, and `native_query` drops it because the log already has it. An error inside a result set ends that set as Failed with the rows it had, and later sets in the batch still complete. A `PRINT`, or a `RAISERROR` at severity 10, which the server sends as class 0, is a NOTICE. Class 1 to 9 is a WARNING, so it turns the **Output** button amber. A count that closes a result set finishes its rows. Any other count is one `Command` line in the log, worded as SSMS words it: "5 rows affected". The driver drops the count of a `SELECT` with no result set, as SqlClient does, so `SET @x = 1` logs nothing. Back-to-back counts keep only the last in `StatementRun.outputs`, so a loop of 10,000 single-row inserts adds 10,000 log lines but one output. That last count is the statement's own, after the counts from its triggers. A batch that counts nothing logs "Commands completed successfully".
- **SQL Server uses native transaction control.** The engine refuses app-managed `TxnMode::Manual` at console configuration and execution boundaries. It never silently converts a Manual request to Auto-commit. Auto-commit executes T-SQL batches unchanged, including explicit `BEGIN TRANSACTION`, `COMMIT` and `ROLLBACK`; transaction buttons operate on the observed native state. A trigger can end and replace a transaction with the same nesting and committability, so final `@@TRANCOUNT` and `XACT_STATE()` cannot prove rollbackability. The pure Manual planner was removed. PostgreSQL keeps its existing automatic transaction wrapping. Unknown commit or undo outcomes use `ApplyError::OutcomeUnknown`, and the frontend asks the user to verify saved data before retrying.
- **SQL Server exact counts belong to the table tab.** `exact_row_count` requires its `TabId` and expected `DatabaseRef`. SQL Server validates the live console, canonical target, native mode, and exclusive session slot before taking the session and again after catalog or login awaits. It moves the session through `Console::take_session`, then returns its observed transaction state through `Console::edit` and `return_session`, preserving the finished run, result arenas, and history. Counts therefore see the page's native settings and uncommitted rows. `ConsoleState.work = Count` exposes progress without replacing the finished run, and enables the toolbar's Counting indication and Cancel/Esc action. Other session work stays noncancellable through those controls apart from ordinary runs. A small task owns that same Busy slot through completion or caller abandonment. Cancellation retains the driver's lease retirement contract, and close or disconnect orphans the count through the existing session-drop path. PostgreSQL retains metadata-session counts. Refusing session-dependent predicates would miss other session state; a separate Last-page execution request would broaden pagination and result contracts without changing this ownership requirement.
- **History and chat retain their original kind.** New history rows record the database kind, and the SQLite migration defaults existing rows to PostgreSQL. History grouping includes kind. Saved `SentContext.databaseKind` supplies historical SQL highlighting after a connection is deleted; its reader supplies the PostgreSQL fallback for older chats. SQL Server assistant schema tools and suggestions remain available, but model-written SQL is refused before approval and again at execution. Only PostgreSQL has the required fresh READ ONLY query path. ApplicationIntent is a routing hint and cannot substitute for enforced read-only execution.

- **Wire casing.** Struct fields and struct-variant fields are camelCase on the wire (`rename_all = "camelCase"` on structs, `rename_all_fields = "camelCase"` on tagged enums). Enum tags keep the Rust variant name (`"Connected"`, `"Manual"`), except `RunRequest` and `RunTarget`, whose tags are camelCase (`"script"`, `"tablePage"`, `"cursor"`).
- **`RunError::Connect { error: ConnectError }`** replaces `Connect(ConnectError)`. Both enums are internally tagged on `kind`, so the newtype form serializes two `kind` keys and the inner one wins in JavaScript. `model::tests::nested_connect_error_keeps_its_own_tag` guards this.
- **`DisconnectError::OpenTransactions { tabs: Vec<OpenTxn> }`**, where `OpenTxn { tabId, hasWrites }` replaces the `(TabId, bool)` tuple, so the frontend reads a named field instead of a positional boolean.
- **Error types the design named but did not define:**
  - `SaveError` is `Invalid | Keychain | Store`, each with a `message`.
  - `DisconnectError` also has `Store`, for `connection_delete`.
  - `NotConnected` is `{ connectionId }`.
  - `ConfigureError` is `TransactionOpen | Busy | UnknownConnection`.
  - `ExportError` is `Gone | Io { message } | NotRerunnable | Busy | TargetChanged | TransactionAborted | Connect { error } | Query { error } | ShapeChanged | Cancelled`. Every variant leaves the target file as it was.
  - `StoreError` is Rust-only, because only `Engine::open` returns it.
- **Command parameter names** (the camelCase invoke keys) are `draft`, `id`, `force`, `schema`, `table`, `tabId`, `config`, `request`, `changes`, `resultRef`, `start`, `len`, `row`, `col`, `format`, `scope`, `path`, `query`, `workspace`, `settings`, and `key`. `resultRef` replaces `ref` because `ref` is a Rust keyword.
- **`console_cancel` takes `AppHandle`, not `State`.** Tauri requires async commands that borrow `State` to return `Result`, and `console_cancel` returns `()`.
- **Pins.** tauri-specta `=2.0.0-rc.25` requires specta `=2.0.0-rc.25` and specta-typescript `=0.0.12`. Bindings use `dangerously_cast_bigints_to_number()`, so `u64` and `i64` (revs, run ids, row counts, epoch millis) export as `number`. All of them stay below 2^53.
- **Bindings regeneration.** Besides debug-build startup, `pnpm bindings` reruns `export_bindings_up_to_date` with `UPDATE_BINDINGS=1`, which rewrites the file without opening a window.
- **Dispatch coverage source.** The dev-bridge test parses the generated `bindings.ts`, not a hand-kept constant. The drift test already proves that file equals the Tauri registration. The test compares command names and camelCase argument names.
- **Dev-bridge protocol.** The route is `/invoke/{cmd}` (axum 0.8 path syntax). A command's own result is 200 `{status:"ok",data}` or 200 `{status:"error",error}`. An unknown command (404), bad argument (400), or engine panic (500) returns `{message}`, and the shim throws it as a real `Error`, so `typedError` rethrows instead of returning a typed error. The body is parsed as JSON whatever the content-type, so a bare `curl -d '{}'` works. `L1_DATA_DATA_DIR` overrides the bridge's temporary data directory.
- **Window creation.** The `main` window is declared in `tauri.conf.json` with `"create": false` and built in `setup`, so `L1_DATA_SELF_CHECK=1` runs without a window. The self-check prints `{"ok":false,...,"connect":{"status":"notImplemented"}}` and exits 1 until the engine phase adds the connect, `SELECT 1`, and catalog steps.
- **Window chrome.** On macOS, `main.rs` turns decorations back on with an overlay title bar and a hidden title. The native traffic lights sit over the sidebar header, and macOS handles resizing. On Windows and Linux, `tauri.conf.json` sets `decorations` to false, and the webview draws the resize edges and the minimize, maximize, and close buttons at the right of the toolbar. The toolbar and the sidebar header are drag regions on every platform. `hostedWindow()` returns `{ chrome: "native" }` on macOS, the custom controls elsewhere, and null in the dev-bridge browser, where no window controls show. The sidebar toggle leads whichever header touches the window's left edge. That is the sidebar header while the sidebar is open and the toolbar while it is hidden. `windowLeadingInset()` gives both headers the same left padding, so the toggle stays put. On macOS that padding clears the traffic lights. The "L1 Data" title follows the toggle on every platform. The window opens with the persisted theme, or no theme for System, so native chrome such as the traffic lights matches from the first frame. When that theme resolves to light, `main.rs` sets a white window background. `index.html` paints a `light-dark()` background for the same reason. `startTheme()` paints the document from the webview's `prefers-color-scheme` before the first render. After that, in the app window, each theme change awaits the window's `setTheme` before it repaints, because the webview reports the window's forced theme until then. Only the latest change repaints and then calls `setBackgroundColor`. In the browser, the repaint is immediate.
- **Engine additions.** `Engine::data_dir()` lets the self-check report and the bridge log show the directory in use. `KeyringKeyStore` and `MemoryKeyStore` are implemented, not stubbed, and sealed passwords are keyed by `ConnectionId`.
- **Panel sizes.** The sidebar and the right panel are plain columns on either side of the main area, with widths set in pixels from `layout.sidebarWidth` and `layout.rightPanelWidth`. They used to be react-resizable-panels panels, which kept each one as a share of the window, so their widths changed whenever the window resized. `PixelColumn` draws either column with its resize handle. The sidebar's handle sits on its right edge and the right panel's on its left. A drag updates the width live, clamps it (200 to 480 pixels for the sidebar, 260 to 560 for the right panel), and saves it once on release. ArrowLeft and ArrowRight on the focused handle move it 8 pixels and save each step. Dragging one column takes space from the main area only. The main area keeps at least 320 pixels. When the window is too narrow for both columns beside it, the right panel shrinks toward its minimum first and the sidebar after it, without changing the saved widths. A drag or arrow key starts from the drawn width, so a squeezed column follows the pointer with no jump. A vertical react-resizable-panels `Group` still holds the editor and the results, and saves the results height on release.
- **Defaults the design left open.** `Settings::default()` sets `stop_on_error: true` and `theme: System`. `Layout::default()` is sidebar and results shown, no right panel, `results_height: 320`, `sidebar_width: 256`, and `right_panel_width: 341`.
- **Fonts** come from `@fontsource-variable/inter` and `@fontsource-variable/jetbrains-mono`, the variable-axis builds of the Fontsource packages. They are the default interface and code fonts. `themes/fonts.ts` lists the other choices, which are system fonts. The picker offers one only when a canvas width probe finds it installed, because WKWebView has no `queryLocalFonts`.

Engine phase (every `todo!()` in `crates/engine` filled in, self-check wired). The contract did not change; `src/ipc/bindings.ts` is byte-identical.

- **`Deserialize` on `ConnectionConfig` and `HistoryOutcome`.** The store round-trips both as JSON. Rust-only derives, no wire effect.
- **`StmtKind` carries the command tag.** `Ddl(String)` and `Other(String)` hold the tag Postgres would print (`CREATE TABLE`, `SET`, `TRUNCATE TABLE`), because tokio-postgres reports only a row count. `NoTxnBlock(String)` (`VACUUM`, `CREATE DATABASE`, `... CONCURRENTLY`, `ALTER SYSTEM`) is excluded from the implicit `BEGIN`. `COPY ... TO STDOUT` is `CopyStdio` too, since the simple protocol cannot carry it either. `UNION`/`INTERSECT`/`EXCEPT` and a leading parenthesis classify as `Query`.
- **Console state split for sync readers.** `Console.results` (std mutex) and `Console.latest` (the last published `ConsoleState`) sit next to the tokio-mutexed `ConsoleInner`, so `result_page`, `result_cell`, and `boot()` never take the async lock. The design's `SessionSlot::InRun` is `SessionSlot::Busy { run_id, backend_pid, cancel, stop }`. `console_commit`, `console_rollback`, and `table_apply` move the session through the same slot, so a run during a commit reports `Busy`.
- **Connections keep their entry.** `ConnSlot::Disconnected` replaces removal on disconnect, so `rev` stays monotonic across reconnects. Only `connection_delete` removes the entry.
- **Schema changes apply at the next run.** The run task compares the session's `search_path` with the console config and issues `SET search_path TO "<schema>"` (the schema alone) before the first statement. One path serves both the busy and the idle case. `SET TimeZone` failures (an OS zone Postgres does not know) are ignored.
- **A FATAL answer is a dead session.** `pg_terminate_backend` delivers `FATAL 57P01` to the statement in flight before the driver task sees EOF, so the run task treats any FATAL as a loss and waits up to 500 ms for the driver's reason. `Session::closed_reason` also consults `Client::is_closed`.
- **Cancel between statements.** The next pending statement is marked `Cancelled`, the rest `Skipped`, outcome `Cancelled`. A statement that fails with `57014` while the stop flag is set is `Cancelled`; the same code without the flag (a `statement_timeout`) is `Failed`.
- **Catalog and database-list refresh obligations retain their original connection and transaction policy.** PostgreSQL publishes deferred invalidations for the original database only after successful commit, and discards them on rollback or retirement. SQL Server can commit DDL before opening another native transaction, so its pending invalidations settle after commit, rollback, session loss or close. Native batches record their possible invalidations before execution, so forced close cannot orphan the obligation. A busy close waits for client cleanup before scheduling metadata reloads. SELECT INTO records a catalog obligation. Opaque EXEC/EXECUTE and implicit procedure calls conservatively record both catalog and database-list obligations because their metadata effects cannot be read from the calling text. This includes native sp_rename and dynamic SQL. SQL Server catalog invalidation refreshes every currently tracked database slot on the original connection because three-part names and procedures can change another database. It uses each slot's existing refresh gate and opens no untracked databases. Ordinary read statements create no obligation; read-only procedure calls can incur extra refresh work. Refresh gates still coalesce bursts.
- **`has_writes`** is probed by taking the idle session out of the slot like a run does, so the console lock is never held across the query; an aborted transaction or a busy session reports `true`.
- **A run task owns the tab only while the slot says so.** Every write a task makes to console state goes through `Console::edit(run_id, ..)`, which applies and publishes the edit only while `SessionSlot::Busy { run_id }` matches. Disconnect and close reassign the slot and finish the run themselves (statement in flight `Cancelled`, the rest `Skipped`, outcome `Cancelled`, `txn` Idle), send a `CancelRequest`, and the task gets `Orphaned` at its next write, stops, and drops its session.
- **CSV** uses CRLF records with a header row, quotes fields containing `"`, `,`, CR, or LF, writes NULL as an empty field and the empty string as `""`, which is `COPY ... CSV HEADER` modulo line endings. The pg test compares after normalizing CRLF.
- **JSON, Markdown, and SQL `INSERT` exports type values by `TypeCategory` and never parse a number.** JSON is an array with one object per line. An `Integer` or `Numeric` cell is a bare number only when its text already matches the JSON number grammar, so `NaN` and PostgreSQL money stay strings; a `Bool` is `true` or `false`; `Json` text is embedded when it parses. A repeated column name becomes `name_2`, skipping names already taken. Markdown is a GitHub table with `NULL` as the grid shows it. SQL `INSERT` writes one statement per row into the single table every column comes from, named by the result's own catalog, or into `target_table`. Into a named table it leaves out the columns the server computes (PostgreSQL generated columns, SQL Server computed, `rowversion`, and period columns) and keeps identity values: PostgreSQL's `GENERATED ALWAYS` identity takes `OVERRIDING SYSTEM VALUE`, and a SQL Server identity wraps the file in `SET IDENTITY_INSERT ... ON` and `OFF`. The catalog's `writable` flag finds those columns, and the relation's detail tells an identity from a computed column. SQL Server literals convert through `mssql_value::input_sql` with an explicit style, so `DATEFORMAT` cannot change a datetime, and a file breaks into batches with `GO` every 1,000 statements.
- **PostgreSQL values are the output function's text**, so booleans are `t`/`f` as in psql. `bool::text` would say `true`; the type-zoo test's oracle is the simple protocol on an independent session, not `::text` casts.
- **`ServerInfo.version`** is the first token of `server_version` (`16.15` from `16.15 (Debian ...)`); the dev image is 16.15, not 16.4, so tests assert the `16.` prefix.
- **`engine::draft_from_url`** turns a `postgres://` URL into a `ConnectionDraft` for the self-check and the test harness.
- **Self-check report** is `{ ok, dataDir, report }` where `report` holds `server`, `select1` (rows and elapsed), and per-schema relation and function counts, or `{ step, error }` on failure with the typed error as JSON.
- **`dev/bridge-smoke.sh`** drives the dev-bridge with curl (save, connect, catalog, configure, run the mockup query, page) and asserts the SSE stream delivered the `Finished` snapshot with six rows.
- **Dev-bridge panic test** replaced: no command panics any more, so the test now checks that a typed error (`catalog_get` on an unknown id) comes back as a command error, not a bridge error.

Frontend phase (PRs #4 and #11 to #13). These PRs built the live store, the editor, the grid, table sort, filter, and edit, and the settings screen. Each item is a frontend decision the design above did not make.

- **A table tab is a query tab with a table binding.** `Tab.table` holds the schema, table, page, and `TableQuery`. Otherwise a table tab is a query tab. It has a title the user can rename, a connection menu, a transaction mode, Commit and Rollback, and saved editor text, and it accepts history and function inserts. `openTableTab` seeds its editor with `browseQuery(schema, table)`. `runTab` sends `RunRequest::TablePage` only while `isBrowsing(tab)`, which means the editor text still equals that query (trimmed, one trailing `;` allowed). Edited text runs as a script, and the results hide the pager, sort, filters, and editing. The first show of a table tab, `loadTableOnce`, loads only its page, so restored text that is no longer the browse query waits for **Run**. A browse in Manual mode runs inside the tab's transaction, and so does `table_apply`. `setTabTarget` refuses while the tab has staged edits, because the edits would then apply to the table in another database.
- **Consoles span the whole database.** The frontend always sends `schema: null`. `tabFromSnapshot` drops a schema saved on a tab by older versions. Completion qualifies names instead (`qualifiedTables`).
- **Paging.** A page change is saved on the tab only after the engine accepts the run. A page request that arrives during a run or SQL Server count waits in `queuedPages`, and the latest one wins. The queue also serializes pages while a count IPC request awaits its first ownership snapshot. Each page intent has a request identity, so it invalidates an older Last count before the saved page changes. Cancel clears queued intent before dispatching cancellation; close, disconnect and session loss discard it too. Changing the page size in `saveSettings` reloads every table tab at page 0. The Last button calls `exact_row_count` with its tab and expected database, stores the count as `exactRowCount`, and jumps to the last page. It discards a count reply if the tab closed or its target, table, query, editor text, page intent, or page size changed meanwhile.
- **Sort is text.** `TableQuery.orderBy` holds the `ORDER BY` text. A header click parses it with `parseOrderBy`, cycles the column with `nextSort`, and writes it back with `formatOrderBy`. Shift-click adds a key. `FilterBar` above the grid edits the `WHERE` text, the `ORDER BY` text, and the column filters directly. `ORDER BY` text that `parseOrderBy` can't read as plain columns stays as written, and a header click then replaces it.
- **Staged edits.** `useTableEdits` holds edits per tab, with each original key stored as named cells and rows keyed by the JSON of their column/value pairs. Catalog refreshes cannot relabel pending key values. A grid is editable only on a PostgreSQL browsing table tab whose relation is of kind `Table` and whose full primary key is among the result columns. Edits are locked while a submit is in flight. ⌘S, or the toolbar, calls `submitEdits`, which sends `table_apply` and reloads the page on success. On failure, `failedAt` maps the failing `ChangeRef` back to its row. Closing a tab with pending edits asks first.
- **Row caches are module-level.** `rowCacheFor` keys a `RowCache` by `ResultRef`, so a cache outlives the components that read it. The first cache for a new `runId` evicts the tab's older ones, and `closeTab` calls `dropRowCaches`.
- **Foreign keys are followed in the webview, through the table query.** `features/results/foreignKeys.ts` maps each result column to a table column by `ColumnMeta.source`. PostgreSQL sets it on any column read straight from a table. SQL Server sets it only on a table page, after `run_outputs` checks the page's shape against the catalog, so a SQL Server statement result offers no follow. It reads `relation_detail` for each relation with a foreign key column (`ColumnInfo.fk`) and offers a key only when all its columns are in the result. Each copy of a single-column key gets its own link. A multi-column key is refused when any column of its table appears twice, because a column source names a table, not one copy of it in a self-join. **Go to referenced row** calls `openTableTab(ref, query)` with one `Eq` filter per referenced column, so the engine still builds and quotes the SQL (decision 9). `openTableTab` with a query reuses the table's open tab only when that tab has no staged edits and still shows its browse text. Otherwise the query opens in a second tab of the same table.
- **The record view reads stored rows only.** `features/results/RecordView.tsx` takes the result's `RowCache` once, at open, and shows rows below `rowCount`, never staged inserts or edits. It keeps that cache, so when the engine no longer holds the result, after a re-run or a close, the next page or cell read returns `Gone` and the view says the result is no longer available. The arrows load one page at a time through `RowCache.ensure`, so the cache's 64-page limit holds. That paging can evict the grid's pages, so the cache notifies its readers when it evicts and the grid loads its window again on each cache change. The view reads a cut value's full text only for the row it has shown for 120 ms. A read that finishes after the view has left its row does not change the view, but `RowCache` still keeps its value. `RowCache` holds full values up to 4 Mi characters and drops the least recently read first.
- **Settings writes are serialized.** `saveSettings` updates the store first, so a second change builds on the first. Each `settings_set` waits for the previous one and sends its captured snapshot. Every access change reaches the engine, including a rapid switch back to the original mode. `sendMessage` waits for the queue to drain before admitting a turn and refuses to send if the last save failed.
- **Keys.** ⌘W closes the active tab. The macOS menu in `main.rs` mirrors Tauri's default menu with Close Window moved to ⇧⌘W, so ⌘W reaches the webview. Global shortcuts live in `shell/commands.ts`.
- **History is a grouped browser.** The sidebar's clock button opens the 960 by 640 History dialog. The left list selects a preview without changing any tab editor state. The preview owns a separate read-only CodeMirror with line numbers and the existing editor theme, SQL highlighting, and function styles. The right panel held only an Assistant placeholder until the Assistant phase below.
- **History grouping and metadata belong to Rust.** `history_browse` groups retained runs by exact SQL text, connection ID, and database. It returns bounded titles and excerpts, run counts, failure counts, and representative run metadata. A leading comment supplies the title when present. The group ID is its oldest retained run ID. Groups page by latest run ID, newest first, with at most 100 groups per response. The UI requests 50. All, Failed, and Writes counts cover groups matching the search before the selected filter. Any matching member admits its whole group. A failed filter admits groups with any error and represents each with its latest error. Other filters use the latest run. Cancelled runs stay separate from errors.
- **History details stay paged.** `history_group` returns the exact SQL once and up to 100 run records, newest first, with a maximum elapsed time over the whole group for duration bars. The UI requests 50 runs. A group timeline includes all retained runs, even when only one member matched the search.
- **History pages share a revision.** The store mutex protects the SQLite connection and an in-memory history revision. A successful insert with pruning, or clear, increments the revision while holding that lock. Later browse pages and group requests must carry the first page's revision. A mismatch returns `HistoryError::Stale`. React discards old search and detail responses, refetches on stale revisions or a `HistoryAppended` tail change, and preserves the selected group when it remains on the refreshed page. It does not aggregate the live tail into history groups.
- **Writes is descriptive history metadata.** Rust recognizes DML, including `RETURNING` and modifying CTEs, and DDL or `TRUNCATE`. It skips quoted text and comments with the splitter's lexical helpers. `EXPLAIN` counts a write only with enabled `ANALYZE` on a recognized write. This metadata does not change `classify` or transaction runner behavior, and does not infer function side effects.
- **History actions reuse tab behavior.** Enter opens a new query tab on the recorded connection and database. Cmd or Ctrl+Enter opens and runs it through `openSqlTab` and `runActive`, including the existing configure error for a removed connection. Alt+Enter inserts into the active tab, a table tab included, through `insertAtCursor`. Closing after an action restores editor focus instead of the dialog trigger. Cmd or Ctrl+C copies the whole query unless the search field has selected text. Clear uses the existing confirmation overlay and reopens fresh history after completion. `history_list` remains available for existing clients and smoke tests.
- **The right panel is only the assistant.** The design also had Schema and History panels, which were never built. `RightPanel` has only `Assistant`, and `Layout`'s deserializer opens the assistant for a workspace saved with either old panel.
- **Export in the dev-bridge** asks `dialogShim` for the path, which answers from the queue that `window.__l1Dialog.answer` fills. The results header has no Import button.
- **Catalogs are addressed by database (PR: address catalogs and consoles by database).** `Live` became `Instance` with a `DbSlot` per database; `ConnStatus::Connected.catalogVersion` and `AppEvent::CatalogChanged` are gone, and `InstanceState` in the connection snapshot is the only source of versions. `ConsoleConfig` and `ConsoleConfigView` carry a required `database`; the persisted `ConsoleConfigSnapshot.database` is `Option<String>` so older workspaces load, and `tabFromSnapshot` resolves a missing one to the connection's configured database. A tab whose connection no longer exists keeps its text at restore and loses its target and its table binding. When a saved connection's `database` is edited, query tabs on the old configured database move to the new one (`upsertConfig`); a tab bound to a table stays on the database the table lives in. `StmtKind::invalidates()` maps a statement to `Nothing`, `Catalog`, or `DatabaseList`; `CREATE`, `DROP`, and `ALTER DATABASE` re-read `pg_database`, and `CONCURRENTLY` index DDL refreshes the catalog. `NotConnected` became `CatalogError { NotConnected | UnknownDatabase | NotShown | Failed }`. The status bar shows `connection · database` for the active tab. Quick navigation shows each destination's connection, database, and schema context. The TopBar picker still lists connections and targets each one's configured database.
- **The tree's databases and schemas come from a stored view (PR: choose which databases and schemas the sidebar shows).** Migration 3 adds `connection_views(connection_id, json)`, one row per connection holding `ConnectionView { databases: Selection, showSystemDatabases, schemas: { <db>: { schemas: Selection, showSystemSchemas } } }` with `Selection = All | DefaultOnly | Only { names }`. The row is separate from `ConnectionConfig` so the dialog and the pickers never write the same row, and a row that fails to parse is logged and read as the initial view. No row, which is every pre-existing connection, resolves to the home database shown with all its schemas; any other database defaults to its default schema only. The engine is the single writer: the pickers send `ViewEdit` intents and `view.rs` resolves them as pure functions (`apply`, `resolve_databases`, `resolve_schemas`, `system_schemas_to_load`); a checkbox under `All` or `DefaultOnly` first materializes the current set as `Only`. `Only` stores names, so a dropped object stays in the set and stops counting. `InstanceState` carries `showSystemDatabases` and, per database, `shown`, `showSystemSchemas`, the resolved `shownSchemas`, and a `SchemaCount`; the tree draws and filters only `shownSchemas`. The catalog lists every schema with `system` and `tableCount`; `SCHEMA_FILTER` became "not system, or named in the shown system schemas" for relations and columns and "not system" for functions, and `pg_toast` and temp schemas never appear. `connection_probe_databases` counts schemas for databases without a slot through short-lived sessions, cached until `pg_database` is re-read (by the connection menu's Refresh or by database DDL) or the connection drops; a probe that finishes after the re-read is discarded. Showing a database whose catalog is unknown registers a deferred check that resets an empty stored selection to the default schema on the first load; a schema edit or a hide of that database withdraws it, and a disconnect makes it a no-op. The connection menu's "Refresh catalog" calls `connection_refresh`.
- **The output log is a toggle in the results pane.** #11 removed the `OutputLog` pane. The log came back as `OutputPanel`, which takes the results body behind an **Output** button in the results header, so it adds no chrome. The log is tab-wide: every run, commit, rollback, and `table_apply` on the tab, oldest first, with a hairline where the `runId` changes. Each PostgreSQL statement logs a `Statement` line, its notices, and then a `Rows`, `Command`, or `Error` line. A SQL Server batch logs one `Statement` line and then its notices, errors, and counts in the order the server sends them, so it can log several `Rows`, `Command`, and `Error` lines, interleaved with its notices. `table_apply` logs the SQL it built for each change and its affected-row tag. Every `OutputEntry` has a `runId`, and commit, rollback, and `table_apply` send the run id they take from the shared counter. `resultView` in `state/status.ts` picks the body: an Output pick made on the current run, then the row grid, then the log when the tab has entries, then the old tail. The tail is still needed because `output` is not in `BootSnapshot`, so after a restart the log is empty while the run state is restored. The pick `{ kind: "output", runId }` in `activeResult` lapses on the next run. The button turns amber only while the latest run in the log raised a WARNING. A run, and the rerun of an All rows export, send their log lines in batches (`OutputBatch` in `console.rs`): when the server sends nothing for 5 ms, and at least every 100 ms. One event per line overran the 1,024-event broadcast buffer, which dropped most of a 5,000-line `PRINT` loop. Sending on every wait for the server still put one line in each event for a `RAISERROR ... WITH NOWAIT` loop, which waits after each message. The 5 ms pause still shows a NOWAIT progress message at once. `output[tabId]` is an `OutputLog`: the last 5,000 entries and a count of the entries it dropped. `OutputPanel` keys each line by its position in the whole log, renders lines in memoized groups of 100, and takes a new batch through `useDeferredValue`. A batch then renders only the lines it adds, and React renders them in short slices, so a 20,000-message flood does not block the window. Rendering every line on each batch blocked it for up to a second per batch. `output[tabId]` is never freed when a tab closes, which holds up to 5,000 entries per closed tab.
- **There is no status bar.** The transaction mode menu is in the TopBar on every tab. It also carries the lost-transaction notice, "rolled back" in red with a tooltip. Ln/Col is in the results header on every tab. The connection, database, server version, and UTF-8 chips are gone, so the status bar no longer shows `connection · database` as the database-addressing bullet says. The `results` palette token is gone too, so the results pane paints `background`. A saved custom theme that still has a `results` color loads, because `customVariant` in `themes/catalog.ts` and the importers keep only keys that `isToken` accepts.
- **Search filters in place (PR: filter the sidebar tree and settings in place).** The tree and Settings search boxes narrow their own content independently of quick navigation. The tree filter searches the shown schemas of the shown databases of connected instances, and matches the names of tables, views, columns, and functions by substring, not fuzzily. A match shows under its connection, database, and schema rows. `filterTree` in `features/sidebar/treeFilter.ts` builds the narrowed tree as one typed value, and `ConnectionTree` draws it with its own rows instead of the normal nodes, which carry the pickers and the loading and error rows. `ConnectionTree` holds which connections have had their first-connect reveal, so the normal nodes that remount when the query clears do not reopen a home database the user closed. Enter opens the first listed table or view whose name has every word at a word start, else the first listed, so "ord it" opens `order_items` rather than `order_audit`.
- **Quick navigation uses existing destinations.** Cmd or Ctrl+P opens a Radix palette from the workspace or Settings without changing either filter or revealing the sidebar. `features/navigation/navigationItems.ts` derives a `Destination` union of tab, database, and relation from workspace tabs, connection snapshots, and loaded catalog views. Accessible discovered databases are included regardless of sidebar visibility. Relations cover loaded catalogs only, as the palette explains. Case-insensitive searches match every word, rank exact names and name prefixes before contextual matches, and then bound the display to 60 results with a total count. Tab activation calls `setActive`; relations call `openTableTab`. A database activation prefers the active SQL tab on that exact connection and database, then another matching SQL tab, and otherwise calls `openSqlTab` with the chosen database. It never retargets an existing session and rejects stale database destinations that would resolve elsewhere. The palette closes before activation, restores opener focus on cancellation, and focuses the destination after activation. There is no new IPC, catalog cache, SQL construction, or persisted state.
- **The connection dialog is a connection manager (PR: redesign the connection dialog).** The dialog lists the saved connections beside the form, and choosing one reopens the `connection` overlay with that id. The form is dirty when its fields, password edit, or URL text differ from what the selected connection opens with (`isDirty` in `connectionForm.ts`), and it reports that to the dialog. While the form is dirty, choosing another connection, **New**, **Close**, the X, Esc, or a backdrop click opens a nested "Discard unsaved changes?" prompt instead, and the dialog runs the stored action only on **Discard**. A successful save closes with no prompt. The prompt is a nested Radix dialog, not the workspace `ask` overlay, because the overlay is a single slot and opening it would close the connection dialog. A new connection's URL field calls `connection_parse_url` on each edit and fills the fields from the draft; a response older than the latest edit is ignored, and a parse error shows only after a blur or once the text has `://` or `=`. An empty name saves as `host / database`. `ConnectionConfig.color` holds an id from `CONNECTION_COLORS` in `connectionColors.tsx` (`red`, `amber`, `green`, `blue`, `purple`), which maps each id to a fixed hue; any other stored value renders as no color. The tree's connection row and the TopBar's connection name show the color as a dot.

Assistant phase (the chat panel, **Settings** > **Providers**, and the engine behind them). Decision 10 and the assistant types above are the design. Each item is a place the code differs from or adds to it.

- **Access starts in General.** Settings opens on a new first section, **General**, with the global `assistantAccess` choice. The composer names the current mode and links to General. In schema only mode it hides the query-permission picker and paperclip, and an access change clears an attachment selection. The engine separately enforces the lease and stored provenance described in decision 10. Generations are UUIDs saved in kv `assistant_access`, rather than a counter the frontend could restore.

- **One process per turn, not a warm process.** A `claude` process took 0.45 to 0.5 s from spawn to its `initialize` response over five runs. That cost did not pay for a warm-process slot, its reaper, and its fingerprint, so each turn starts its own process and resumes the session with `--resume`.
- **The process driver is `provider/agent.rs`.** The design put it at `assistant/agent.rs`. The Full probe's no-turn handshake drives a process too, and `provider` importing from `assistant` would make the two modules depend on each other.
- **`application_name` is set after connect.** `Session::open_assistant` calls `Session::open`, then runs `SET application_name = 'L1 Data assistant'` and `SET default_transaction_read_only = on`, instead of passing the name as a connect parameter.
- **Stop fails a running tool.** A tool call that Stop cancels mid-run ends `ToolStatus::Failed { message: "Stopped by the user." }`, which is the text the model receives. `Expired` is for a call still open when the turn ends, and for one a crash left open.
- **Revs on open.** Stored rows carry no rev. `assistant_open` serves them at the chat's own rev, which every event for them preceded, and keeps the running turn's snapshot revs, so the live store's keep-higher rule still takes the newest of each and two opens of an unchanged chat return the same view. `boot` lists each stored chat that is not loaded yet with `rev: 0`, so that chat's first event replaces the listed state. `ChatInner.live` holds only the running turn's snapshots; `finish` clears it.
- **The agent PATH is an `EngineConfig` field.** `EngineConfig.agent_path` is `AgentPath::LoginShell` in the app and the dev-bridge. It reads the login shell's PATH once and appends the usual install directories, because a Finder-launched app gets launchd's PATH, which misses `claude`. Tests pass `AgentPath::Fixed` with an empty PATH, so `cargo test` never starts a login shell or the installed `claude`.
- **Launch-argument `--model` and `--effort` set defaults.** Typed into a provider's launch arguments, they choose the model and effort a new chat starts with. The model matches by alias or by resolved id, so `--model claude-opus-5-5` selects Opus 5.5. The engine does not pass them through, because each chat's pickers set its model and effort, and the field does not list them as ignored.
- **The resync check runs from boot.** `startAssistantSync` in `boot.ts` reopens an idle chat whose `ChatState.entryCount` is more than the entries the panel holds. A missed `ChatEntryChanged` therefore heals while the panel is closed.
- **Unvetted SQL opens without running.** A SQL card whose block is not `vetted` offers **Open in new tab** (`sql-open-new-tab`) in place of **Run in new tab**. A tool row's **Open in tab** opens its query without running it. A statement the model wrote runs only when the user presses Run.
- **The CLI waits on `can_use_tool`.** The design capped tool execution at 45 s in case a 60 s MCP call timer also covered the wait for approval. The `claude-smoke` test approves after 75 s and the query still runs, so the CLI does not time out a waiting `can_use_tool`. The 45 s cap on execution stays.
- **Invoke keys.** The assistant commands add `chatId`, `options`, `approval`, and `decision` to the command parameter names.
- **Markdown.** `ReplyText` renders replies with `react-markdown` and `remark-gfm`. It skips raw HTML and draws links and images as text. `SqlCard` highlights SQL with `@lezer/highlight` and the editor's token table, with no `EditorView` per card.
- **Vetted calls, not a denylist.** The design's `side_effect_calls` named functions to refuse, and a `U&"…"` identifier, `query_to_xml`, or an unlisted function slipped past it. `sql::vet` in `sql/relations.rs` replaces it and fails closed, as decision 10 now says. `VETTED` lists the built-ins an analytics query calls, each stable or immutable, plus six volatile size helpers that only read file sizes. Whether `name(` is a call or syntax comes from the token before it and from the server's keyword table in `sql/keywords.rs`, which `dev/pg-keywords.sh` regenerates and a pg-test checks: reserved and column-name keywords are never functions, an identifier before the name makes it a clause such as `JOIN` or `LIKE`, and `ROLLUP` and `CUBE` are syntax only inside `GROUP BY`. The tokenizer emits a `UnicodeEscape` token for `U&`. `VETTED_OPERATORS` lists the operator symbols the same way, each a `pg_catalog` operator with no volatile implementation, which a pg-test checks: the tokenizer emits an `Operator` token for each run of operator characters, lexed as Postgres lexes it (a comment start ends the run, a trailing `+` or `-` leaves a run with none of `~ ! @ # % ^ & | ` ?`, `!=` reads as `<>`, and `=>` is not an operator), `vet` judges the symbol like a call, bare or as `OPERATOR(schema.op)`, and the `*` after a dot is a column list. `1 ## 2` asks and names `operator ##`. `reply::vetted` uses the same verdict, so a card that calls an unvetted function offers **Open in new tab**. Three shapes ask although they read: `group by a, rollup (b)`, a `LIKE` whose left operand is a column named by an unreserved keyword such as `name`, and `GROUPING SETS` after a comma.
- **One input per approval.** `can_use_tool` can carry other arguments than the `tool_use` block that created the card. `ensure_tool` refreshes the card from the ask, the ledger records that input, and the allow reply echoes it as `updatedInput`. `on_gate` in `assistant/turn.rs` serves both consults, denies at once once `stop_deadline` is set, and `settle` answers either site from a `Refusal` the ledger records (policy text, declined, stopped, cancelled, expired, mismatch), so a refused `tools/call` hears the policy's own message rather than "not approved". A `tools/call` whose input differs from a pending ask's is refused at once: the card and the allow reply keep the asked input, and the ask stays answerable. `ToolCancelled` also denies a pending approval for its call.
- **The next turn waits for the last process, and a kill sweep at quit.** `finish` publishes the End and Idle as soon as the outcome is known and hands the process to a close task; `ChatInner.closing` keeps a handle that resolves once the child is reaped. The next turn's task waits for it before it starts its process, bounded by the close's 2 s grace plus 1 s, and `assistant_delete` and `shutdown` wait for it too. A chat that stayed Running until the exit showed Stop and refused a send for the second the real CLI takes to exit after stdin closes. `Shared.process_groups` registers every live agent process group at spawn and drops it when the child is reaped; `Agent::drop` signals only a group still registered, so a reaped pid is never signalled. `Engine::shutdown` kills every registered group on its own thread after its wait, as decision 10 says. After its grace, `close` also kills the child itself through the handle it holds, and on Windows, which has no process groups, the sweep ends each registered process by pid, so a CLI that ignores interrupt and EOF cannot outlive a turn or quit there either. `Agent::next` remembers the end of stream, so a cancelled poll loses neither it nor the exit code. A supervisor task in `assistant/supervisor.rs` awaits each turn task and gives a turn that panicked or was aborted its End through its private `end_abruptly`, which closes open rows as `recover` does; nothing else can reach it.
- **Delete marks the chat first.** `assistant_delete` sets `ChatStatus::Deleted` under the chat's lock before anything else; `store_entry`, `store_record`, `publish_entry`, `publish_state`, and the supervisor's `end_abruptly` all take the lock through `Chat::unless_deleted`, which returns nothing for a deleted chat, so a turn task past the wait cannot write the row back. The one tombstone is emitted after the rows are gone, also for a chat this session never loaded.
- **Only the CLI's own lost-session signal restarts a chat.** `Provider::session_lost` names it (for Claude, "No conversation found" in the `result` frame or on stderr). The turn clears the session and retries once as a new session with a recap only when a resumed start fails that way, or when the first `result` of a resumed turn says so before any text or tool use. Any other failure keeps the session and ends the turn `Failed`. The Notice is written once the replacement process has started, and the recap leaves out the turn being sent. `StartError::Reported` carries an error the CLI answered with, so a sign-in failure at start ends `SignedOut`.
- **New chats start in Ask first.** `default_options` never copies a permission. It takes the newest chat's model and effort only when the last Ready probe lists them and the launch arguments name neither; else the recommended model with the launch arguments' effort when listed, else `high`, else the last level; with no Ready probe, the provider's fallback.
- **The engine lists the ignored flags.** `ProviderState.ignored_flags` is the engine's own tokenization of the saved launch arguments, so the settings page shows what the engine drops, and the TypeScript tokenizer is gone. `ProviderInfo.owned_flags` stays on the wire. `ENGINE_FLAGS` also owns `--plugin-dir`, `--agents`, `--append-system-prompt-file`, and `--no-session-persistence`. Passthrough arguments go before the engine's flags in argv, so when the CLI reads a flag from both, the engine's wins. The child env also drops `ANTHROPIC_AUTH_TOKEN`.
- **Paths and directories.** A relative binary path is made absolute against the engine's own working directory, because the process starts in the assistant directory. A missing assistant directory is reported as such, not as a missing binary, since both fail `spawn` with the same error.
- **The cursor statement has its own budget.** The context block cuts the statement at the cursor at 4,000 chars, separately from the tab SQL's 8,000.

Usage popup (opened from the sidebar's bottom bar). Usage is an `Overlay` variant and has no Settings entry. `Overlays` keeps `UsageDialog` mounted, and its `Modal` mounts the limits body only while open. Decision 10's sentence on limits and the usage types above are the design. Each item is a place the code differs from or adds to the rest of this doc.

- **Limits are keyed by `ProviderKind`.** The report holds one `SourceLimits` per `ProviderKind::ALL`, named by the provider's saved display name. Claude's read uses the Claude provider's saved config and `Provider::launch`, so the binary path, config directory, launch arguments, and env scrub match a chat.
- **A one-shot `Exchange`, not a `Codec`.** `Codec` is a chat session's contract, with a `Ready` handshake and turns. A limits read is a script: write `open()`, then feed each stdout line to `read` until it returns an output. `agent::exchange` runs it on the same `Pipes` that `Agent` holds, so the cwd check, `process_group(0)`, the group registration, the line cap, and the kill on drop are one copy. It closes the child on a background task, so the reply does not wait for the process to exit. Claude's `get_usage` needs no `initialize`, and the engine test checks that the fake received only the `get_usage` line.
- **Pull, not a pushed snapshot.** Nothing in the engine changes between reads, so there is no `rev` and no `live.ts` slice. `useLimits.ts` holds the last report in a module store, so reopening the popup shows it at once while the next read runs, and it starts no read while one is in flight. The popup reads on open, on Refresh, and once when the earliest reset after the last read passes.
- **Signed out or API key comes from `auth status`.** A signed-out `claude` answers `get_usage` with `rate_limits: null`, the same as an API-key login. When a read gets no rate limits, it runs `claude auth status` itself: the read is `SignedOut` when that says signed out, and `NoLimits` otherwise. It does not use the provider's cached probe, which a switched-off provider never refreshes. A switched-off Claude provider is still read, since the limits belong to the account.
- **Only `limits[]` is read.** Claude's reply also carries a changing set of codenamed keys beside `rate_limits.limits`. The parser reads `subscription_type` and that list only, and the fake keeps a few codename keys so the parser's tests show it ignores them.

Formatter (`editor_format`, `⌘⌥L`, and **Format**). Decision 6 and decision 9 are the design. Each item is a place the code adds to it.

- **A lossless lexer under the old tokenizer.** `sql::lex` returns lexemes that tile the input, whitespace and comments included, so text can be rebuilt from them. `tokens()`, which `vet` and `history_is_write` read, is a projection of `lex()` with its old output: a string's `E`, `B`, `X`, `N`, or `U&` prefix is a word of its own, `::` is two `Other` tokens, and a number splits at its dots, signs, and letters. A number lexeme keeps any word glued to it. Postgres 16 reads `0x1F`, `0o17`, and `0b1010` as numbers and rejects the rest, such as `1abc`, as trailing junk, so the formatter must not space them apart. A string lexeme includes every part that continues it across a newline, so `'a'` and `'b'` on two lines are one lexeme, with any `--` comment between them. `split_statements` still scans bytes on its own, with the quote, string, and comment skips it shares with `lex`.
- **The formatter refuses what it cannot prove.** `sql::format_statement` lays out queries, DML, and `CREATE TABLE` with a stack of clause frames, and keeps every other statement's whitespace, changing only keyword case. Before it returns, it lexes its output and compares the significant lexemes with the input's: whitespace is dropped, unquoted words compare case-insensitively, and everything else compares byte for byte, comments included. A mismatch returns `FormatError::Unsafe`, and the editor keeps the text and shows a toast. The check trusts `lex` to read the text as Postgres's `scan.l` does. Where `lex` reads a literal differently, the check compares the wrong lexemes and can pass a layout that changes the literal's value. The whitespace between the parts of a continued string is inside the one string lexeme, so the check keeps it byte for byte. The formatter has no session, and a tab may `SET standard_conforming_strings = off`, where a backslash escapes in a plain `'…'` or `N'…'` literal. So the check runs twice, once for each reading of the setting, and a layout must pass both. `'a\'b'` is refused, because the two readings end the literal in different places, while `'C:\path'` reads the same either way and still formats. The pg-test `formatting_keeps_the_value_of_every_awkward_literal` selects a list of awkward literals before and after formatting and compares the server's text output, so it fails when `lex` drifts from `scan.l` on one of them. `formatting_keeps_the_value_of_a_backslash_literal_with_standard_strings_off` does the same with the setting off.
- **Keyword case follows the server's categories.** `dev/pg-keywords.sh` also writes `RESERVED` and `TYPE_OR_FUNCTION_NAME`, and the keyword pg-test checks them. Those keywords are uppercased unless they sit next to a `.` or after `AS`. Unreserved and column-name keywords are uppercased only inside a phrase `format.rs` lists, such as `ORDER BY` or `ROWS BETWEEN`, or where a clause opens, such as `SET` after `UPDATE`. A column named `day`, `name`, or `type` keeps its case, and so do type and function names.
- **A selection formats the statements it touches.** A selection run runs exactly the selected text, but `editor_format` widens a selection to every statement it overlaps, so a partial selection never formats a fragment. A cursor and `All` pick as a run does.
- **One edit, one undo step.** The frontend applies the `FormatEdit` as one CodeMirror transaction with `isolateHistory`, so `⌘Z` restores the text as it was even right after typing, and moves the cursor to the end of the edit. A reply for a document that changed meanwhile is dropped, and a browsing table tab is not formatted, since its text is the engine's browse query.

Files (`file_open`, `file_save`, `⌘O`, `⌘S`, `⌘⇧S`). The design had no file tabs; "Still open" asked whether tabs needed them. Each item is a place the code adds to the design.

- **The engine owns the bytes, and nothing about tabs.** `crates/engine/src/files.rs` holds the only file IO. `decode` strips a leading BOM, checks UTF-8, takes `crlf` from the first line break, and turns every `\r\n` and lone `\r` into `\n`, which is CodeMirror's own split, so offsets stay LF-based and the lexers never see a U+FEFF glued to the first word. `encode` puts the BOM and CRLF back. A `FileStamp` is `"<len>:<sha256>"` of the bytes, so a `touch` or an identical rewrite is not a change. The engine keeps no file state: the frontend stores each `FileBinding` whole and hands it back.
- **An overwrite names the version it overwrites.** A `Bound` save compares the disk's stamp with the binding's where it writes. On a mismatch it answers `Changed { disk }` and writes nothing. The UI asks, then sends the same target with `overwrite: disk`, and a newer outside change answers `Changed` again. This is the approval ledger's rule applied to a file. A `Changed` the user does not overwrite sets the tab's baseline unknown and its health `changed`, so the tab reads unsaved and closing it asks. Before the stamp check, a read-only file is refused with `access(W_OK)`, never by opening it for write, so the atomic rename never bypasses `chmod 444`. A folder the user cannot write is found only when the temp file is made. The write goes to a hidden temp file in the target's folder, created with the target's mode, is fsynced, and is renamed over the target. The new inode keeps only the permission bits: owner and group, ACLs, extended attributes such as Finder tags, and other hard links stay with the old one. A crash between the temp file's creation and the rename can leave `.<name>.*.l1data-tmp` in the folder. A symlink is written through to its target.
- **One file, one tab.** A binding's path is the canonical folder joined with the file's own name. On Windows it drops the `\\?\` verbatim prefix that `fs::canonicalize` adds to drive and UNC paths, unless the plain form would name another file, as a name ending in `.` or a space, or a device name such as `CON`, would. Open switches to a tab with the same path. Save As sends the other tabs' paths in `taken`, and `file_save` refuses with `OpenInAnotherTab` where it writes when the target is the same file as one of them. On Unix that is the same device and inode when both files exist, and the same path otherwise. On Windows it is the same path, compared without case. So `/tmp` and `/private/tmp`, a symlinked folder, a symlink, or a hard link cannot bind two tabs to one file through Save As.
- **The binding is saved in `TabSnapshot.file`.** Text and binding live in one blob, so a crash cannot leave them disagreeing. The previous Stable ignores the field, so a downgrade keeps each file tab's title and text and loses only the binding; the next `⌘S` asks where to save.
- **Dirty is derived next to the doc.** `editorStates.ts` keeps each file tab's baseline beside its doc. Every write to a doc goes through `commit()`, which tells `onBuffersChanged` listeners about background tabs too. The tab bar reads `isDirty` through `useSyncExternalStore`, and persistence schedules its save on the same signal, so a background reload or a save's new baseline is written without waiting for an edit. Typing during a save stays dirty, because the baseline is the doc that was sent.
- **A save flushes the workspace before it writes the file.** `saveTab` captures the doc once, awaits `flushWorkspaceSave()`, and then calls `file_save` with that doc, again on an overwrite retry. The stored buffer is then never older than the file. A crash between the file write and the next autosave restores a tab whose text equals the disk, and boot calls it clean. Saves of one tab run in order, and a save stops before writing once its tab is gone or bound elsewhere.
- **Boot reconciles each bound file once.** `reconcileFiles` reads every bound file and applies `judgeFile`, a pure table: the disk holds the tab's text, so adopt the disk binding, clean; the stamp matches, so the disk text is the baseline; it changed and the tab was clean, so reload as one undoable edit and toast; it changed and the tab was dirty, so keep the buffer and the old stamp, and the next save asks; missing or unreadable, so the baseline is unknown and the tab reads dirty. A verdict applies only if the tab's doc and binding are the ones its read started with. The app does not watch files while it runs.
- **One prompt mechanism.** `ask()` in `workspace.ts` replaces the `confirm` overlay. It resolves with the chosen id, or `null` when anything else replaces or clears the question, and the caller's continuation runs after the answering click has closed the dialog, so `closeTab` asks its next question straight from the continuation. `confirm()` wraps it and keeps `confirm-dialog` and `confirm-btn`. `closeTab` asks about a dirty file first (Save, Don't Save, Cancel), then staged edits, then the open transaction.
- **A focused control owns its chord.** The window keydown handler skips an event with `defaultPrevented`, so a chord a focused control handles never also runs a global command. The grid leaves `⌘S` and `⌘⇧S` to the window. With staged edits in the active tab, the global `⌘S` submits them wherever focus is, and `⌘⇧S` is always Save As. The in-cell editor stops every keydown, so that the grid never acts on a key typed into the cell, and runs a global shortcut itself after it commits the cell. `⌘O` and the saves do nothing while a dialog is up, so they never cancel a question. On macOS the global commands match `⌘` only, because CodeMirror owns the `Ctrl` chords there.
- **A window close flushes first.** The webview's close-requested listener makes Tauri hold the close, writes the pending save and waits for file saves under way for at most 2 s, then destroys the window, which needs `core:window:allow-destroy`. The shell destroys the window 5 s after every close request, which does nothing to a window the webview already destroyed. On macOS, **Quit L1 Data** (`⌘Q`) is a custom menu item that closes every window and exits the app 5 s later, because the predefined item terminates with no close event. Dock Quit, logout, and SIGTERM still terminate directly, since tao exposes no hook before `applicationWillTerminate`. They lose at most the last 500 ms of typing, as before.
- **Saved queries are a column table, not a JSON blob (PR: add saved queries).** `saved_queries` holds one typed row per query, so a stable build that opens a file a nightly migrated reads only the columns it names. A column a later migration adds to this table must have a `DEFAULT`, because that stable build still inserts rows. A row this build cannot read, such as one whose `database_kind` a newer build wrote, is logged and left out of the list. No statement touches it, since updates and deletes go by id. `saved.rs` owns every rule. A save takes the selection as selected, or what a `Cursor` run would pick through `text::pick`, with the statement's `;`, so an inserted query does not run into the next statement. Saving the same text in the same dialect returns the existing entry and writes nothing. An in-memory revision starts at 1 and rises only when a write changes a row, which `Connection::total_changes` decides inside the store lock, and the list is read under that lock. Each `saved_query_*` command returns the whole list, and only a write that changed a row emits `SavedQueriesChanged`. `live.ts` merges the boot snapshot, the event, and each reply by keeping the higher `rev`, as `hydrateChat` does for chats. That reply path writes `live.ts` outside an event, so the palette can open on a just-saved id without waiting for the event.
- **The Saved queries palette.** The sidebar's bookmark button and `⌘E` open the `saved` overlay, and `⌘⇧E` saves from the active tab. `entry: name` opens the new entry's name for editing, and finishing or leaving that edit closes the palette. The list is the snapshot in `live.ts`, filtered in TypeScript by `rankSaved` with no round trip. The preview side is `paneReducer` over `view`, `edit`, `confirmDiscard`, `confirmDelete`, and `done`, so Esc and a backdrop click leave an edit or a confirmation before they close the palette. Copy calls `navigator.clipboard.writeText` before any IPC and records the use after it. `HistorySql` became `features/editor/SqlText.tsx` with a read-only `view` mode, which history uses, and an `edit` mode with undo and dialect highlighting and no completion.
- **The dev-bridge answers file dialogs.** `src/dev/dialogShim.ts` answers `plugin:dialog|open` and `plugin:dialog|save` in the page from answers queued on `window.__l1Dialog`, so the dialog calls never reach the bridge and `ROUTES` stays equal to the Tauri command list. An empty queue answers Cancel.

Environments and the run target (the Production write check, the connection dialog's **Environment**, and the top bar). The design had neither. Each item is a place the code adds to it.

- **An environment is a label the user sets.** `ConnectionConfig.environment` is an `Option<Environment>`, one of `Local`, `Staging`, or `Production`, and `None` means not set. The UI labels the stored `Local` value **Develop**. `ConnectionDraft` carries it, and `connection_save` stores it as sent. The hand-written reader defaults a missing field to `None`, so a connection saved before the field existed loads. The previous stable build reads a row this build wrote, because its reader skips fields it does not know, so the field needs no other fallback. Only the Production write check reads the label.
- **The engine checks a Production write where the run starts.** `console_run` passes every statement it is about to send to `writes::check_run`, and `table_apply` to `writes::check`, under the console lock, after the session slot is filled and just before the session moves to the task. For `table_apply` that includes the `BEGIN` or `SAVEPOINT` and the `COMMIT` or `RELEASE` around the edits. The check judges the session that will run the work, not the connection's instance. Each tab session keeps the `ConnectionConfig` it was opened from in its `SessionSlot`, because a save changes neither the server that session reached nor the environment it was opened in, and the instance can reconnect from a newer save while the tab keeps its session. The check asks when the connection as saved now is Production or the tab's session was opened while it was Production. A session opened on Production keeps asking after the connection is relabelled, until it closes, for example on **Disconnect**. When any statement writes by `text::is_write` or a fence proved it a write, or the tab holds unasked writes, the check takes a SHA-256 of the saved config, the config the session opened from, the console's connection, database, schema, and mode, the tab's transaction state, the unasked writes the tab holds, and the statements with each one's write bit. It runs the action only when the request's `confirmed` equals that digest. Otherwise it returns `ConfirmWrites` with the connection name, the database, the unit (statement or batch), the writes the action sends and the unasked writes the transaction holds, each as a count and the first three statements cut at 2,000 characters, and the digest. It sends nothing, so nothing is recorded in history. A new session the refused call opened stays in the slot for the retry. The frontend shows its question from that error and sends the same request again with the digest. A change to any of those inputs makes the digest stale, and the engine asks again. A caller that skips the question cannot write to Production, because the engine never sees a matching digest. The digest is not secret: it stops a caller that skips the question, not one that computes the digest on purpose. Every run in the app goes through `runTab`: `⌘↵`, **Run**, the statement chooser, History's run, and the assistant's **Run in new tab**. A table page load asks only while the transaction holds unasked writes, because the engine builds that `SELECT` itself. The assistant's own queries go through `run_query`, which is read-only. PostgreSQL `COPY ... FROM` counts as a write, including a server-side file or `PROGRAM`. `COPY ... TO` of a table or a read-only query does not count. A parenthesized COPY query uses the existing write classifier, so recognized `INSERT`, `UPDATE`, `DELETE`, and data-modifying CTEs count. That classifier still misses data-modifying CTEs whose names use Unicode escapes, tracked in [#75](https://github.com/ImNallen/L1Data/issues/75). Comments, quoted names, strings, and nested column or query parentheses cannot supply the COPY direction. The check reads SQL text, so it misses a function with side effects called from a `SELECT`, and on PostgreSQL it misses `SELECT ... INTO`. On SQL Server that miss writes. On PostgreSQL the run fails closed, as the next item says. An export of all rows re-runs its query on the tab's session without this check.
- **Any run can commit writes that ran unasked, so every run asks for them.** `ConsoleInner.unasked` counts the writes that no Production question covered, because the session was not on Production when they ran, and keeps the first three. The run task adds each write after it runs, and `table_apply` adds its edits once they apply. The list empties only at an end of the transaction that the engine sees, as `txn::ended` decides: a `COMMIT`, `ROLLBACK`, `END`, `ABORT`, `COMMIT PREPARED`, or `ROLLBACK PREPARED` that succeeds, a `COMMIT AND CHAIN` or `ROLLBACK AND CHAIN`, which ends the transaction that held the writes and opens a new one, a SQL Server batch after which the server reports no transaction, the `COMMIT` or `ROLLBACK` of a staged apply outside an open transaction, or the end of the session. Every way a session leaves the tab, whether lost, closed, or dropped when `console_configure` moves the tab to another connection or database, goes through `ConsoleInner::end_session`, which empties the list with the transaction state. An Idle state that the engine infers from statement kinds never empties the list. A statement the classifier misreads, such as a `BEGIN` behind a comment it ends too early, leaves the server's transaction open while the engine shows Idle, and a later `COMMIT` would commit the writes unasked. PostgreSQL reports no transaction state to the engine, so its auto-commit writes stay listed too. After a relabel to Production, the next run on that tab asks once for writes that may have committed already, so the question words each sentence to hold either way and in either mode: if the writes are still in the tab's transaction, confirming covers them, so a later commit does not ask about them again. **Commit on Production?** names no count, because the held writes may have committed already. A `ROLLBACK TO SAVEPOINT` keeps the list as it was, so the question can list a write the savepoint undid. That errs on the side of asking. The classifier reads its words through the lexer that the statement splitter uses, so it ends every comment and quote where PostgreSQL ends it. On Production, while the list is not empty, `writes::check` asks before every `console_run`, `console_commit`, and `table_apply`, whatever the text, because a batch such as `IF @@TRANCOUNT > 0 COMMIT`, `PRINT 'x'; COMMIT`, or `PREPARE TRANSACTION` can commit or hand off the writes, and spelling cannot find every form. The question lists the held writes beside the action's own. Confirming it empties the list, so the next run does not ask again for the same writes. **Commit** after a confirmed run in Manual mode does not ask. **Rollback** never asks. `PREPARE TRANSACTION`, `COMMIT PREPARED`, and `ROLLBACK PREPARED` count as writes.
- **`Work::Run`, `Work::Edit`, `Work::Import`, and `Work::Commit` cannot be built without a check.** `writes::check` and `writes::check_import` return `WritesChecked` on success. Only `writes.rs` builds one. `Work::Run`, `Work::Edit`, `Work::Import`, and `Work::Commit` each carry a `WritesChecked`, and `Console::take_session` is the only way to take a session out of `SessionSlot::Idle`, whose `IdleSession` fields are private to `console.rs`. A transaction command sends `COMMIT` only for `Work::Commit`. It reads that from the variant, so `Work::Rollback` sends `ROLLBACK`. Other work sends `COMMIT` too, each under its own check: a typed `COMMIT` runs as `Work::Run`, and an auto-commit `table_apply` ends with its own `COMMIT` as `Work::Edit`. Taking the `WritesChecked` also settles the unasked writes. The type does not prove that the check read the statements the work sends. A check over other statements, a second check whose result replaces the first, a run handed over as `Work::Count`, and a transaction command that picks `COMMIT` from its own flag rather than the variant all compile. Which `Work` kind the work takes, and that the check covers the statements it sends, are conventions, and the tests in `writes.rs` and `tests/pg.rs` enforce them. Forging a `WritesChecked` or taking the session out of an `IdleSession` does not compile. Four kinds carry no `WritesChecked`. `Work::Rollback` sends only `ROLLBACK`, which discards the transaction and writes nothing. `Work::Count` and `Work::Probe` run SQL the engine builds. `Work::Export` re-runs the result's query unchecked, which is a known gap.
- **A Production PostgreSQL read runs read-only.** `console_run` calls `writes::check_run`, which also returns a `ReadOnly` bit per statement. A bit is set on Production PostgreSQL for a statement that is not a write and that `sql::classify` calls `Query`, or the `COPY ... TO` form. DML, DDL, transaction control, `NoTxnBlock`, and other verbs such as `SET`, `LOCK`, `DECLARE`, and `LISTEN` run bare, because the fence's close would undo their session effects. `run_task` opens a fence before a covered statement: `BEGIN READ ONLY` when the tab is Idle, and `SAVEPOINT l1_read; SET LOCAL transaction_read_only = on` when it is Active or Aborted. After the statement it closes the fence even after an error or a cancel. A Txn fence ends with `COMMIT` on success, so a `set_config` persists as auto-commit would, and `ROLLBACK` on failure. A savepoint fence always ends with `ROLLBACK TO SAVEPOINT l1_read; RELEASE SAVEPOINT l1_read`, because read-only cannot be switched off after a read and a `RELEASE` would leave the user's transaction read-only. So a fenced read never ends, begins, or aborts the user's transaction, and its `txn` is the state it entered with. A fence that fails to open fails the statement unsent, with the same transaction effect as an unfenced error. A close that fails retires the session, as an export does. A lost connection or a FATAL error skips the close. When a fenced statement fails with SQLSTATE 25006, the run adds its trimmed text to `ConsoleInner.proven`, a `writes::Proven` of the newest 32 distinct texts, and logs that the statement tried to write. `proven` lives as long as the console, not the session. `check_run` counts a proven text as a write, so the next run of it asks and runs unfenced once confirmed. The digest covers each statement's write bit, so a digest issued before a statement was proven no longer matches. The savepoint fence also rolls back what a read takes in the transaction, such as a `set_config(..., false)` or an advisory transaction lock, and `SELECT ... FOR UPDATE` fails as a write.
- **The PostgreSQL write check counts opaque execution.** `CALL`, `DO`, `EXECUTE`, `REFRESH`, `VACUUM`, and `CLUSTER` count as writes, as `EXEC` already does on SQL Server. History marks new rows with these verbs as writes. Rows saved before keep the mark they had.
- **An answer holds only for the target it named.** If the tab's connection, database, or mode changes while the question is open, the frontend drops the run. A change it does not see, such as the environment set in another window, makes the engine ask again.
- **The top bar names the run target.** It reads the connection, then database. The tab bar already names the tab, so the top bar leaves the title out. The connection and database share the target width. Below 520px of toolbar width, their target gets a full row above the actions. That threshold rises to 760px while transaction controls, a lost-session notice, or Counting need more room. Actions wrap within their row. The database menu lists the databases `InstanceState` marks shown, plus the tab's own, which is the only entry while the connection is down. The split Run button is red for Production, yellow for Staging, and blue otherwise. Its icon, shortcut, and Explain trigger share that palette. Environment icons and the Production underline are absent from the top bar. Container queries on the bar give the run target room. Below 900px **Format** keeps only its icon, and below 520px it hides. `⌘⌥L` still formats. The connection tree draws a monitor for Develop, an amber flask for Staging, or a red shield for Production after the connection name, outside its truncation. An open transaction is amber in the mode menu and on the tab's dot, and an aborted one is danger.

CSV import. PostgreSQL table menus, including filtered tree results, open the desktop CSV picker and capture the concrete table tab returned by `openTableTab`. Existing Manual mode stays intact. Staged grid edits prevent setup. A header maps only unambiguous exact names; without a header, writable columns map by position. The dialog lets the user override or skip each source field. Only bounded preview cells cross IPC.

- `table_import_prepare(tab_id, target, file)` installs one tab-owned Preparing draft and returns a backend-generated `ImportSourceId` before disk or network work. `Shared::spawn` starts preparation. The blocking worker opens with `O_NONBLOCK` on Unix, checks descriptor metadata for a regular file, copies to a private temporary file, hashes its bytes, and validates every record. `table_import_preview(tab_id, source, header)` waits for Ready or Failed. Header changes reuse that snapshot. `table_import_discard` affects only the matching draft. Dismissal before prepare returns discards the handle as soon as it arrives. Replacement, reconfiguration, close, and disconnect drop the draft and stop its file worker. An in-flight connection or catalog operation settles before the stopped preparation exits; dropping those futures can strand shared connection state. A replaced worker can briefly overlap the next worker while it unwinds; there is no global registry or strict one-file disk bound.
- The incremental parser retains quote provenance and supports a UTF-8 BOM, LF and CRLF records, quoted commas, doubled quotes, embedded CR and LF, and a last record without a newline. Unquoted empty is NULL; quoted empty is empty text. Invalid UTF-8, NUL, malformed quoting, and ragged records fail with line and record information. Each source record is limited to 8 MiB and 1,024 fields. Preview holds at most ten data records, 64 KiB of sample text and 32 KiB of header text, with each displayed value capped at 256 bytes. Display shortening does not change imported values. A producer uses the same parser and quotes every non-NULL value in canonical CSV. It skips unmapped fields. COPY omits unmapped target columns so PostgreSQL supplies defaults. Generated and server-numbered identity columns are excluded.
- `table_import` validates the source, mapping, current catalog relation identity and shape, saved connection, and session's server before final admission. The catalog lookup awaits outside the console lock. Under that lock, `writes::check_import` checks the saved and opened Production labels and binds parser version, source ID and hash, byte and row counts, header, mapping, database/schema/table/OID, eligible columns, and exact COPY and transaction work. It explicitly counts COPY as a write. The generic typed-SQL COPY classification gap remains separate in [issue 69](https://github.com/ImNallen/L1Data/issues/69). After BEGIN or SAVEPOINT, the task locks the qualified table in ROW EXCLUSIVE mode and verifies its OID on the tab session before COPY. The lock and identity query are part of the checked statements. Import SQL is ASCII on the wire: non-ASCII identifiers use PostgreSQL Unicode escapes, and the identity literal escapes Unicode code points. This preserves the selected object and columns under a changed client encoding without changing the session setting. COPY declares UTF8 separately for data. A replacement table with the same name is refused even if the cached catalog is stale. Other schema changes still pass through server validation and atomic cleanup.
- Admission consumes the draft once and takes `Work::Import { checked, progress }`. Progress derives only from Busy ownership. The blocking producer feeds four bounded chunks, normally 256 KiB each, and carries source-byte checkpoints. Bytes mean source bytes represented by submitted COPY chunks, not parser read-ahead. A large record can exceed the normal chunk size but stays within the record limit. The status remains Finishing until the transaction fence completes. `session/postgres.rs` owns the COPY protocol and its native types, drains notices, and drops COPY before rollback. Every outcome closes the channel and joins the producer.
- Idle AutoCommit uses BEGIN/COPY/COMMIT. Idle Manual uses BEGIN/COPY and leaves Active. Existing Active uses SAVEPOINT/COPY/RELEASE, with ROLLBACK TO and RELEASE on failure or cancellation, preserving earlier writes. Aborted and unexpected Open states are refused. Stop is checked before and during COPY and before finalization. Cancellation while awaiting the table lock is reported as cancellation after rollback. A successful COMMIT wins late cancellation. A normal server rejection at COMMIT, such as a deferred constraint violation, leaves a rolled-back import and reusable session. Ambiguous commit or unverified cleanup reports OutcomeUnknown and retires the session. A dropped IPC waiter does not stop the cleanup task. Console edits and session return require the original run ID, so an orphan cannot overwrite a new run. Pending successful imports off Production enter `unasked`; savepoint outcomes preserve prior entries. Only actual full transaction boundaries, confirmed settlement, and session end clear them. Output records final rows and committed or pending status. The frontend refreshes only the captured table while it still has the same session and no staged edits.

Query parameters. `script_params(text, target, databaseKind)` is a pure synchronous lexical command. It selects the same original statements or GO batches as a run and returns unique names in first-use order. PostgreSQL accepts named `:name` and positive positional `$n` markers. Casts, array slices, quoted text, identifiers, and comments keep their meaning. Parameter occurrences must agree under both PostgreSQL string modes when the source contains backslashes. SQL Server excludes `@@` globals, variables declared in the selected batch, explicit and implicit procedure argument labels, and procedure, function, trigger, or rule definition batches. External parameter identity is exact-case because discovery cannot know the server's collation. Declaration exclusions ignore case to preserve existing scripts on case-insensitive servers. On a case-sensitive server, a differently cased reference to a declared variable remains SQL for the server to reject.

- PostgreSQL positional references inside `PREPARE` and `CREATE [OR REPLACE] FUNCTION` or `PROCEDURE` belong to the server and stay unchanged. Named parameters in those statements still accept client values. A separate ordinary statement in the same script can use an external `$1` value.
- `RunRequest::Script.params` carries names and `string | null` values. `console_run` rediscovers parameters and rejects missing, duplicate, unexpected, and NUL values. `params::ExecutableScript` materializes executable SQL once, keeps the original UTF-16 ranges separately, and never splits substituted SQL again. PostgreSQL parameter literals use `E'...'` with both quotes and backslashes escaped, independent of `standard_conforming_strings`. SQL Server uses Unicode `N'...'` literals. Classification, transaction planning, Explain, Production digests, history, and export origins all consume that executable text. Server error positions are omitted for substituted units, whose lengths can differ, while unchanged units retain original document positions.
- The editor captures one immutable run intent before discovery and the parameter dialog. Tab, document, database, mode, screen, or competing overlay changes cancel pending prompts. A sequence identifies each prompt, and supersession settles the previous promise. The same request and value snapshot goes through a Production confirmation retry. Accepted values remain only in memory per tab and target. Canonical table browsing does not discover parameters.
- The earlier plan requested PostgreSQL prepare-derived type labels from the synchronous command. Discovery has no connection or session and performs no network work, so it supplies no type labels. Explicit casts guide parameter types, and authorized execution still uses the native PostgreSQL prepare path. Discovery never borrows a tab session or adds unchecked work.

## Open questions and risks

Answered since the design:

- **Databases in the tree.** A connection is an instance. `InstanceState` lists every database of the server, and the tree draws the ones the stored `ConnectionView` resolves as `shown`, each with its own catalog. The connection row's `shown/total` pill opens a picker whose edits the engine applies and persists; a database added there shows its default schema. `Catalog.db` names the database.
- **Files on disk.** Tabs open and save `.sql` files. See "Files" under "Implementation reconciliation".
- **System schemas.** The catalog lists `pg_catalog`, `information_schema`, and other `pg_` schemas with `system: true` but loads their relations and types only once the user turns on "Show system schemas" in the schema picker and checks them; functions, sequences, and synonyms never load for them. `pg_toast` and temp schemas never appear. Turning the toggle off hides a checked system schema without forgetting it.
- **Timestamps.** The grid shows PG's text output verbatim, seconds included, and the export is verbatim too.
- **`COPY` over stdio.** `CopyStdio` covers `COPY ... TO STDOUT` as well as `FROM STDIN`, and `console_run` rejects both with `RunError::Unsupported`.

Still open:

- Are `max_rows = 200,000` and `max_result_bytes = 512 MiB` acceptable defaults? The settings screen does not expose `maxResultBytes`. A truncated read can export all rows, which runs it again on the tab's session and streams to the file.
- **Risk:** tauri-specta is still a release candidate (`=2.0.0-rc.25`, and docs.rs failed to build it). Pin exact versions of `tauri-specta`, `specta`, and `specta-typescript`. The fallback is `ts-rs` for types plus a handwritten `commands.ts` checked by the drift test.
- **Risk:** catalog size on very large databases. If the snapshot exceeds about 5 MB, switch columns to lazy per relation behind `catalog_get`. The `Relation.columns` shape stays the same.
- **Risk:** the dev-bridge runs in Chromium, not WKWebView. Rendering differences are covered only by `pnpm screenshot` until the `@wdio/tauri-service` follow-up lands.
- **Risk:** `prepare` fails for statements the extended protocol can't Describe. The classifier rejects `CopyStdio` up front with `RunError::Unsupported`. Other failures surface as a normal `QueryError`.
- **Risk:** table pages use `LIMIT n OFFSET m`. Postgres reads and discards every skipped row, so deep pages of a large table get slower.
- **Risk:** the pager's total is the catalog's `reltuples` estimate (`of ~241,183`) until the user presses Last, which fetches the exact count. A filtered table shows only a lower bound (`of 500+`) until then.
- **Risk:** the assistant speaks the stream-json control protocol that the Claude Agent SDK uses with the CLI: `initialize`, `can_use_tool`, the SDK MCP messages, and `interrupt`. It is not a documented public API, so a CLI release can change it. The codec tests over the lines captured from `claude` 2.1.301 pin the shapes the engine depends on, and the `claude-smoke` test checks them against the installed CLI.
- Assistant queries are not recorded in query history or in a tab's output log. They show only in the chat.
- Deleting a chat leaves the CLI's own transcript file under the config directory.
- On Windows the engine kills only the CLI process, not its children. On Unix the CLI runs in its own process group and the engine kills the group. To do the same, Windows would need a Job object.
- The dev-bridge seals passwords under an in-memory key, so after a bridge restart a connection needs its password again. Until the user reconnects, the assistant's context reads `Not connected: password missing`. The desktop app keeps the key in the keychain.

## Next implementation step

The engine and the frontend are built and proven against Docker (`cargo test -p engine --features pg-tests`, `pnpm test`, `pnpm self-check`, `dev/bridge-smoke.sh`). Remaining work, in no set order:

- End-to-end tests in the real WKWebView with `@wdio/tauri-service`.
