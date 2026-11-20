// The person's own computer, as a story drives it: a home of its own, the
// `tilo` command run there as a person runs it in a terminal, and the MCP
// client their agent speaks to over stdio.
//
// A computer that keeps the workspace's folder mounted runs in a user and
// mount namespace of its own (`namespace: true`): mounting FUSE needs a
// namespace where the user is root, since the check's sandbox gives it no
// capability and keeps fusermount3's setuid bit from helping. Every command
// the story runs there goes through one shell in that one namespace, so the
// mount `tilo login` makes is the same folder `cp` writes into.
import {spawn} from 'node:child_process';
import {mkdirSync, writeFileSync} from 'node:fs';
import {join} from 'node:path';

const quote = text => `'${String(text).replace(/'/g, `'\\''`)}'`;

/** Claude Code, as far as `tilo login` reaches it: its `mcp add` writes the
 * server into the configuration the command reads back afterwards. */
const AGENT_SCRIPT = `#!/bin/sh
[ "$1 $2" = "mcp add" ] || exit 0
shift 2
while [ $# -gt 0 ]; do
  case "$1" in
    --scope|-s) shift 2 ;;
    --*) shift ;;
    *) break ;;
  esac
done
name=$1; shift
[ "$1" = "--" ] && shift
program=$1; shift
args=
for arg in "$@"; do args="$args\\"$arg\\","; done
printf '{"mcpServers":{"%s":{"type":"stdio","command":"%s","args":[%s],"env":{}}}}\\n' \\
  "$name" "$program" "\${args%,}" > "$HOME/.claude.json"
`;

/** A shell that runs one command at a time, answering what it said. */
class Shell {
  constructor(command, args, env) {
    this.child = spawn(command, args, {env, stdio: ['pipe', 'pipe', 'pipe']});
    this.out = '';
    this.err = '';
    this.exited = null;
    this.serial = 0;
    this.queue = Promise.resolve();
    this.child.stdout.setEncoding('utf8');
    this.child.stderr.setEncoding('utf8');
    this.child.stdout.on('data', chunk => { this.out += chunk; this.settle(); });
    this.child.stderr.on('data', chunk => { this.err = (this.err + chunk).slice(-200_000); });
    this.child.on('exit', code => {
      this.exited = code;
      const waiting = this.waiting;
      if (!waiting) return;
      this.waiting = null;
      clearTimeout(waiting.timer);
      waiting.reject(new Error(`the shell ended (${code}): ${this.err.slice(-2000)}`));
    });
  }

  /** What the command running now answered, once the shell has said so. */
  settle() {
    const waiting = this.waiting;
    if (!waiting) return;
    const at = this.out.indexOf(`\n${waiting.marker} `);
    if (at < 0) return;
    const end = this.out.indexOf('\n', at + 1);
    if (end < 0) return;
    const code = Number(this.out.slice(at + waiting.marker.length + 2, end).trim());
    const stdout = this.out.slice(0, at);
    this.out = this.out.slice(end + 1);
    this.waiting = null;
    clearTimeout(waiting.timer);
    waiting.resolve({code: Number.isFinite(code) ? code : 0, stdout, stderr: this.err});
    this.err = '';
  }

  /** Run `line` and answer `{code, stdout, stderr}`, or fail after `timeout` ms. */
  run(line, {timeout = 300_000} = {}) {
    const next = this.queue.then(() => new Promise((resolve, reject) => {
      if (this.exited !== null) {
        reject(new Error(`the shell is not running (it ended with ${this.exited})`));
        return;
      }
      const marker = `__tilo_done_${++this.serial}_${Math.random().toString(36).slice(2)}`;
      const timer = setTimeout(() => {
        this.waiting = null;
        this.child.kill('SIGKILL');
        reject(new Error(`\`${line}\` did not finish within ${timeout / 1000} s; it said: ${this.err.slice(-2000)}`));
      }, timeout);
      this.waiting = {marker, resolve, reject, timer};
      this.child.stdin.write(`${line}\nprintf '\\n%s %s\\n' ${marker} $?\n`);
      this.settle();
    }));
    this.queue = next.catch(() => undefined);
    return next;
  }

  end() {
    try { this.child.stdin.end(); } catch {}
    this.child.kill('SIGTERM');
  }
}

/** A computer at OUT/computer: its home, the environment its commands run in,
 * `run(args)` for `tilo …`, and `mcp()` for the agent's server. With
 * `namespace`, every command runs in one user and mount namespace, so the
 * workspace folder `tilo login` mounts can be written into. */
export function computerAt(out, {namespace = false, env: extra = {}, agent = null} = {}) {
  const bin = process.env.TILO_CLI_BIN;
  if (!bin) throw new Error('TILO_CLI_BIN names no tilo command');
  const home = join(out, 'computer');
  const dirs = {
    state: join(home, 'state'),
    config: join(home, 'config'),
    cache: join(home, 'cache'),
    tmp: join(home, 'tmp'),
  };
  for (const dir of [home, ...Object.values(dirs)]) mkdirSync(dir, {recursive: true});
  const env = {
    PATH: process.env.PATH,
    HOME: home,
    XDG_STATE_HOME: dirs.state,
    XDG_CONFIG_HOME: dirs.config,
    XDG_CACHE_HOME: dirs.cache,
    TMPDIR: dirs.tmp,
    ...extra,
  };
  // A computer whose agent runs `tilo login` there (`agent: 'claude'`): the
  // tool it is run from, which records the server the command adds into the
  // configuration the command then reads back, as Claude Code's own `mcp add`
  // does — and the marker Claude Code runs its commands with.
  if (agent) {
    const tool = join(home, 'bin', agent);
    mkdirSync(join(home, 'bin'), {recursive: true});
    writeFileSync(tool, AGENT_SCRIPT, {mode: 0o755});
    env.PATH = `${join(home, 'bin')}:${env.PATH}`;
    env.CLAUDECODE = '1';
  }
  const shell = namespace
    ? new Shell('unshare', ['--user', '--map-root-user', '--mount', '--', 'bash', '--noprofile', '--norc', '-s'], env)
    : new Shell('bash', ['--noprofile', '--norc', '-s'], env);
  const command = args => `${quote(bin)} ${args.map(quote).join(' ')}`;
  const run = (args, options) => shell.run(command(args), options);
  const line = (text, options) => shell.run(text, options);
  let client = null;
  /** The agent's MCP server over stdio, as an agent's client starts it. */
  const mcp = () => {
    if (namespace) throw new Error('an MCP client is started outside the namespace, not in it');
    client ??= mcpClient(spawn(bin, ['mcp'], {env, stdio: ['pipe', 'pipe', 'pipe']}));
    return client;
  };
  return {home, env, bin, run, line, mcp, close: () => { client?.close(); shell.end(); }};
}

/** A JSON-RPC client of a server speaking MCP over stdio: `initialize()`,
 * `call(tool, arguments)` and `notify(method)`. Every answer is what the
 * server said of the request it was asked; a call's text is its content. */
export function mcpClient(child, {timeout = 120_000} = {}) {
  let id = 0;
  let rest = '';
  const waiting = new Map();
  const said = [];
  child.stdout.setEncoding('utf8');
  child.stderr.setEncoding('utf8');
  child.stdout.on('data', chunk => {
    rest += chunk;
    let at;
    while ((at = rest.indexOf('\n')) >= 0) {
      const line = rest.slice(0, at);
      rest = rest.slice(at + 1);
      let answer;
      try { answer = JSON.parse(line); } catch { said.push(line); continue; }
      const pending = answer.id !== undefined ? waiting.get(answer.id) : null;
      if (!pending) { if (answer.id === undefined) said.push(`said: ${line}`); continue; }
      waiting.delete(answer.id);
      clearTimeout(pending.timer);
      if (answer.error) pending.reject(new Error(`the server refused: ${JSON.stringify(answer.error)}`));
      else pending.resolve(answer.result);
    }
  });
  child.stderr.on('data', chunk => { said.push(String(chunk)); if (said.length > 200) said.shift(); });
  child.on('exit', code => {
    for (const [n, pending] of waiting) {
      waiting.delete(n);
      clearTimeout(pending.timer);
      pending.reject(new Error(`the MCP server ended (${code}): ${said.join('').slice(-2000)}`));
    }
  });
  const write = message => child.stdin.write(`${JSON.stringify(message)}\n`);
  const request = (method, params = {}) => new Promise((resolve, reject) => {
    const n = ++id;
    const timer = setTimeout(() => {
      waiting.delete(n);
      reject(new Error(`${method} was not answered within ${timeout / 1000} s${said.length ? `; it said: ${said.join('').slice(-1500)}` : ''}`));
    }, timeout);
    waiting.set(n, {resolve, reject, timer});
    write({jsonrpc: '2.0', id: n, method, params});
  });
  const client = {
    /** What the server said on its own account, for a failure's words. */
    get said() { return said.join('').slice(-4000); },
    async initialize() {
      const answer = await request('initialize', {
        protocolVersion: '2025-06-18', capabilities: {}, clientInfo: {name: 'the person’s agent', version: '1'},
      });
      client.instructions = answer?.instructions ?? '';
      write({jsonrpc: '2.0', method: 'notifications/initialized'});
      return answer;
    },
    async listTools() {
      // Some servers answer the list only after a page asks for it.
      const answer = await request('tools/list');
      return (answer?.tools ?? []).map(tool => tool.name);
    },
    /** `tool` with `arguments`: the text it answered, or a failure. */
    async call(tool, args = {}) {
      const answer = await request('tools/call', {name: tool, arguments: args});
      const text = (answer?.content ?? []).map(block => block.text).join('\n');
      if (answer?.isError) throw new Error(`${tool} failed: ${text || JSON.stringify(answer)}`);
      return text || JSON.stringify(answer);
    },
    notify: (method, params = {}) => write({jsonrpc: '2.0', method, params}),
    close() {
      try { child.stdin.end(); } catch {}
      try { child.kill('SIGTERM'); } catch {}
    },
  };
  return client;
}
