#!/usr/bin/env python3
import argparse
import copy
import csv
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import tempfile
import time
from urllib.parse import unquote, urlparse
from urllib.request import Request, urlopen
import uuid


class Bridge:
    def __init__(self, url):
        self.url = url.rstrip("/")

    def response(self, command, **args):
        request = Request(
            f"{self.url}/invoke/{command}", json.dumps(args).encode(),
            {"content-type": "application/json"},
        )
        with urlopen(request, timeout=20) as response:
            return json.load(response)

    def invoke(self, command, **args):
        response = self.response(command, **args)
        assert response["status"] == "ok", (command, response)
        return response["data"]

    def wait(self, label, read, accept, timeout=15):
        deadline = time.monotonic() + timeout
        last = None
        while time.monotonic() < deadline:
            last = read()
            if accept(last):
                return last
            time.sleep(0.05)
        raise AssertionError((f"timed out waiting for {label}", last))

    def console(self, tab):
        return next(c for c in self.invoke("app_boot")["consoles"] if c["tabId"] == tab)

    def configure(self, connection, database, mode="AutoCommit", tab=None):
        tab = tab or str(uuid.uuid4())
        self.invoke("console_configure", tabId=tab, config={
            "connectionId": connection, "database": database, "schema": None, "mode": mode,
        })
        return tab

    def start(self, tab, text, target=None):
        return self.invoke("console_run", tabId=tab, request={
            "kind": "script", "text": text, "target": target or {"kind": "all"}, "mode": "Run", "params": [],
        })["runId"]

    def finished(self, tab, run_id):
        return self.wait("run completion", lambda: self.console(tab), lambda c:
            c["run"]["kind"] == "Finished" and c["run"]["runId"] == run_id)

    def run(self, tab, text, target=None, outcome="Ok"):
        state = self.finished(tab, self.start(tab, text, target))
        assert state["run"]["outcome"] == outcome, state
        return state

    def results(self, state):
        return [o["result"] for s in state["run"]["statements"]
                for o in s["outputs"] if o["kind"] == "Rows"]

    def page(self, result, length=100):
        return self.invoke("result_page", resultRef=result["ref"], start=0, len=length)

    def rows(self, state):
        return [self.page(result)["rows"] for result in self.results(state)]


def draft(url, kind, name):
    parsed = urlparse(url)
    assert parsed.hostname and parsed.username, "test URL must include a host and user"
    return {
        "id": None, "name": name, "databaseKind": kind,
        "host": parsed.hostname, "port": parsed.port or (1433 if kind == "SqlServer" else 5432),
        "database": unquote(parsed.path.lstrip("/")), "user": unquote(parsed.username),
        "password": {"Set": unquote(parsed.password)} if parsed.password else "Clear",
        "sslMode": "Require" if kind == "SqlServer" else "Prefer",
        "caCertPath": None, "color": None,
    }


def check_assistant(bridge, db, tab):
    binary = Path("target/debug/fake-claude").resolve()
    assert binary.is_file(), "build fake-claude with cargo build -p engine before this smoke"
    saved = bridge.invoke("app_boot")["settings"]
    settings = copy.deepcopy(saved)
    settings["assistantAccess"] = "Full"
    provider = next(p for p in settings["providers"] if p["provider"] == "Claude")
    provider.update(binaryPath=str(binary), launchArgs="--fake-scenario query --fake-sql 'SELECT 999 AS assistant_probe'")
    chats = []
    try:
        bridge.invoke("settings_set", settings=settings)
        bridge.wait("fake provider probe", lambda: bridge.invoke("app_boot")["providers"], lambda providers:
            any(m["id"] == "fake" for p in providers for m in (p["outcome"] or {}).get("models", [])))
        for permission in ("Auto", "AskFirst"):
            chat = str(uuid.uuid4())
            chats.append(chat)
            view = bridge.invoke("assistant_open", chatId=chat)
            options = {**view["state"]["options"], "provider": "Claude", "model": "fake", "effort": None, "permission": permission}
            bridge.invoke("assistant_configure", chatId=chat, options=options)
            bridge.invoke("assistant_send", chatId=chat, request={"text": "Read a value", "tab": {
                "tabId": tab, "title": "SQL Server smoke", "db": db, "text": "SELECT 1",
                "target": {"kind": "all"}, "attach": None,
            }})
            view = bridge.wait("SQL Server assistant refusal", lambda: bridge.invoke("assistant_open", chatId=chat),
                lambda v: v["state"]["status"]["kind"] == "Idle" and any(e["entry"]["kind"] == "End" for e in v["entries"]))
            entries = [e["entry"] for e in view["entries"]]
            tool = next(e for e in entries if e["kind"] == "Tool" and e["tool"]["kind"] == "RunQuery")
            assert tool["status"]["kind"] == "Failed" and "SQL Server model queries are disabled" in tool["status"]["message"], tool
            user = next(e for e in entries if e["kind"] == "User")
            assert user["context"]["databaseKind"] == "SqlServer", user
        print("PASS SQL Server assistant refusal before approval in both permission modes")
    finally:
        for chat in chats:
            bridge.invoke("assistant_delete", chatId=chat)
        bridge.invoke("settings_set", settings=saved)


def check(bridge, work, assistant=True):
    pg = bridge.invoke("connection_save", draft=draft(
        os.environ.get("L1_DATA_TEST_PG", "postgres://l1data:l1data@localhost:5433/shop"),
        "Postgres", "smoke-postgres"))
    ms_draft = draft(os.environ.get("L1_DATA_TEST_MSSQL", "mssql://sa:L1Data_dev_Str0ng@localhost:1434/shop"),
                     "SqlServer", "smoke-sqlserver")
    bridge.invoke("connection_test", draft=ms_draft)
    ms = bridge.invoke("connection_save", draft=ms_draft)
    assert ms["databaseKind"] == "SqlServer"
    for connection in (pg, ms):
        bridge.invoke("connection_connect", id=connection["id"])
    db = {"connectionId": ms["id"], "database": ms["database"]}
    pg_tab = bridge.configure(pg["id"], pg["database"])
    tab = bridge.configure(ms["id"], ms["database"])
    observer = bridge.configure(ms["id"], ms["database"])
    table = f"smoke_{uuid.uuid4().hex}"
    qualified = f"[lab].[{table}]"

    def catalog_with_table():
        response = bridge.response("catalog_get", db=db)
        if response["status"] != "ok":
            return None
        catalog = response["data"]
        return next((r for s in catalog["schemas"] if s["name"] == "lab"
                     for r in s["relations"] if r["name"] == table), None)

    def refused_apply(**changes):
        before = bridge.console(tab)
        response = bridge.response("table_apply", tabId=tab, changes={
            "schema": "lab", "table": table, "inserts": [], "updates": [], "deletes": [], **changes,
        })
        assert response["status"] == "error" and response["error"]["kind"] == "Unsupported", response
        assert bridge.console(tab) == before

    def refused_manual():
        before = bridge.console(tab)
        response = bridge.response("console_configure", tabId=tab, config={
            "connectionId": ms["id"], "database": ms["database"], "schema": None, "mode": "Manual",
        })
        assert response["status"] == "error" and response["error"]["kind"] == "Unsupported", response
        assert "Select Auto-commit" in response["error"]["what"], response
        assert bridge.console(tab) == before

    cell = lambda column, value: {"column": column, "value": value}
    query = {"where": "", "orderBy": "", "filters": []}
    try:
        assert bridge.rows(bridge.run(pg_tab, "SELECT 17::integer AS n")) == [[["17"]]]
        batches = "DECLARE @n int = 7; SELECT @n AS value; SELECT N'世界' AS value;\nGO\nSELECT 9 AS value"
        state = bridge.run(tab, batches)
        assert len(state["run"]["statements"]) == 2, state
        assert bridge.rows(state) == [[["7"]], [["世界"]], [["9"]]], state
        assert all(r["databaseKind"] == "SqlServer" for r in bridge.results(state))
        assert len({r["ref"]["index"] for r in bridge.results(state)}) == 3
        assert bridge.rows(bridge.run(tab, "SELECT DATALENGTH(0x) AS empty_binary_length")) == [[["0"]]]
        selected = "SELECT 10 AS skipped;\nSELECT 11 AS picked;\nGO\nSELECT 12 AS skipped"
        start = selected.index("SELECT 11")
        end = selected.index("\nGO")
        assert bridge.rows(bridge.run(tab, selected, {"kind": "selection", "from": start, "to": end})) == [[["11"]]]
        outline = bridge.invoke("script_outline", text=batches, cursor=5, databaseKind="SqlServer")
        assert outline["unitKind"] == "Batch" and len(outline["statements"]) == 2, outline
        bridge.run(tab, "CREATE TABLE #l1_scope (n int); INSERT INTO #l1_scope VALUES (23);\nGO\nSELECT n FROM #l1_scope")
        assert bridge.rows(bridge.run(tab, "SELECT n FROM #l1_scope")) == [[["23"]]]
        print("PASS mixed engines, batch scope, exact selections and multiple results")

        bridge.run(tab, f"CREATE TABLE {qualified} ([id] int PRIMARY KEY, [value] nvarchar(100), [amount] decimal(38,12), [cash] money)")
        bridge.invoke("catalog_refresh", db=db)
        relation = bridge.wait("new table catalog", catalog_with_table, lambda r: r is not None)
        assert relation["qualifiedName"] == qualified, relation
        detail = bridge.invoke("relation_detail", db=db, oid=relation["oid"])
        assert any(k["kind"]["kind"] == "Primary" for k in detail["keys"]), detail
        precise = "12345678901234567890.123456789012"
        cash = "922337203685477.5807"
        original = "quote ' and 世界 %_[x]"
        quoted = original.replace("'", "''")
        bridge.run(tab, f"INSERT INTO {qualified} ([id],[value],[amount],[cash]) VALUES (1,N'{quoted}',{precise},{cash})")
        refused_manual()
        refused_apply()
        refused_apply(inserts=[[cell("id", "3"), cell("value", "x" * 101)]])
        assert bridge.rows(bridge.run(observer, f"SELECT COUNT_BIG(*) FROM {qualified} WHERE id = 3")) == [[["0"]]]
        bridge.run(tab, f"CREATE TRIGGER [lab].[trg_{table}] ON {qualified} AFTER UPDATE AS BEGIN SET NOCOUNT ON; SELECT 99 AS trigger_result; END")
        refused_apply(updates=[{"key": [cell("id", "1")], "set": [cell("value", "must not save")]}])
        assert bridge.rows(bridge.run(observer, f"SELECT [value] FROM {qualified} WHERE id=1")) == [[[original]]]
        state = bridge.run(tab, f"UPDATE {qualified} SET [value]=N'{quoted} changed' WHERE id=1")
        assert bridge.rows(state) == [[["99"]]]
        assert bridge.rows(bridge.run(observer, f"SELECT [value], [amount] FROM {qualified} WHERE id = 1")) == [[[original + " changed", precise]]]
        refused_apply(deletes=[[cell("id", "1")]])
        filtered = {**query, "filters": [{"column": "value", "op": "Contains", "value": "%_[x]"}]}
        assert bridge.invoke("exact_row_count", tabId=tab, db=db, schema="lab", table=table, query=filtered) == 1
        accepted = bridge.invoke("console_run", tabId=tab, request={"kind": "tablePage", "schema": "lab", "table": table, "page": 0, "query": filtered})
        assert bridge.rows(bridge.finished(tab, accepted["runId"])) == [[["1", original + " changed", precise, cash]]]
        unsafe = {**query, "where": f"1=1 DELETE FROM {qualified}"}
        refused = bridge.response("console_run", tabId=tab, request={"kind": "tablePage", "schema": "lab", "table": table, "page": 0, "query": unsafe})
        assert refused["status"] == "error", refused
        assert bridge.rows(bridge.run(observer, f"SELECT COUNT_BIG(*) FROM {qualified}")) == [[["1"]]]
        sort = bridge.invoke("table_order", db=db, schema="lab", table="awkward names", orderBy="", edit={"kind": "Cycle", "column": "a]b", "additive": False})
        assert sort["sql"] == "[a]]b] ASC" and sort["keys"] == [{"column": "a]b", "desc": False}], sort
        print("PASS catalog, native writes and trigger results, capability refusals, filters and injection rejection")

        accepted = bridge.invoke("console_run", tabId=tab, request={"kind": "tablePage", "schema": "lab", "table": "type_zoo", "page": 0, "query": query})
        state = bridge.finished(tab, accepted["runId"])
        assert state["run"]["outcome"] == "Ok", state
        result = bridge.results(state)[0]
        names = [c["name"] for c in result["columns"]]
        page = bridge.page(result)
        values = dict(zip(names, page["rows"][0]))
        assert values["precise"] == precise and values["big"] == "9223372036854775807", values
        assert values["money_value"] == "12.3456" and values["bytes"].lower() == "0x0001feff", values
        assert values["null_value"] is None, values
        long_col = names.index("text_value")
        assert [0, long_col] in page["cut"], page["cut"]
        full = bridge.invoke("result_cell", resultRef=result["ref"], row=0, col=long_col)
        assert full == "😀" * 3000
        exported = work / "types.csv"
        bridge.invoke("result_export", resultRef=result["ref"], format="Csv", scope="Loaded", path=str(exported))
        with exported.open(newline="") as stream:
            exported_row = next(csv.DictReader(stream))
        assert exported_row["precise"] == precise and exported_row["text_value"] == full
        raw = bridge.run(tab, "SELECT CAST(922337203685477.5807 AS money), CAST(5 AS sql_variant), hierarchyid::Parse('/1/')")
        assert bridge.rows(raw)[0][0] == ["922337203685477.5807", "5", "0x58"], raw
        print("PASS precise text, raw money, sql_variant, CLR, long-cell retrieval and CSV export")

        bridge.run(tab, f"SET XACT_ABORT ON; BEGIN TRANSACTION; INSERT INTO {qualified} ([id],[value]) VALUES(2,N'rollback')")
        assert bridge.console(tab)["txn"] == "Open"
        refused_manual()
        refused_apply(inserts=[[cell("id", "1"), cell("value", "duplicate")]])
        assert bridge.rows(bridge.run(tab, f"SELECT [value],@@TRANCOUNT,XACT_STATE() FROM {qualified} WHERE id=2")) == [[["rollback", "1", "1"]]]
        bridge.invoke("console_rollback", tabId=tab)
        assert bridge.rows(bridge.run(observer, f"SELECT COUNT_BIG(*) FROM {qualified} WHERE id = 2")) == [[["0"]]]
        bridge.run(tab, f"BEGIN TRANSACTION; INSERT INTO {qualified} ([id],[value]) VALUES(2,N'commit')")
        bridge.invoke("console_commit", tabId=tab)
        assert bridge.rows(bridge.run(observer, f"SELECT [value] FROM {qualified} WHERE id = 2")) == [[["commit"]]]
        bridge.run(tab, "BEGIN TRANSACTION; BEGIN TRANSACTION;")
        assert bridge.console(tab)["txn"] == "Open" and bridge.console(tab)["txnDepth"] is None
        bridge.invoke("console_commit", tabId=tab)
        assert bridge.console(tab)["txn"] == "Idle"
        backend_pid = bridge.console(tab)["session"]["backendPid"]
        run_id = bridge.start(tab, "WAITFOR DELAY '00:00:30'; SELECT 1")
        bridge.wait("WAITFOR on the server", lambda: bridge.rows(bridge.run(observer,
            f"SELECT COUNT_BIG(*) FROM sys.dm_exec_requests WHERE session_id = {backend_pid} AND command = 'WAITFOR'")),
            lambda rows: rows == [[["1"]]])
        bridge.invoke("console_cancel", tabId=tab)
        canceled = bridge.finished(tab, run_id)
        assert canceled["run"]["outcome"] == "Cancelled" and canceled["session"]["kind"] == "Lost", canceled
        bridge.wait("server request termination", lambda: bridge.rows(bridge.run(observer,
            f"SELECT COUNT_BIG(*) FROM sys.dm_exec_requests WHERE session_id = {backend_pid}")),
            lambda rows: rows == [[["0"]]])
        assert bridge.rows(bridge.run(tab, "SELECT 29")) == [[["29"]]]
        assert bridge.rows(bridge.run(tab, "SELECT OBJECT_ID(N'tempdb..#l1_scope')")) == [[[None]]]
        print("PASS native rollback/commit, preserved outer work after refusal, nested transactions and cancellation retirement")

        if assistant:
            check_assistant(bridge, db, tab)
        history = bridge.invoke("history_list", query={"text": None, "connectionId": ms["id"], "beforeId": None, "limit": 100})
        assert history["entries"] and all(h["databaseKind"] == "SqlServer" for h in history["entries"])
    finally:
        for current in (tab, observer):
            bridge.response("console_rollback", tabId=current)
        bridge.configure(ms["id"], ms["database"], "AutoCommit", tab)
        bridge.run(tab, f"DROP TABLE IF EXISTS {qualified}")
        assert bridge.rows(bridge.run(tab, f"SELECT COUNT_BIG(*) FROM sys.objects WHERE name IN (N'{table}',N'trg_{table}')")) == [[["0"]]]
        for current in (tab, observer, pg_tab):
            bridge.invoke("console_close", tabId=current, force=True)
        bridge.invoke("connection_delete", id=ms["id"])
        bridge.invoke("connection_delete", id=pg["id"])
    history = bridge.invoke("history_list", query={"text": None, "connectionId": ms["id"], "beforeId": None, "limit": 100})
    assert history["entries"] and all(h["databaseKind"] == "SqlServer" for h in history["entries"])
    print("PASS historical database kind after connection deletion")


def bridge_port():
    value = os.environ.get("L1_DATA_BRIDGE_PORT", "7077")
    if not (value.isascii() and value.isdigit() and 1 <= int(value) <= 65535):
        raise SystemExit(f"L1_DATA_BRIDGE_PORT must be a port from 1 to 65535, got {value!r}")
    return int(value)


def main():
    parser = argparse.ArgumentParser(description="Exercise PostgreSQL and SQL Server through the real dev bridge.")
    parser.add_argument("--bridge", help="Use an existing bridge instead of starting a temporary engine on :$L1_DATA_BRIDGE_PORT (default 7077).")
    parser.add_argument("--skip-assistant", action="store_true", help="Skip fake-provider configuration and assistant turns.")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="l1data-mssql-smoke-") as directory:
        work = Path(directory)
        process = None
        log = None
        try:
            url = args.bridge
            if not url:
                port = bridge_port()
                url = f"http://127.0.0.1:{port}"
                try:
                    occupied = socket.create_connection(("127.0.0.1", port), timeout=1)
                except ConnectionRefusedError:
                    pass
                else:
                    occupied.close()
                    raise RuntimeError(f"port {port} is in use; stop the bridge or pass --bridge explicitly")
                log = (work / "bridge.log").open("w")
                process = subprocess.Popen(["./target/debug/dev-bridge"],
                    env={**os.environ, "L1_DATA_DATA_DIR": directory}, stdout=log, stderr=log)
            bridge = Bridge(url)
            deadline = time.monotonic() + 15
            while True:
                if process is not None and process.poll() is not None:
                    raise RuntimeError(f"temporary bridge exited; stop any bridge already on :{port}")
                try:
                    bridge.invoke("app_boot")
                    break
                except OSError:
                    if time.monotonic() >= deadline:
                        raise
                    time.sleep(0.1)
            check(bridge, work, assistant=not args.skip_assistant)
        except Exception:
            if log is not None:
                log.flush()
                print((work / "bridge.log").read_text()[-10000:])
            raise
        finally:
            if process is not None:
                process.send_signal(signal.SIGINT)
                try:
                    process.wait(timeout=8)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
            if log is not None:
                log.close()


if __name__ == "__main__":
    main()
