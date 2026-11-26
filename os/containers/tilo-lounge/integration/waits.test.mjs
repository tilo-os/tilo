import test from 'node:test';
import assert from 'node:assert/strict';
import {mkdir, mkdtemp, rm, stat} from 'node:fs/promises';
import {createServer} from 'node:http';
import {createServer as createSocketServer} from 'node:net';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {reloaded} from './waits.mjs';
import {agentCommand, computerEnv, hostedTiloPath, runSetupPrompt} from './shell.mjs';
import {HOSTED_CLI_VERSION, setupPrompt, signInPrompt} from '../../../../crates/browser/tilo-browser-runtime/web/first-screen.mjs';

test('two simulated computers can hold independent local daemon sockets', async t => {
  // A short path fits the kernel's Unix socket name on Linux and macOS.
  const homes = await mkdtemp('/tmp/tilo-runtime-');
  t.after(() => rm(homes, {recursive: true, force: true}));
  const servers = [];
  try {
    for (const name of ['first', 'second']) {
      const home = join(homes, name);
      await mkdir(home);
      const runtime = computerEnv(home).XDG_RUNTIME_DIR;
      assert.ok(runtime, 'the computer has its own runtime directory');
      assert.equal((await stat(runtime)).mode & 0o777, 0o700);
      const dir = join(runtime, 'tilo');
      await mkdir(dir, {mode: 0o700, recursive: true});
      const server = createSocketServer();
      await new Promise((resolve, reject) => server.once('error', reject)
        .listen(join(dir, 'session-0000000000000001.api.sock'), resolve));
      servers.push(server);
    }
    assert.notEqual(servers[0].address(), servers[1].address());
  } finally {
    await Promise.all(servers.map(server => new Promise(resolve => server.close(resolve))));
  }
});

test('the page drivers run the ticket displayed in the setup and sign-in prompts', () => {
  const ticket = `tilo1${'ab'.repeat(80)}`;
  assert.deepEqual(agentCommand(setupPrompt(ticket)),
    {kind: 'login', text: ticket, ticket, code: null, site: null});
  assert.deepEqual(agentCommand(signInPrompt(ticket)),
    {kind: 'device add', text: ticket, ticket, code: null, site: null});
  assert.equal(agentCommand(signInPrompt(ticket + 'x')), null);
  assert.deepEqual(agentCommand('tilo login D9B-X4GK --site https://node.example, then follow the instructions.'),
    {kind: 'login', text: 'D9B-X4GK', ticket: null, code: 'D9B-X4GK', site: 'https://node.example'});
});

test('the copied commands run the pinned installer in the agent’s test home', async t => {
  const home = await mkdtemp(join(tmpdir(), 'tilo prompt '));
  t.after(() => rm(home, {recursive: true, force: true}));
  const paths = [];
  const installer = '/install/hosted-v1.sh';
  let available = true;
  const server = createServer((request, response) => {
    paths.push(request.url);
    if (!available || request.url !== installer) {
      response.writeHead(404);
      response.end('echo the missing route response reached the shell\n');
      return;
    }
    response.end('printf "%s\\n" "$TILO_VERSION" "$TILO_HOSTED_SETUP" "$TILO_INSTALL_DIR" "$@"\n');
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  t.after(() => new Promise(resolve => server.close(resolve)));
  const site = `http://127.0.0.1:${server.address().port}`;
  const version = HOSTED_CLI_VERSION;
  const options = {site, version, env: {HOME: home, PATH: process.env.PATH}};
  const ticket = `tilo1${'ab'.repeat(80)}`;
  const installDir = join(home, '.local/share/tilo/cli', version);
  assert.equal(hostedTiloPath(home, version), join(installDir, 'tilo'));
  const setup = setupPrompt(ticket, {site});
  const signIn = signInPrompt(ticket, {site});
  for (const [prompt, extra, args] of [
    [setup, {}, ['login', ticket]],
    [setup, {agent: 'Review agent'}, ['login', ticket, '--agent', 'Review agent']],
    [signIn, {}, ['device', 'add', ticket]],
  ]) {
    const result = await runSetupPrompt(prompt, {...options, ...extra});
    assert.equal(result.code, 0, result.stderr);
    assert.deepEqual(result.stdout.trimEnd().split('\n'), [version, '1', installDir, ...args]);
  }
  for (const [prompt, settings] of [
    [setup.replaceAll(version, 'v1.0.0-wrong.1'), options],
    [setup.replace(installer, '/install.sh'), options],
    [setup.replace(site, 'http://localhost:1'), options],
    [setup.replace('sh -s', 'echo wrong | sh -s'), options],
    [setup.replace('TILO_INSTALL_DIR="$HOME/.local/share', 'TILO_INSTALL_DIR="$HOME/.local/bin'), options],
    [setup.replace(site, 'https://public.example'), {...options, site: 'https://public.example'}],
    [setup, {...options, env: {HOME: 'relative'}}],
  ]) {
    await assert.rejects(runSetupPrompt(prompt, settings));
  }
  available = false;
  const missing = await runSetupPrompt(setup, options);
  assert.notEqual(missing.code, 0, 'curl failing while sh reads an empty pipe must fail the check');
  assert.match(missing.stderr, /404/);
  assert.equal(missing.stdout, '', 'the response body of an unavailable route must never run');
  assert.deepEqual(paths, [installer, installer, installer, installer], 'only the versioned installer was requested');
});

/** A tab reloading in `turns` reads: the document it loads answers as arrived
 * once its page is drawn (`data-arrival`, a page element), never by its own
 * phase. */
function reloading({turns = 2, presents = 1} = {}) {
  let turn = 0;
  let reloadedAt = null;
  return {
    tab: {
      reload: async () => {},
      documents: () => (reloadedAt === null ? 1 : 2),
      evaluate: async expression => {
        if (!expression.includes('readyState')) return true; // marking the old document
        turn += 1;
        if (reloadedAt === null && turn > turns) reloadedAt = turn;
        const old = reloadedAt === null;
        const up = presents === null ? null : reloadedAt !== null && turn > reloadedAt + presents;
        return JSON.stringify({old, ready: true, page: old ? true : Boolean(up)});
      },
    },
    turns: () => turn,
  };
}

test('a reload wait ends on the new document’s shell, never the one going away', async () => {
  // The old document keeps answering as a presented shell after the browser
  // took the reload. A wait that read that would judge the page that is going
  // away; this one ends only once the new document's shell says it is up.
  const {tab, turns} = reloading({turns: 3, presents: 1});
  let pauses = 0;
  const how = await reloaded(tab, {pause: async () => { pauses += 1; }, timeoutMs: 20_000});
  assert.equal(how, 'presented', 'the wait ended on the new document, not the old one’s shell');
  assert.equal(turns(), 6, 'it read until the new document said its shell was up');
  assert.equal(pauses, 5, 'it waited for a change between reads');
});

test('a page that presents and is replaced by the next navigation is waited past', async () => {
  // The reloaded document presents, but a navigation starts the next one while
  // it is read: the wait must not take the first for the page that stayed.
  let turn = 0;
  let documents = 1;
  const tab = {
    reload: async () => {},
    documents: () => documents,
    evaluate: async expression => {
      if (!expression.includes('readyState')) return true; // marking the old document
      turn += 1;
      if (turn === 2) documents = 2;
      if (turn === 3) documents = 3; // replaced while it was read
      const at = turn <= 1 ? {old: true, ready: true, page: true}
        : turn === 4 ? {old: false, ready: true, page: false}
        : {old: false, ready: true, page: true};
      return JSON.stringify(at);
    },
  };
  const how = await reloaded(tab, {pause: async () => {}, timeoutMs: 20_000});
  assert.equal(how, 'presented');
  assert.equal(turn, 5, 'it waited past the page that was replaced');
});

test('a reloaded document whose shell never says it is up is stalled', async () => {
  // A failure screen: the new document is there, its shell never is.
  const {tab} = reloading({turns: 1, presents: null});
  let clock = 0;
  const how = await reloaded(tab, {pause: async ms => { clock += ms; }, now: () => clock, timeoutMs: 1_000});
  assert.equal(how, 'stalled');
});

test('a document that never reloads times out instead of judging the old page', async () => {
  const tab = {reload: async () => {}, documents: () => 1, evaluate: async expression =>
    (expression.includes('readyState')
      ? JSON.stringify({old: true, ready: true, page: true})
      : true)};
  let clock = 0;
  const how = await reloaded(tab, {pause: async ms => { clock += ms; }, now: () => clock, timeoutMs: 1_000});
  assert.equal(how, 'timeout');
});
