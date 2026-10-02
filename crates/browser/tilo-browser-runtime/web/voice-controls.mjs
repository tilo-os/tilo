/** The toolkit's shared local media commands, executed by this browser peer.
 * Call bindings come from the retained component, never from page chrome.
 * The host still decides participation and direction through its grants.
 */
const services = new WeakMap();

export function createVoiceControls({voice, room, gesture = () => globalThis.navigator?.userActivation?.isActive === true}) {
  let generation = 0;
  let owner;
  let inventory = '';
  let revision = 0;
  let inputs = [];
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
      inventory_revision: live ? revision : null, publication, publishers};
  }
  return {
    async execute(request) {
      if (request.app !== 'so.tilo.chat' || request.room !== room || request.command?.version !== 1) throw new Error('unsupported local media route');
      const command = request.command.command;
      const route = key(request);
      if (command.op === 'status') {
        if (owner && owner !== route) return {...snapshot(), state: 'idle', muted: true, capture_active: false, input_id: null, issue: null, inventory_revision: null, publication: null, publishers: []};
        return snapshot();
      }
      if (owner && owner !== route && command.op !== 'join') throw new Error('this route does not own the active call');
      if (command.op === 'join') {
        const ticket = ++generation;
        if (owner && owner !== route) voice.call.leave();
        owner = route;
        const calls = await voice.list();
        if (ticket !== generation) throw new Error('call join superseded');
        const call = calls.find(call => call.app === request.app && call.record === request.record && call.channel === command.channel_id && call.active);
        if (!call) throw new Error('channel has no active call');
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
      } else throw new Error('unsupported local media command');
      // Enumeration never prompts; update labels after consent without
      // delaying a stop command's completion.
      void voice.call.inputs().then(devices => {inputs = devices.filter(d => d.deviceId).map(d => ({id: d.deviceId, label: d.label || d.deviceId}));}).catch(() => {});
      return snapshot();
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
