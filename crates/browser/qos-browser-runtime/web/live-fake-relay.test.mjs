/**
 * In-process fake LIVE_V1 relay for tests: the vendored MoQ library's own
 * in-memory broadcasts/tracks behind a fake `Connection` that checks the
 * authorization token (like the host relay) and routes namespaces and
 * announcements between connections. No network.
 */
/* Browser-importable (the self-test below loads node modules only when run directly by node). */
import * as realLibrary from './media-moq-library.mjs';

function stripPrefix(prefix, path) {
  if (prefix === '') return path;
  if (path === prefix) return '';
  return path.startsWith(`${prefix}/`) ? path.slice(prefix.length + 1) : null;
}

function eventQueue() {
  const items = [];
  const waiters = [];
  let closed = false;
  return {
    push(item) {
      if (closed) return;
      const waiter = waiters.shift();
      if (waiter) waiter(item); else items.push(item);
    },
    next() {
      if (items.length) return Promise.resolve(items.shift());
      if (closed) return Promise.resolve(undefined);
      return new Promise(resolve => waiters.push(resolve));
    },
    close() {
      closed = true;
      for (const waiter of waiters.splice(0)) waiter(undefined);
    },
  };
}

/**
 * `authorize(bytes, url)` returns `{namespace}` (the only path the
 * connection may publish) or throws, like the relay's grant check.
 */
export function createFakeRelay({library = realLibrary, authorize = () => ({namespace: null})} = {}) {
  const published = new Map(); // path -> {consumer}
  const watchers = new Set();
  const connections = new Set();
  const counters = {connects: 0, refused: 0, published: 0};
  const announce = (path, active) => {
    for (const watcher of watchers) {
      const suffix = stripPrefix(watcher.prefix, path);
      if (suffix !== null && suffix !== '') watcher.queue.push({path: suffix, active});
    }
  };
  const Connection = {
    async connect(url, options = {}) {
      if (url.pathname !== '/moq/v21/live') throw new Error('fake relay: wrong path');
      if (!options.webtransport?.protocols?.includes('moqt-21')) throw new Error('fake relay: wrong subprotocol');
      let grant;
      try { grant = await authorize(options.authorization, url); } catch (error) { counters.refused++; throw error; }
      counters.connects++;
      let resolveClosed;
      const closed = new Promise(resolve => { resolveClosed = resolve; });
      const own = new Set();
      const mine = new Set();
      let open = true;
      const connection = {
        version: 'moq-transport-21',
        transport: 'webtransport',
        closed,
        publish(path, broadcast) {
          if (!open) throw new Error('fake relay: closed');
          if (grant?.namespace && path !== grant.namespace) throw new Error('fake relay: foreign namespace');
          published.get(path)?.end();
          let end;
          const closedSignal = new Promise(resolve => { end = resolve; });
          published.set(path, {consumer: broadcast.consume(), closed: closedSignal, end: () => end(null)});
          own.add(path);
          counters.published++;
          announce(path, true);
        },
        announced(prefix) {
          const watcher = {prefix, queue: eventQueue()};
          for (const path of published.keys()) {
            const suffix = stripPrefix(prefix, path);
            if (suffix !== null && suffix !== '') watcher.queue.push({path: suffix, active: true});
          }
          watchers.add(watcher);
          mine.add(watcher);
          return {next: () => watcher.queue.next(), close: () => { watcher.queue.close(); watchers.delete(watcher); }};
        },
        consume(path) {
          const entry = published.get(path);
          // Never close the shared consumer: its last close would close the publisher.
          return {
            track: name => ({subscribe: options => {
              if (!entry) return new library.Broadcast.Consumer().subscribe(name, options);
              const track = entry.consumer.subscribe(name, options);
              if (entry.ops && name.startsWith('ops/')) entry.ops(name, options);
              return track;
            }}),
            closed: entry ? entry.closed : Promise.resolve(null),
            close() {},
          };
        },
        close() {
          if (!open) return;
          open = false;
          connections.delete(connection);
          for (const path of own) { published.get(path)?.end(); published.delete(path); announce(path, false); }
          for (const watcher of mine) { watcher.queue.close(); watchers.delete(watcher); }
          resolveClosed();
        },
      };
      connections.add(connection);
      return connection;
    },
  };
  return {
    library: {...library, Connection},
    counters,
    connections,
    published,
    /**
     * Withdraw a namespace like the relay does on revocation: the consumed
     * broadcast closes, but already-open track subscriptions are not ended
     * and no inactive announcement is sent (a fresh announcement follows).
     */
    withdraw(path) { const entry = published.get(path); published.delete(path); entry?.end(); },
    /** Announce a path again without a preceding inactive event. */
    reannounce(path) { announce(path, true); },
    /** Observe every ops subscription served for a published path. */
    onOpsSubscribe(path, callback) { const entry = published.get(path); if (entry) entry.ops = callback; },
    /** Drop every connection (relay restart / network loss). */
    dropAll() { for (const connection of [...connections]) connection.close(); },
  };
}

// Self-test only when run directly by node, not when imported by other live
// tests or by the in-browser harness.
const direct = Boolean(globalThis.process?.argv?.[1])
  && import.meta.url === (await import('node:url')).pathToFileURL(globalThis.process.argv[1]).href;
const test = direct ? (await import('node:test')).default : () => {};
const assert = direct ? (await import('node:assert/strict')).default : undefined;
test('fake relay routes announcements and refuses unauthorized connections', async () => {
  const relay = createFakeRelay({authorize: bytes => { if (bytes?.[0] !== 1) throw new Error('unauthorized'); return {namespace: 'tilo-live/r/a'}; }});
  const url = new URL('https://relay.test/moq/v21/live');
  await assert.rejects(relay.library.Connection.connect(url, {authorization: new Uint8Array([2]), webtransport: {protocols: ['moqt-21']}}));
  const a = await relay.library.Connection.connect(url, {authorization: new Uint8Array([1]), webtransport: {protocols: ['moqt-21']}});
  const watch = a.announced('tilo-live/r');
  const broadcast = new realLibrary.Broadcast.Producer();
  assert.throws(() => a.publish('tilo-live/r/b', broadcast));
  a.publish('tilo-live/r/a', broadcast);
  assert.deepEqual(await watch.next(), {path: 'a', active: true});
  a.close();
  assert.deepEqual(await watch.next(), {path: 'a', active: false});
  assert.equal(relay.counters.refused, 1);
});
