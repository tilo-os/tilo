/**
 * In-process fake LIVE_V1 relay for tests, built from `@moq/net`'s own
 * in-memory origins, broadcasts and tracks. No network.
 *
 * Like the host relay it admits a grant (`carrier`, standing in for the
 * WebTransport admission preface), accepts only the granted namespace and its
 * record broadcasts from a publisher, and re-publishes every member as one
 * namespace broadcast carrying `presence` and `ops/<record>` tracks, served
 * on demand: the library's `Origin.Producer` is wrapped so that asking a
 * member broadcast for an `ops/<record>` track creates it, as the host
 * relay's on-demand track serving does.
 */
/* Browser-importable (the self-test below loads node modules only when run directly by node). */
import * as realLibrary from './media-moq-library.mjs';
import {MOQ_PROTOCOL, requestBroadcast} from './moq-carrier.mjs';

const RECORD = /^ops\/[0-9a-f]{64}$/;

/**
 * `authorize(bytes, url)` returns `{namespace}` (the only path the
 * connection may publish) or throws, like the relay's grant check.
 */
export function createFakeRelay({library = realLibrary, authorize = () => ({namespace: null})} = {}) {
  const moq = library;
  const members = new Map(); // namespace -> member
  const connections = new Set();
  const records = new Set(); // open record broadcast paths
  const counters = {connects: 0, refused: 0, published: 0};
  const timestamp = () => new moq.Time.Timestamp(Math.round(performance.now() * 1000), moq.Time.Timescale.MICRO);

  function member(path) {
    let entry = members.get(path);
    if (entry) return entry;
    const presence = new moq.Track.Producer('presence').accept({timescale: moq.Time.Timescale.MICRO, maxAge: 100});
    entry = {path, tracks: new Map([['presence', presence]]), mirrors: new Map(), owner: undefined};
    members.set(path, entry);
    for (const connection of connections) mirror(entry, connection);
    return entry;
  }
  function mirror(entry, connection) {
    if (!connection.incoming || entry.mirrors.has(connection)) return;
    const broadcast = connection.incoming.createBroadcast(entry.path);
    for (const track of entry.tracks.values()) broadcast.insertTrack(track);
    broadcast.announce();
    entry.mirrors.set(connection, broadcast);
  }
  function track(entry, name) {
    let producer = entry.tracks.get(name);
    if (producer && producer.closed.peek() === undefined) return producer;
    producer = new moq.Track.Producer(name).accept({timescale: moq.Time.Timescale.MICRO, maxAge: 2000});
    entry.tracks.set(name, producer);
    for (const broadcast of entry.mirrors.values()) {
      broadcast.removeTrack(name);
      broadcast.insertTrack(producer);
    }
    return producer;
  }
  /** End a member: its tracks end, its broadcast is withdrawn everywhere. */
  function remove(path) {
    const entry = members.get(path);
    if (!entry) return;
    members.delete(path);
    for (const producer of entry.tracks.values()) producer.close();
    for (const broadcast of entry.mirrors.values()) broadcast.close();
  }

  async function forwardPresence(publish, path, entry, alive) {
    const {broadcast, request} = await requestBroadcast(publish, path);
    const input = broadcast.track('presence').subscribe({maxAge: 100});
    try {
      for (;;) {
        const datagram = await input.recvDatagram();
        if (!datagram || !alive()) return;
        entry.tracks.get('presence')?.appendDatagram(timestamp(), datagram.payload);
      }
    } finally {
      input.close();
      request.close();
    }
  }
  async function forwardRecord(publish, path, name, entry, alive) {
    const {broadcast, request} = await requestBroadcast(publish, path);
    const input = broadcast.track('ops').subscribe({maxAge: 2000});
    try {
      for (;;) {
        const group = await input.recvGroup();
        if (!group || !alive()) return;
        const output = track(entry, name).appendGroup();
        for (;;) {
          const frame = await group.readFrame();
          if (!frame) break;
          output.writeFrame({payload: frame.payload, timestamp: timestamp()});
        }
        output.close();
      }
    } finally {
      input.close();
      request.close();
    }
  }

  async function watchPublisher(connection, publish) {
    const namespace = connection.grant?.namespace;
    const announced = publish.announced();
    const alive = () => connection.open;
    for (;;) {
      const update = await announced.next();
      if (!update || !alive()) return;
      const path = update.prefix;
      const retracted = update.kind === 'retracted';
      if (namespace && path === namespace) {
        if (retracted) { remove(path); continue; }
        const entry = member(path);
        entry.owner = connection;
        connection.own.add(path);
        counters.published++;
        void forwardPresence(publish, path, entry, () => alive() && members.get(path) === entry).catch(() => {});
        continue;
      }
      const name = namespace && path.startsWith(`${namespace}/`) ? path.slice(namespace.length + 1) : undefined;
      if (!name || !RECORD.test(name)) continue; // foreign namespace: refused
      if (retracted) { records.delete(path); continue; }
      records.add(path);
      const entry = member(namespace);
      void forwardRecord(publish, path, name, entry, () => alive() && members.get(namespace) === entry).catch(() => {});
    }
  }

  /** Stands in for the WebTransport admission preface. */
  async function carrier({url, grant}) {
    if (url.pathname !== '/moq/live') throw new Error('fake relay: wrong path');
    let admitted;
    try { admitted = await authorize(grant, url); } catch (error) { counters.refused++; throw error; }
    let revoke;
    const revoked = new Promise(resolve => { revoke = resolve; });
    return {transport: {fake: true, grant: admitted, revoke}, revoked, close: () => revoke()};
  }

  const Connection = {
    async connect({url, transport, publish, consume}) {
      if (!transport?.fake) throw new Error('fake relay: not admitted');
      counters.connects++;
      let resolveClosed;
      const closed = new Promise(resolve => { resolveClosed = resolve; });
      const connection = {
        grant: transport.grant,
        incoming: consume,
        publish,
        own: new Set(),
        open: true,
        version: MOQ_PROTOCOL,
        transport: 'webtransport',
        url,
        closed,
        close() {
          if (!connection.open) return;
          connection.open = false;
          connections.delete(connection);
          for (const path of connection.own) if (members.get(path)?.owner === connection) remove(path);
          for (const entry of members.values()) {
            entry.mirrors.get(connection)?.close();
            entry.mirrors.delete(connection);
          }
          transport.revoke();
          resolveClosed(null);
        },
      };
      void transport.revoked?.then?.(() => connection.close());
      connections.add(connection);
      for (const entry of members.values()) mirror(entry, connection);
      if (publish) void watchPublisher(connection, publish).catch(() => {});
      return connection;
    },
  };

  /** Origins the transport creates: member ops tracks are served on demand. */
  class Origin extends moq.Origin.Producer {
    request(path, options) {
      const inner = super.request(path, options);
      const wrap = broadcast => broadcast && {
        track(name) {
          const entry = members.get(path);
          if (entry && RECORD.test(name)) track(entry, name);
          return broadcast.track(name);
        },
        get closed() { return broadcast.closed; },
        close() { broadcast.close?.(); },
      };
      return {
        path,
        active: {peek: () => wrap(inner.active.peek()), changed: async () => wrap(await inner.active.changed())},
        unroutable: inner.unroutable,
        close: () => inner.close(),
      };
    }
  }

  return {
    library: {...library, Connection, Origin: {...library.Origin, Producer: Origin}},
    carrier,
    counters,
    connections,
    members,
    records,
    /**
     * A host-fed track on `path` (as the relay's own injection would be),
     * created with the member if needed.
     */
    hostTrack(path, name) { return track(member(path), name); },
    /** Withdraw a member like the relay does on revocation. */
    withdraw(path) { remove(path); },
    /**
     * Announce a withdrawn member again (a fresh, separate announcement),
     * resuming its presence from the still-connected publisher.
     */
    reannounce(path) {
      const owner = [...connections].find(connection => connection.open && connection.own.has(path));
      const entry = member(path);
      if (!owner?.publish) return;
      entry.owner = owner;
      void forwardPresence(owner.publish, path, entry, () => owner.open && members.get(path) === entry).catch(() => {});
    },
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
test('fake relay admits grants, re-publishes the granted namespace and ignores foreign ones', async () => {
  const relay = createFakeRelay({authorize: bytes => { if (bytes?.[0] !== 1) throw new Error('unauthorized'); return {namespace: 'tilo-live/r/a'}; }});
  const url = new URL('https://relay.test/moq/live');
  await assert.rejects(relay.carrier({url, grant: new Uint8Array([2])}));
  const outgoing = new realLibrary.Origin.Producer();
  const incoming = new realLibrary.Origin.Producer();
  const opened = await relay.carrier({url, grant: new Uint8Array([1])});
  const connection = await relay.library.Connection.connect({url, transport: opened.transport, publish: outgoing.consume(), consume: incoming});
  const watch = incoming.announced(realLibrary.Path.Pattern.subtree('tilo-live/r'));
  outgoing.createBroadcast('tilo-live/r/b').announce();
  const own = outgoing.createBroadcast('tilo-live/r/a');
  own.createTrack('presence');
  own.announce();
  assert.equal((await watch.next()).prefix, 'tilo-live/r/a');
  connection.close();
  assert.equal((await watch.next()).kind, 'retracted');
  assert.equal(relay.counters.refused, 1);
  assert.equal(relay.members.has('tilo-live/r/b'), false);
});
