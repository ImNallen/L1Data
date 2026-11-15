#!/usr/bin/env python3
import argparse
import json
import shutil
import subprocess
import tempfile
import time
import uuid
from pathlib import Path
from urllib.request import Request, urlopen


class Check:
    def __init__(self, args):
        self.args = args
        if not args.session:
            args.session = subprocess.check_output(
                ['agent-browser', 'session', 'id', '--scope', 'worktree', '--prefix', 'params-check'],
                text=True,
            ).strip()
        self.out = Path(args.output)
        self.out.mkdir(parents=True, exist_ok=True)
        self.passed = []

    def invoke(self, command, **body):
        req = Request(self.args.bridge.rstrip('/') + '/invoke/' + command,
                      json.dumps(body).encode(), {'content-type': 'application/json'})
        with urlopen(req, timeout=30) as response:
            reply = json.load(response)
        assert reply['status'] == 'ok', (command, reply)
        return reply['data']

    def ab(self, *args):
        result = subprocess.run(['agent-browser', '--session', self.args.session, *args],
                                text=True, capture_output=True, timeout=45)
        assert result.returncode == 0, (args, result.stdout, result.stderr)
        return result.stdout

    def evaluate(self, expression):
        reply = json.loads(self.ab('eval', expression, '--json'))
        assert reply['success'], reply
        return reply['data']['result']

    def screenshot(self, name):
        self.ab('screenshot', str(self.out / (name + '.png')))

    def count(self, testid):
        return self.evaluate('document.querySelectorAll(' + json.dumps('[data-testid="' + testid + '"]') + ').length')

    def strip_value(self, name):
        return self.evaluate('document.querySelector(' + json.dumps('[data-testid="notebook-param-value-' + name + '"]') + ')?.value ?? null')

    def asked(self):
        return self.evaluate('[...document.querySelectorAll(\'[data-testid^="param-value-"]\')].map(e => e.dataset.testid.slice(12))')

    def show(self, notebook):
        self.ab('find', 'testid', 'notebook-switcher-btn', 'click')
        self.ab('find', 'testid', 'notebook-switcher-' + notebook, 'click')
        self.ab('wait', '--fn', 'document.querySelector(\'[data-testid="notebook-switcher"]\') === null')

    def tab(self, tab):
        if self.count('tab-' + tab) == 0:
            self.show(self.notebook_of[tab])
        self.ab('find', 'testid', 'tab-' + tab, 'click')

    def unsaved(self):
        return self.count('notebook-unsaved') == 1

    def save(self, path=None):
        if path:
            self.evaluate('window.__l1Dialog.answer(' + json.dumps(str(path)) + ')')
        self.ab('press', self.mod + '+s')
        self.ab('wait', '--fn', 'document.querySelector(\'[data-testid="notebook-unsaved"]\') === null')

    def header(self, path):
        deadline = time.monotonic() + 10
        while not Path(path).exists() and time.monotonic() < deadline:
            time.sleep(.05)
        return Path(path).read_text().split('\n', 1)[0]

    def edit(self, sql):
        self.ab('click', '[data-testid="sql-editor"] .cm-content')
        self.ab('press', self.select_all)
        self.ab('keyboard', 'inserttext', sql)

    def console(self, tab):
        return next(c for c in self.invoke('app_boot')['consoles'] if c['tabId'] == tab)

    def run_id(self, tab):
        return self.console(tab)['run'].get('runId')

    def finish(self, tab, before):
        deadline = time.monotonic() + 20
        last = None
        while time.monotonic() < deadline:
            last = self.console(tab)['run']
            if last['kind'] == 'Finished' and last['runId'] != before:
                assert last['outcome'] == 'Ok', last
                return last
            time.sleep(.05)
        raise AssertionError(('No successful new run', last))

    def rows(self, run):
        result = next(o['result'] for s in run['statements'] for o in s['outputs'] if o['kind'] == 'Rows')
        return self.invoke('result_page', resultRef=result['ref'], start=0, len=10)['rows']

    def run(self, tab, sql=None, values=None, shot=None, expect=None):
        self.tab(tab)
        if sql is not None:
            self.edit(sql)
        before = self.run_id(tab)
        self.ab('find', 'testid', 'run-btn', 'click')
        if values is not None:
            self.ab('wait', '[data-testid="params-dialog"]')
            assert self.asked() == list(values), (self.asked(), list(values))
            for name, value in values.items():
                self.ab('find', 'testid', 'param-value-' + name, 'fill', value)
            if shot:
                self.screenshot(shot + '-dialog')
            self.ab('find', 'testid', 'params-run', 'click')
        run = self.finish(tab, before)
        assert self.count('params-dialog') == 0
        rows = self.rows(run)
        if expect is not None:
            assert rows == expect, (sql, rows, expect)
            target = json.dumps('NULL' if expect[0][0] is None else expect[0][0])
            self.ab('wait', '--fn', 'document.querySelector(\'[data-testid="grid-cell-0-0"]\')?.textContent === ' + target)
        if shot:
            self.screenshot(shot)
        return run, rows

    def pass_case(self, name, evidence):
        self.passed.append({'case': name, 'evidence': evidence})
        print('PASS ' + name, flush=True)
        (self.out / 'results.json').write_text(json.dumps(self.passed, indent=2) + '\n')

    def setup(self):
        def save(kind, name, port, user, password):
            return self.invoke('connection_save', draft={
                'id': None, 'databaseKind': kind, 'name': name, 'host': 'localhost',
                'port': port, 'database': 'shop', 'user': user,
                'sslMode': 'Require' if kind == 'SqlServer' else 'Prefer',
                'caCertPath': None, 'password': {'Set': password}, 'color': None,
            })['id']
        pg = save('Postgres', 'P11 PostgreSQL', 5433, 'l1data', 'l1data')
        ms = save('SqlServer', 'P11 SQL Server', 1434, 'sa', 'L1Data_dev_Str0ng')
        self.pg, self.ms = str(uuid.uuid4()), str(uuid.uuid4())
        self.pg_a, self.pg_b, self.ms_a, self.ms_b = (str(uuid.uuid4()) for _ in range(4))
        cells = {
            pg: [('Parameters', [(self.pg, 'PostgreSQL parameters', 'select 1')]),
                 ('Reference', [(self.pg_a, 'a', 'select :x::int + 1 as n'), (self.pg_b, 'b', 'select * from {{a}}')])],
            ms: [('Parameters', [(self.ms, 'SQL Server parameters', 'select 1')]),
                 ('Reference', [(self.ms_a, 'a', 'select CAST(@X AS int) + 1 as n'),
                                (self.ms_b, 'b', 'DECLARE @X int = 1\nselect * from {{a}} where n > @X')])],
        }
        tabs, notebooks, self.notebook_of = [], [], {}
        for conn, books in cells.items():
            self.invoke('connection_connect', id=conn)
            config = {'connectionId': conn, 'database': 'shop', 'schema': None, 'mode': 'AutoCommit'}
            for title, cell_list in books:
                notebook = str(uuid.uuid4())
                for tab, name, text in cell_list:
                    self.invoke('console_configure', tabId=tab, config=config)
                    tabs.append({'id': tab, 'title': name, 'text': text, 'console': config, 'table': None,
                                 'file': None, 'collapsed': False})
                    self.notebook_of[tab] = notebook
                notebooks.append({'id': notebook, 'title': title, 'db': {'connectionId': conn, 'database': 'shop'},
                                  'cells': [c[0] for c in cell_list], 'active': cell_list[-1][0], 'shownAtMs': 0,
                                  'params': [], 'file': None})
        workspace = self.invoke('app_boot')['workspace']
        workspace.update(tabs=tabs, active=self.pg, notebooks=notebooks, shown=self.notebook_of[self.pg])
        workspace['layout'].update(sidebar=True, results=True, rightPanel=None, resultsHeight=300)
        self.invoke('workspace_save', workspace=workspace)
        (self.out / 'fixture.json').write_text(json.dumps({'pgTab': self.pg, 'msTab': self.ms, 'pgConnection': pg, 'msConnection': ms}, indent=2))
        self.ab('open', self.args.ui)
        self.ab('set', 'viewport', '1440', '900')
        self.ab('wait', '[data-testid="sql-editor"]')
        self.mod = 'Meta' if 'Mac' in self.evaluate('navigator.platform') else 'Control'
        self.select_all = self.mod + '+a'

    def check(self):
        self.setup()
        _, rows = self.run(self.pg, "select now()::date, 'a:b' as text", shot='no-params')
        assert rows[0][1] == 'a:b', rows
        self.pass_case('casts and string colons do not prompt', rows)

        _, rows = self.run(self.pg, 'select * from orders where id = :id', {':id': '42'}, 'named-param')
        assert len(rows) == 1 and rows[0][0] == '42', rows
        self.pass_case('named parameter executes', 'named-param.png')

        self.run(self.pg, 'select $1::int + 1 as answer', {'$1': '41'}, 'positional', [['42']])
        self.pass_case('positional parameter executes', 'positional.png')

        self.run(self.pg, 'select :text::text as value', {':text': "O'Brien"}, 'quote-value', [["O'Brien"]])
        self.pass_case('apostrophe stays inside the literal', 'quote-value.png')

        self.run(self.pg, 'select :twice::int + :twice::int as answer', {':twice': '21'}, 'repeated', [['42']])
        self.pass_case('repeated parameter asks once', 'repeated.png')

        self.run(self.pg, 'select arr[1:2] from (select array[1,2,3] arr) t', shot='slice', expect=[['{1,2}']])
        self.pass_case('array slice does not prompt', 'slice.png')

        self.run(self.ms, 'select @x as answer', {'@x': '42'}, 'mssql-params', [['42']])
        self.run(self.ms, 'DECLARE @x int = 1; select @x as answer', shot='mssql-declared', expect=[['1']])
        self.pass_case('SQL Server prompts for external variables only', 'mssql-params.png; mssql-declared.png')

        self.run(self.pg, 'select :id::int as answer', shot='known', expect=[['42']])
        assert self.strip_value(':id') == '42'
        self.pass_case('a name the notebook holds runs without asking', 'known.png')

        self.ab('find', 'testid', 'open-queries', 'click')
        self.ab('wait', '[data-testid="history-input"]')
        self.ab('find', 'testid', 'history-input', 'fill', 'answer')
        self.ab('wait', '--fn', 'document.querySelector(\'[data-testid="history-sql"]\')?.textContent.includes("42")')
        history = self.evaluate('document.querySelector(\'[data-testid="history-sql"]\').textContent')
        assert ':id' not in history and '42' in history, history
        self.screenshot('history')
        self.ab('find', 'testid', 'queries-close', 'click')
        self.ab('wait', '--fn', 'document.querySelector(\'[data-testid="queries-dialog"]\') === null')
        self.pass_case('history contains executed literal SQL', history)

        self.edit('select :cancel::text as answer')
        before = self.run_id(self.pg)
        self.ab('find', 'testid', 'run-btn', 'click')
        self.ab('wait', '[data-testid="params-dialog"]')
        self.ab('press', 'Escape')
        assert self.count('params-dialog') == 0 and self.run_id(self.pg) == before
        assert self.strip_value(':cancel') is None
        self.screenshot('cancel')
        self.pass_case('Escape cancels without executing or keeping a value', 'cancel.png')

        self.reference()

    def reference(self):
        files = Path(tempfile.mkdtemp(prefix='l1-params-'))
        path = files / 'reference.sql'

        self.run(self.pg_b, values={':x': '41'}, shot='ref-pg-prompt', expect=[['42']])
        assert self.strip_value(':x') == '41'
        self.screenshot('ref-pg-strip')
        self.pass_case('a referenced cell asks for its parameter, and the strip shows it', 'ref-pg-prompt-dialog.png; ref-pg-strip.png')

        self.run(self.pg_b, expect=[['42']])
        self.pass_case('the second run of the referencing cell does not ask', 'no dialog')

        self.save(path)
        assert self.header(path).endswith(' param=":x=41"'), self.header(path)
        self.ab('find', 'testid', 'notebook-param-value-:x', 'fill', '1')
        assert self.unsaved()
        self.run(self.pg_b, shot='ref-pg-edited', expect=[['2']])
        self.pass_case('a strip edit runs with the new value and marks the file notebook unsaved', 'ref-pg-edited.png')

        self.ab('find', 'testid', 'notebook-param-null-:x', 'click')
        self.run(self.pg_b, shot='ref-pg-null', expect=[[None]])
        self.save()
        assert self.header(path).endswith(' null=":x"'), self.header(path)
        self.ab('find', 'testid', 'notebook-param-null-:x', 'click')
        self.ab('find', 'testid', 'notebook-param-value-:x', 'fill', '5')
        self.save()
        header = self.header(path)
        assert header.endswith(' param=":x=5"'), header
        self.pass_case('NULL and a value save in the header', header)

        copy = files / 'reopened.sql'
        shutil.copy(path, copy)
        self.evaluate('window.__l1Dialog.answer(' + json.dumps([str(copy)]) + ')')
        self.ab('press', self.mod + '+o')
        self.ab('wait', '--fn', 'document.querySelector(\'[data-testid="notebook-param-value-:x"]\')?.value === "5" && document.querySelector(\'[data-testid="notebook-switcher-btn"]\')?.title === "reopened"')
        self.screenshot('ref-pg-reopened')
        self.pass_case('a reopened file shows its values in the strip', 'ref-pg-reopened.png')

        self.run(self.ms_b, values={'@X': '41'}, shot='ref-ms-prompt', expect=[['42']])
        assert self.strip_value('@X') == '41'
        self.screenshot('ref-ms-strip')
        self.run(self.ms_b, expect=[['42']])
        self.pass_case('SQL Server binds the referenced @X before the batch declares its own', 'ref-ms-prompt-dialog.png; ref-ms-strip.png')


def main():
    parser = argparse.ArgumentParser(description='Exercise the parameter prompt through the real bridge and browser.')
    parser.add_argument('--bridge', default='http://127.0.0.1:7077')
    parser.add_argument('--ui', default='http://localhost:5173')
    parser.add_argument('--session')
    parser.add_argument('--output', default='/tmp/l1-review/P11')
    args = parser.parse_args()
    check = Check(args)
    try:
        check.check()
    finally:
        check.ab('close')


if __name__ == '__main__':
    main()
