#!/usr/bin/env python3
import argparse
import json
import subprocess
import time
import uuid
from pathlib import Path
from urllib.request import Request, urlopen

DEV = 'Run all PostgreSQL'
PROD = 'Run all Production'


class Check:
    def __init__(self, args):
        self.args = args
        if not args.session:
            args.session = subprocess.check_output(
                ['agent-browser', 'session', 'id', '--scope', 'worktree', '--prefix', 'run-all-check'],
                text=True,
            ).strip()
        self.out = Path(args.output)
        self.out.mkdir(parents=True, exist_ok=True)
        self.passed = []
        self.table = 'lab.run_all_' + uuid.uuid4().hex[:12]

    def invoke(self, command, **body):
        req = Request(self.args.bridge.rstrip('/') + '/invoke/' + command,
                      json.dumps(body).encode(), {'content-type': 'application/json'})
        with urlopen(req, timeout=30) as response:
            reply = json.load(response)
        assert reply['status'] == 'ok', (command, reply)
        return reply['data']

    def psql(self, sql):
        return subprocess.check_output(['docker', 'exec', 'l1-data-pg', 'psql', '-U', 'l1data', '-d', 'shop', '-tAc', sql],
                                       text=True).strip()

    def ab(self, *args):
        result = subprocess.run(['agent-browser', '--session', self.args.session, *args],
                                text=True, capture_output=True, timeout=45)
        assert result.returncode == 0, (args, result.stdout, result.stderr)
        return result.stdout

    def evaluate(self, expression):
        reply = json.loads(self.ab('eval', expression, '--json'))
        assert reply['success'], reply
        return reply['data']['result']

    def query(self, selector, read='textContent'):
        return self.evaluate('document.querySelector(' + json.dumps(selector) + ')?.' + read + ' ?? null')

    def wait_for(self, expression):
        self.ab('wait', '--fn', expression)

    def wait_text(self, selector, text):
        self.wait_for('document.querySelector(' + json.dumps(selector) + ')?.textContent === ' + json.dumps(text))

    def wait_toast(self, text):
        self.wait_text('[data-testid="toast-announcement"]', text)

    def screenshot(self, name):
        self.ab('screenshot', str(self.out / (name + '.png')))

    def click(self, testid):
        self.ab('find', 'testid', testid, 'click')

    def edit(self, tab, sql):
        self.click('tab-' + tab)
        self.ab('click', '[data-testid="notebook-cell-' + tab + '"] [data-testid="sql-editor"] .cm-content')
        self.ab('press', self.mod + '+a')
        self.ab('keyboard', 'inserttext', sql)

    def command(self, command, label):
        self.ab('press', self.mod + '+Shift+p')
        self.ab('wait', '[data-testid="command-input"]')
        self.ab('find', 'testid', 'command-input', 'fill', label)
        self.click('command-item-' + command)

    def reveal(self, tab):
        self.evaluate('''(() => {
            const notebook = document.querySelector('[data-testid="notebook"]');
            const cell = document.querySelector(''' + json.dumps('[data-testid="notebook-cell-' + tab + '"]') + ''');
            notebook.scrollTop += cell.getBoundingClientRect().top - notebook.getBoundingClientRect().top - 120;
            return true;
        })()''')

    def runs(self):
        return {c['tabId']: c['run'] for c in self.invoke('app_boot')['consoles']}

    def run_ids(self, tabs):
        runs = self.runs()
        return {tab: runs.get(tab, {}).get('runId') for tab in tabs}

    def active(self, tab):
        return self.query('[data-testid="notebook-cell-' + tab + '"]', 'dataset.active') == 'true'

    def question(self):
        return {
            'title': self.query('[data-testid="confirm-dialog"] h2'),
            'rows': self.query('[data-testid="confirm-rows"]', 'innerText'),
        }

    def sample(self, connection):
        marker = json.dumps('[data-testid="notebook-marker-' + connection + '/shop"]')
        self.evaluate('''(() => {
            clearInterval(window.__runSampler);
            window.__runSamples = [];
            window.__runSampler = setInterval(() => window.__runSamples.push({
                stop: !!document.querySelector('[data-testid="stop-run-btn"]'),
                marker: document.querySelector(''' + marker + ''')?.textContent ?? null,
                prompt: !!document.querySelector('[data-testid="params-dialog"]'),
            }), 20);
            return true;
        })()''')

    def samples(self):
        return self.evaluate('(() => { clearInterval(window.__runSampler); return window.__runSamples; })()')

    def pass_case(self, name, evidence):
        self.passed.append({'case': name, 'evidence': evidence})
        print('PASS ' + name, flush=True)
        (self.out / 'results.json').write_text(json.dumps(self.passed, indent=2) + '\n')

    def setup(self):
        self.psql('create table ' + self.table + ' (n int)')

        def save(name, environment):
            return self.invoke('connection_save', draft={
                'id': None, 'databaseKind': 'Postgres', 'name': name, 'host': 'localhost',
                'port': 5433, 'database': 'shop', 'user': 'l1data', 'sslMode': 'Prefer',
                'caCertPath': None, 'password': {'Set': 'l1data'}, 'color': None, 'environment': environment,
            })['id']
        dev, prod = save(DEV, None), save(PROD, 'Production')
        self.cells = {
            'base': 'select 1 as n',
            'sleep': 'select pg_sleep(2), :x::int as x',
            'boom': 'select 1/0',
            'ref': 'select n + 1 as m from {{base}}',
            'p1': 'insert into ' + self.table + ' values (1)',
            'p2': 'select pg_sleep(3), count(*) from ' + self.table,
            'p3': 'insert into ' + self.table + ' values (2); insert into ' + self.table + ' values (3)',
        }
        self.id = {name: str(uuid.uuid4()) for name in self.cells}
        self.main, self.prod = str(uuid.uuid4()), str(uuid.uuid4())
        books = [(dev, self.main, 'Run all', ['base', 'sleep', 'boom', 'ref']), (prod, self.prod, 'Production', ['p1', 'p2', 'p3'])]
        tabs, notebooks = [], []
        for conn, notebook, title, names in books:
            self.invoke('connection_connect', id=conn)
            config = {'connectionId': conn, 'database': 'shop', 'schema': None, 'mode': 'AutoCommit'}
            for name in names:
                self.invoke('console_configure', tabId=self.id[name], config=config)
                tabs.append({'id': self.id[name], 'title': name, 'text': self.cells[name], 'console': config, 'table': None,
                             'file': None, 'collapsed': False})
            notebooks.append({'id': notebook, 'title': title, 'db': {'connectionId': conn, 'database': 'shop'},
                              'cells': [self.id[n] for n in names], 'active': self.id[names[0]], 'shownAtMs': 0,
                              'params': [], 'file': None})
        workspace = self.invoke('app_boot')['workspace']
        workspace.update(tabs=tabs, active=self.id['base'], notebooks=notebooks, shown=self.main)
        workspace['layout'].update(sidebar=True, results=True, rightPanel=None, resultsHeight=220)
        self.invoke('workspace_save', workspace=workspace)
        self.ab('open', self.args.ui)
        self.ab('set', 'viewport', '1440', '900')
        self.ab('wait', '[data-testid="run-all-btn"]')
        self.mod = 'Meta' if 'Mac' in self.evaluate('navigator.platform') else 'Control'

    def check(self):
        self.setup()
        self.screenshot('after-idle')
        main = [self.id[n] for n in ('base', 'sleep', 'boom', 'ref')]

        self.sample(DEV)
        self.click('run-all-btn')
        self.ab('wait', '[data-testid="params-dialog"]')
        asked = self.evaluate('[...document.querySelectorAll(\'[data-testid^="param-value-"]\')].map(e => e.dataset.testid.slice(12))')
        assert asked == [':x'], asked
        self.ab('find', 'testid', 'param-value-:x', 'fill', '7')
        self.screenshot('run-all-prompt')
        self.click('params-run')
        self.wait_text('[data-testid="run-progress"]', '2 of 4')
        self.screenshot('run-all-progress')
        self.wait_toast('Stopped at boom: division by zero')
        self.screenshot('run-all-stopped')
        samples = self.samples()
        running = [s for s in samples if s['stop']]
        assert running and all(s['marker'] == 'Running' for s in running), samples
        prompts = sum(1 for a, b in zip(samples, samples[1:]) if b['prompt'] and not a['prompt'])
        assert prompts == 1, ('the run asked for values', prompts, 'times')
        assert self.active(self.id['boom'])
        assert self.run_ids([self.id['ref']])[self.id['ref']] is None
        self.pass_case('Run all asks for :x once, shows 2 of 4, stops at the failing cell, and reads Running throughout',
                       {'samples': len(running), 'files': ['run-all-prompt.png', 'run-all-progress.png', 'run-all-stopped.png']})

        self.edit(self.id['boom'], 'select 3 as three')
        self.click('tab-' + self.id['sleep'])
        before = self.run_ids(main)
        self.command('run-from-active-cell', 'Run from active cell')
        self.wait_toast('Ran 3 cells.')
        after = self.run_ids(main)
        changed = [n for n, t in zip(('base', 'sleep', 'boom', 'ref'), main) if after[t] != before[t]]
        assert changed == ['sleep', 'boom', 'ref'], changed
        self.screenshot('run-from-active')
        self.pass_case('Run from active cell runs the active cell and the ones after it, with no prompt', changed)

        self.edit(self.id['base'], 'select 10 as n')
        self.ab('wait', '[data-testid="stale-' + self.id['ref'] + '"]')
        self.ab('press', 'Escape')
        self.reveal(self.id['ref'])
        self.screenshot('stale-marked')
        before = self.run_ids(main)
        self.command('run-stale-cells', 'Run stale cells')
        self.wait_toast('Ran 1 cell.')
        after = self.run_ids(main)
        changed = [n for n, t in zip(('base', 'sleep', 'boom', 'ref'), main) if after[t] != before[t]]
        assert changed == ['ref'], changed
        assert self.query('[data-testid="stale-' + self.id['ref'] + '"]') is None
        self.reveal(self.id['ref'])
        self.screenshot('run-stale')
        self.pass_case('Run stale cells runs only the cell whose reference changed, and clears Stale', changed)

        self.edit(self.id['sleep'], 'select pg_sleep(5), :x::int as x')
        before = self.run_ids(main)
        self.click('run-all-btn')
        self.wait_text('[data-testid="run-progress"]', '2 of 4')
        self.screenshot('stop-progress')
        self.click('stop-run-btn')
        self.wait_toast('Stopped. 3 cells did not run.')
        runs, after = self.runs(), self.run_ids(main)
        assert runs[self.id['sleep']]['outcome'] == 'Cancelled', runs[self.id['sleep']]
        assert [after[self.id[n]] == before[self.id[n]] for n in ('boom', 'ref')] == [True, True], (before, after)
        self.screenshot('stopped')
        self.pass_case('Stop cancels the sleeping cell and skips the rest', runs[self.id['sleep']]['outcome'])

        self.click('notebook-switcher-btn')
        self.click('notebook-switcher-' + self.prod)
        self.wait_for('document.querySelector(\'[data-testid="notebook-switcher"]\') === null')
        self.click('run-all-btn')
        self.ab('wait', '[data-testid="confirm-dialog"]')
        asked = self.question()
        assert asked['title'] == 'Run 3 writes on Production?', asked
        assert asked['rows'].split('\n') == ['p1', '1 write', 'p3', '2 writes'], asked
        self.screenshot('production-question')
        self.click('confirm-btn')
        self.wait_text('[data-testid="run-progress"]', '2 of 3')
        self.edit(self.id['p3'], 'insert into ' + self.table + ' values (4)')
        self.ab('wait', '[data-testid="confirm-dialog"]')
        again = self.question()
        assert again['title'] == 'Run 1 write on Production?', again
        self.screenshot('production-asks-again')
        self.click('confirm-btn')
        self.wait_toast('Ran 3 cells.')
        rows = self.psql('select string_agg(n::text, \',\' order by n) from ' + self.table)
        assert rows == '1,4', rows
        self.screenshot('production-done')
        self.pass_case('Production asks once for every writing cell, and again for a cell edited before its turn',
                       {'question': asked, 'again': again, 'rows': rows})

    def cleanup(self):
        self.psql('drop table if exists ' + self.table)
        self.ab('close')


def main():
    parser = argparse.ArgumentParser(description='Exercise Run all, Run stale, Run from active cell, and Stop through the real bridge and browser.')
    parser.add_argument('--bridge', default='http://127.0.0.1:7077')
    parser.add_argument('--ui', default='http://localhost:5173')
    parser.add_argument('--session')
    parser.add_argument('--output', default='/tmp/l1-review/run-all')
    args = parser.parse_args()
    check = Check(args)
    try:
        check.check()
    finally:
        check.cleanup()


if __name__ == '__main__':
    main()
