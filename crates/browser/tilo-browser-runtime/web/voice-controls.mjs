/** The toolkit's shared local media commands, executed by this browser peer.
 * Call bindings come from the retained component, never from page chrome.
 * The host still decides participation and direction through its grants.
 *
 * Joining a voice channel is being in its call muted and listening, as on
 * a Tilo device: the browser is asked for nothing. The first unmute asks it
 * for the microphone; a refusal answers with the call still on, muted, and
 * the reason in the status (`permission_denied`), never with an error that
 * would end the call.
 *
 * The camera and a shared screen work as on a Tilo device: the person turns
 * them on in the call (the browser asks for them), everyone learns who shows
 * what from the room's call list (`cameras`, `screens` in each roster), the
 * app says which pictures it shows (`show`) and collects them (`pictures`),
 * coded, to decode itself.
 */
const services = new WeakMap();
const ROSTER_REFRESH_MS = 1000;
/** How long a watch is held before it is answered unchanged (the toolkit's
 * `WATCH_HOLD`), and how often a held watch looks again. */
const WATCH_HOLD_MS = 20000;
const WATCH_LOOK_MS = 100;
/** How long a pictures collection is held before it is answered empty (`PICTURES_HOLD`). */
const PICTURES_HOLD_MS = 5000;

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
    const roster = channels.get(call.channel) ?? {people: new Set(), cameras: new Set(), screens: new Set()};
    for (const participant of call.participants ?? []) {
      if (!participant.user) continue;
      roster.people.add(participant.user);
      if (participant.camera) roster.cameras.add(participant.user);
      if (participant.screen) roster.screens.add(participant.user);
    }
    channels.set(call.channel, roster);
  }
  const sorted = set => [...set].sort();
  return [...channels].filter(([, roster]) => roster.people.size > 0)
    .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))
    .map(([channel_id, roster]) => {
      const out = {channel_id, people: sorted(roster.people)};
      // Written only when someone shows something, as the native service does.
      if (roster.cameras.size) out.cameras = sorted(roster.cameras);
      if (roster.screens.size) out.screens = sorted(roster.screens);
      return out;
    });
}

export function createVoiceControls({voice, room, gesture = () => globalThis.navigator?.userActivation?.isActive === true, clock = Date, wait = ms => new Promise(resolve => setTimeout(resolve, ms))}) {
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
    const status = {version: 1, state: ({connecting: 'joining', ended: 'ended'})[state.phase] ?? state.phase,
      muted: state.muted || !live, capture_active: live && !state.muted && voice.call.stats().captureActive === true,
      input_id: live ? input : null, inputs,
      issue: state.phase === 'ended' ? 'session_ended' : null,
      inventory_revision: live ? revision : null, publication, publishers, rosters: [], speaking: [], revision: 0};
    if (live && state.camera) status.camera = true;
    if (live && state.screen) status.screen = true;
    if (live && state.micIssue) status.issue = state.micIssue;
    if (live && state.videoIssue) status.issue = state.videoIssue;
    return status;
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
      // Collected as they arrive, or after the hold: the pictures of what
      // the app shows.
      if (command.op === 'pictures') {
        if (owner !== route || !voice.call.takePictures) return stamped({...status(), state: owner === route ? status().state : 'idle'});
        const until = clock.now() + PICTURES_HOLD_MS;
        for (;;) {
          const taken = voice.call.takePictures(command.after);
          const live = voice.call.state.phase === 'live';
          if (taken.pictures.length || !live || clock.now() >= until) {
            const answer = {...status(), rosters: [], pictures: taken.pictures};
            if (taken.cursor) answer.pictures_cursor = taken.cursor;
            return answer;
          }
          await (voice.call.waitPictures?.(Math.max(0, until - clock.now())) ?? wait(WATCH_LOOK_MS));
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
        voice.call.join(call.call, {muted: true});
      } else if (command.op === 'leave') {
        generation++;
        voice.call.leave();
        owner = undefined;
      } else if (command.op === 'set_muted') {
        if (!command.muted && !gesture()) throw new Error('microphone requires a user gesture');
        if (!command.muted && (voice.call.state.phase !== 'live' || !voice.call.state.canPublish)) throw new Error('microphone requires a live publishing grant');
        if (command.muted) generation++;
        await voice.call.setMuted(command.muted);
      } else if (command.op === 'select_input') {
        await voice.call.selectInput(command.input_id);
      } else if (command.op === 'set_camera' || command.op === 'set_screen') {
        if (command.on && !gesture()) throw new Error(`${command.op === 'set_camera' ? 'the camera' : 'sharing a screen'} requires a user gesture`);
        if (command.on && (voice.call.state.phase !== 'live' || !voice.call.state.canPublish)) throw new Error('pictures require a live publishing grant');
        if (!voice.call.setVideo) throw new Error('this browser cannot show pictures');
        await voice.call.setVideo(command.op === 'set_camera' ? 'camera' : 'screen', command.on);
      } else if (command.op === 'show') {
        voice.call.showPictures?.(command.pictures ?? []);
        return stamped({...snapshot(), rosters: rosters()});
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
