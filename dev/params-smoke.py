#!/usr/bin/env python3
import argparse
import json
import subprocess
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

    def value(self, name):
        return self.evaluate('document.querySelector(' + json.dumps('[data-testid="param-value-' + name + '"]') + ').value')

    def tab(self, tab):
        self.ab('find', 'testid', 'tab-' + tab, 'click')

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

    def run(self, tab, sql, values=None, shot=None, expect=None):
        self.tab(tab)
        self.edit(sql)
        before = self.run_id(tab)
        self.ab('find', 'testid', 'run-btn', 'click')
        if values is not None:
            self.ab('wait', '[data-testid="params-dialog"]')
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
            target = json.dumps(expect[0][0])
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
        tabs = []
        for tab, conn, title in [(self.pg, pg, 'PostgreSQL parameters'), (self.ms, ms, 'SQL Server parameters')]:
            self.invoke('connection_connect', id=conn)
            config = {'connectionId': conn, 'database': 'shop', 'schema': None, 'mode': 'AutoCommit'}
            self.invoke('console_configure', tabId=tab, config=config)
            tabs.append({'id': tab, 'title': title, 'text': 'select 1', 'console': config, 'table': None, 'file': None})
        workspace = self.invoke('app_boot')['workspace']
        workspace.update(tabs=tabs, active=self.pg)
        workspace['layout'].update(sidebar=True, results=True, rightPanel=None, resultsHeight=360)
        self.invoke('workspace_save', workspace=workspace)
        (self.out / 'fixture.json').write_text(json.dumps({'pgTab': self.pg, 'msTab': self.ms, 'pgConnection': pg, 'msConnection': ms}, indent=2))
        self.ab('open', self.args.ui)
        self.ab('set', 'viewport', '1440', '900')
        self.ab('wait', '[data-testid="sql-editor"]')
        self.select_all = 'Meta+a' if 'Mac' in self.evaluate('navigator.platform') else 'Control+a'

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

        self.run(self.pg, 'select :id::int + :id::int as answer', {':id': '21'}, 'repeated', [['42']])
        self.pass_case('repeated parameter asks once', 'repeated.png')

        self.run(self.pg, 'select arr[1:2] from (select array[1,2,3] arr) t', shot='slice', expect=[['{1,2}']])
        self.pass_case('array slice does not prompt', 'slice.png')

        self.run(self.ms, 'select @x as answer', {'@x': '42'}, 'mssql-params', [['42']])
        self.run(self.ms, 'DECLARE @x int = 1; select @x as answer', shot='mssql-declared', expect=[['1']])
        self.pass_case('SQL Server prompts for external variables only', 'mssql-params.png; mssql-declared.png')

        self.run(self.pg, 'select :id::int as answer', {':id': '42'}, 'remembered-first', [['42']])
        before = self.run_id(self.pg)
        self.ab('find', 'testid', 'run-btn', 'click')
        self.ab('wait', '[data-testid="params-dialog"]')
        assert self.value(':id') == '42'
        self.screenshot('remembered')
        self.ab('find', 'testid', 'params-cancel', 'click')
        assert self.run_id(self.pg) == before
        self.pass_case('accepted values remembered in the same tab', 'remembered.png')

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
        self.screenshot('cancel')
        self.pass_case('Escape cancels without executing', 'cancel.png')


def main():
    parser = argparse.ArgumentParser(description='Exercise the parameter prompt through the real bridge and browser.')
    parser.add_argument('--bridge', default='http://127.0.0.1:7377')
    parser.add_argument('--ui', default='http://localhost:5473')
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
