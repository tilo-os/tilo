/** The toolkit's shared local media commands, executed by this browser peer.
 * Call bindings come from the retained component, never from page chrome.
 * The host still decides participation and direction through its grants.
 *
 * Joining a voice channel is being in its call with the microphone on, as
 * on a Tilo device, unless the person last chose to be muted: that choice
 * is remembered (`preferences`). The browser asks for the microphone itself.
 */
const services = new WeakMap();
const ROSTER_REFRESH_MS = 1000;
/** How long a watch is held before it is answered unchanged (the toolkit's
 * `WATCH_HOLD`), and how often a held watch looks again. */
const WATCH_HOLD_MS = 20000;
const WATCH_LOOK_MS = 100;
const JOIN_MUTED_KEY = 'tilo.calls.joinMuted';

/** The person's remembered mute choice, in this browser's storage. */
export function storedCallPreferences(storage = globalThis.localStorage) {
  return {
    joinMuted() { try { return storage?.getItem(JOIN_MUTED_KEY) === 'true'; } catch { return false; } },
    remember(muted) { try { storage?.setItem(JOIN_MUTED_KEY, muted ? 'true' : 'false'); } catch {} },
  };
}

/** The revision of a status's content (FNV-1a over its JSON): it changes
 * whenever anything a watcher sees changes, and is never zero. */
export function revisionOf(status) {
  const text = JSON.stringify({...status, revision: 0});
  let hash = 0x811c9dc5;
  for (let index = 0; index < text.length; index++) {
    hash ^= text.charCodeAt(index);
    hash = Math.imul(hash, 0x01000193) >>> 0;
  }
  return hash || 1;
}

/** Who is in each active call of `app`'s `record`, one roster per voice
 * channel with anyone in it, sorted by channel; each person once. */
export function rostersOf(calls, app, record) {
  const channels = new Map();
  for (const call of calls ?? []) {
    if (!call.active || call.app !== app || call.record !== record) continue;
    const people = channels.get(call.channel) ?? new Set();
    for (const participant of call.participants ?? []) if (participant.user) people.add(participant.user);
    channels.set(call.channel, people);
  }
  return [...channels].filter(([, people]) => people.size > 0)
    .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))
    .map(([channel_id, people]) => ({channel_id, people: [...people].sort()}));
}

export function createVoiceControls({voice, room, gesture = () => globalThis.navigator?.userActivation?.isActive === true, preferences = storedCallPreferences(), clock = Date, wait = ms => new Promise(resolve => setTimeout(resolve, ms))}) {
  let generation = 0;
  let owner;
  let inventory = '';
  let revision = 0;
  let inputs = [];
  let listedAt = -Infinity;
  function refreshRosters() {
    if (!voice.list || clock.now() - listedAt < ROSTER_REFRESH_MS) return;
    listedAt = clock.now();
    void voice.list().catch(() => {});
  }
  const key = request => `${request.app}:${request.record}:${request.owner ?? request.instance ?? ''}`;
  function snapshot() {
    const state = voice.call.state;
    const live = state.phase === 'live';
    const publishers = live ? state.participants.filter(p => !p.self && p.publishes).map(p => p.namespace).sort() : [];
    const publication = live && state.canPublish ? state.participants.find(p => p.self)?.namespace ?? null : null;
    const next = JSON.stringify([publication, publishers]);
    if (next !== inventory) {inventory = next; revision++;}
    const input = state.inputId || 'default';
    return {version: 1, state: ({connecting: 'joining', ended: 'ended'})[state.phase] ?? state.phase,
      muted: state.muted || !live, capture_active: live && !state.muted && voice.call.stats().captureActive === true,
      input_id: live ? input : null, inputs,
      issue: state.phase === 'ended' ? 'session_ended' : null,
      inventory_revision: live ? revision : null, publication, publishers, rosters: [], speaking: [], revision: 0};
  }
  const stamped = status => ({...status, revision: revisionOf(status)});
  return {
    async execute(request) {
      if (request.app !== 'so.tilo.chat' || request.room !== room || request.command?.version !== 1) throw new Error('unsupported local media route');
      const command = request.command.command;
      const route = key(request);
      const rosters = () => rostersOf(voice.calls, request.app, request.record);
      const status = () => {
        if (owner && owner !== route) return stamped({...snapshot(), state: 'idle', muted: true, capture_active: false, input_id: null, issue: null, inventory_revision: null, publication: null, publishers: [], rosters: rosters()});
        return stamped({...snapshot(), rosters: rosters()});
      };
      if (command.op === 'status') {
        refreshRosters();
        return status();
      }
      // Answered once the status differs from the one the app saw, or after
      // the hold: who joins and leaves reaches the app as it happens.
      if (command.op === 'watch') {
        refreshRosters();
        const until = clock.now() + WATCH_HOLD_MS;
        for (;;) {
          const current = status();
          if (current.revision !== command.revision || clock.now() >= until) return current;
          await wait(WATCH_LOOK_MS);
        }
      }
      if (owner && owner !== route && command.op !== 'join') throw new Error('this route does not own the active call');
      if (command.op === 'join') {
        const ticket = ++generation;
        if (owner && owner !== route) voice.call.leave();
        owner = route;
        // Joining a voice channel opens its call: the host starts it when
        // the channel has none yet.
        const call = await voice.open({app: request.app, record: request.record, channel: command.channel_id});
        if (ticket !== generation) throw new Error('call join superseded');
        if (!call?.active) throw new Error('channel has no active call');
        voice.call.join(call.call, {muted: preferences.joinMuted()});
      } else if (command.op === 'leave') {
        generation++;
        voice.call.leave();
        owner = undefined;
      } else if (command.op === 'set_muted') {
        if (!command.muted && !gesture()) throw new Error('microphone requires a user gesture');
        if (!command.muted && (voice.call.state.phase !== 'live' || !voice.call.state.canPublish)) throw new Error('microphone requires a live publishing grant');
        preferences.remember(command.muted);
        if (command.muted) generation++;
        await voice.call.setMuted(command.muted);
      } else if (command.op === 'select_input') {
        await voice.call.selectInput(command.input_id);
      } else throw new Error('unsupported local media command');
      // Enumeration never prompts; update labels after consent without
      // delaying a stop command's completion.
      void voice.call.inputs().then(devices => {inputs = devices.filter(d => d.deviceId).map(d => ({id: d.deviceId, label: d.label || d.deviceId}));}).catch(() => {});
      return stamped({...snapshot(), rosters: rosters()});
    },
    snapshot,
    release(request) {if (owner === key(request)) {generation++; voice.call.leave(); owner = undefined;}},
    dispose() {generation++; if (owner) voice.call.leave(); owner = undefined;},
  };
}

/** Pump the bound media lane on a mounted surface; stopping does not wait
 * for another command or a pending room lookup. */
export function createMediaPump({runtime, id, voice, room, active, report = () => {}, clock = globalThis}) {
  let controls = services.get(voice);
  if (!controls) {controls = createVoiceControls({voice, room}); services.set(voice, controls);}
  const routes = new Map();
  let closed = false;
  let timer;
  function schedule() {
    if (closed || timer !== undefined || ['idle', 'ended'].includes(voice.call.state.phase)) return;
    timer = clock.setTimeout(() => {timer = undefined; pump();}, 500);
  }
  function pump() {
    if (closed || !active() || !runtime.take_media_requests) return;
    for (const request of JSON.parse(runtime.take_media_requests(id))) {
      const route = `${request.app}:${request.record}:${request.owner ?? request.instance}`;
      if (request.retired) {controls.release(request); routes.delete(route); continue;}
      routes.set(route, request);
      void controls.execute(request).then(snapshot => ({Ok: snapshot}), error => ({Err: String(error.message ?? error)})).then(result => {
        if (!closed && active()) runtime.complete_media_request(id, BigInt(request.ticket), JSON.stringify(result));
      }).catch(error => report({mediaError: String(error)}));
    }
    schedule();
  }
  const unsubscribe = voice.call.subscribe(() => {queueMicrotask(() => {if (!closed) pump();});});
  return {
    pump,
    close() {
      if (closed) return;
      closed = true;
      if (timer !== undefined) clock.clearTimeout(timer);
      unsubscribe?.();
      for (const request of routes.values()) controls.release(request);
      routes.clear();
    },
  };
}
