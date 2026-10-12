/**
 * `peer.actions`: the room's app actions, listed and invoked from the page.
 *
 * The host answers `tilo_actions::remote` frames addressed to its own device
 * for every record of the room, open or not (`RoomActions` in
 * `tilo-agent-participant`): it mounts the record headless, runs the action
 * through the app's own door and commits the change as its own, so every page
 * receives it as an ordinary durable change. This module speaks those frames
 * over the peer's remote-action path; it knows nothing of the carrier.
 *
 * - `catalogue(query)` → `CataloguePage`: a provider per record
 *   (`record:<app>:<record>`) and the room's (`workspace:<room>`); with no
 *   provider named, rows for the records open on the desktop.
 * - `invoke(target, argument, {confirmation, invocation, label})` → a ticket
 *   (actionsV2): `{id, done, status(), cancel()}`. The intent is saved before
 *   it is sent and the id is reused on every retry, so a lost reply never
 *   runs the action twice.
 * - `status(id)`, `cancel(id)`, `recover()`; `history()` is the recent
 *   invocations, kept in the page's store.
 * - `confirmation(descriptor)` says what a descriptor's contract asks the
 *   person before it runs: `null`, `{kind: 'confirm'}` or
 *   `{kind: 'phrase', phrase}`.
 * - `provider` answers a search palette: `query(needle)` → hits whose target
 *   is an action, `choose(hit, {confirmation})` → a ticket.
 * - `v1` is the frozen `actionsV1` contract the Cloud console reads.
 */

const REMOTE_ACTIONS_VERSION = 1;
const DEFAULT_TIMEOUT_MS = 30_000;
// A reply timer that fires this long after it was due ran behind a stall of
// the page itself (a long task, a frozen or throttled tab), not the host.
const LATE_TIMER_MS = 100;
// The least a stalled page is given to read an answer that reached it while it
// could not run.
const STALL_GRACE_MS = 1_000;
const CATALOGUE_REFRESH_MS = 15_000;
const HISTORY_LIMIT = 50;
const PALETTE_ROWS = 50;
export const ROOM_ACTIONS_PROVIDER = 'room-actions';

/** An error from the host (`code` is its `ActionHostError` code) or the transport. */
export class ActionsError extends Error {
  constructor(code, message) {
    super(message);
    this.name = 'ActionsError';
    this.code = code;
  }
}

const snake = name => name.replace(/[A-Z]/g, (letter, index) => `${index ? '_' : ''}${letter.toLowerCase()}`);
const HOST_CODES = {
  unavailable: 'remote_actions_unavailable',
  confirmation_required: 'action_confirmation_required',
  invalid_request: 'invalid_arguments',
};

function hostError(error) {
  const [variant, detail] = typeof error === 'string' ? [error, {}] : Object.entries(error ?? {})[0] ?? ['Refused', {}];
  const code = HOST_CODES[snake(variant)] ?? snake(variant);
  return new ActionsError(code, detail?.reason ?? code.replaceAll('_', ' '));
}

/** `InvokeArgument` for a JS value: nothing, a prompted text, or a typed object. */
export function invokeArgument(argument) {
  if (argument === undefined || argument === null || argument === 'None') return 'None';
  if (typeof argument === 'string') return {Text: argument};
  if (typeof argument === 'object' && (argument.Text !== undefined || argument.Json !== undefined)) return argument;
  if (typeof argument === 'object' && !Array.isArray(argument)) return {Json: argument};
  throw new TypeError('an action argument is a string or an object');
}

function wireConfirmation(confirmation) {
  if (!confirmation) return undefined;
  if (confirmation.kind === 'phrase') return {Phrase: String(confirmation.phrase ?? '')};
  return 'Confirmed';
}

/** What `descriptor`'s contract asks before it runs. */
export function confirmationFor(descriptor) {
  const policy = descriptor?.contract?.confirmation;
  if (policy?.mode === 'confirm') return {kind: 'confirm'};
  if (policy?.mode === 'phrase') return {kind: 'phrase', phrase: policy.phrase};
  return null;
}

/** A serialized `InvocationUpdate` in the v1 vocabulary. */
function updateOf(update) {
  const [variant, body] = Object.entries(update ?? {})[0] ?? [];
  if (!variant) return null;
  const invocation = body?.invocation;
  switch (variant) {
    case 'Accepted': return {state: 'accepted', invocation};
    case 'Progress': return {state: 'progress', invocation, sequence: String(body.sequence), phase: body.phase,
      completed: body.completed ?? null, total: body.total ?? null, message: body.message ?? null};
    case 'Item': return {state: 'item', invocation, sequence: String(body.sequence), value: body.value};
    case 'Succeeded': return {state: 'succeeded', invocation, output: {value: body.output?.value, resources: body.output?.resources ?? []}};
    case 'Failed': return {state: 'failed', invocation, error: body.error};
    case 'Cancelled': return {state: 'cancelled', invocation, message: body.message ?? null};
    default: return null;
  }
}

/** A receipt status as a v1 update for `invocation`. */
function receiptUpdate(invocation, status) {
  if (status === 'Accepted') return {state: 'accepted', invocation};
  if (status?.Completed) return {state: 'succeeded', invocation, output: {value: status.Completed.output?.value, resources: status.Completed.output?.resources ?? []}};
  if (status?.Failed) return {state: 'failed', invocation, error: status.Failed.error};
  if (status?.Cancelled) return {state: 'cancelled', invocation, message: status.Cancelled.message ?? null};
  if (status?.Rejected) {
    const error = hostError(status.Rejected.error);
    return {state: 'failed', invocation, error: {code: error.code, message: error.message, retryable: false}, rejected: true};
  }
  return null;
}

const TERMINAL = new Set(['succeeded', 'failed', 'cancelled']);

function score(needle, fields) {
  const want = needle.trim().toLowerCase();
  if (!want) return 1;
  let best = 0;
  for (const [field, weight] of fields) {
    const text = String(field ?? '').toLowerCase();
    if (!text) continue;
    if (text === want) best = Math.max(best, 100 * weight);
    else if (text.startsWith(want)) best = Math.max(best, 80 * weight);
    else if (text.split(/[^a-z0-9]+/).some(word => word.startsWith(want))) best = Math.max(best, 60 * weight);
    else if (text.includes(want)) best = Math.max(best, 40 * weight);
  }
  return best;
}

/**
 * @param options.peer           `{sendRemoteAction(room, to, frame), takeRemoteActionEvents()}`
 * @param options.room           room uuid
 * @param options.hostDevice     the host's device key (hex)
 * @param options.device         this page's device key (hex), for the v1 binding
 * @param options.userPublicKey  the user root (hex), for the v1 binding
 * @param options.store          `{get(key), put(key, value)}` keeping recent invocations
 * @param options.now            a monotonic clock (ms) that tells a late reply timer from a silent host
 */
export function createActions({
  peer, room, hostDevice, device = '', userPublicKey = '', store,
  newId = () => globalThis.crypto.randomUUID(),
  wallNow = () => Date.now(),
  now = () => globalThis.performance?.now() ?? Date.now(),
  timeoutMs = DEFAULT_TIMEOUT_MS,
  refreshMs = CATALOGUE_REFRESH_MS,
  setTimer = (callback, delay) => globalThis.setTimeout(callback, delay),
  clearTimer = handle => globalThis.clearTimeout(handle),
}) {
  if (!peer || !room || !hostDevice) throw new TypeError('peer, room and hostDevice are required');
  const historyKey = `tilo.actions.v2:${room}`;
  const pending = new Map(); // `invoke:<id>` | `observe:<id>` | `cancel:<id>` | `catalogue:<n>` -> waiter
  const listeners = new Set();
  const entries = new Map(); // invocation id -> entry, oldest first
  let correlation = 0;
  let disposed = false;
  let catalogueState = {state: 'unavailable', reason: 'not-discovered'};
  let saving = Promise.resolve();

  const loaded = Promise.resolve().then(async () => {
    try {
      const saved = store ? JSON.parse(await store.get(historyKey) ?? '[]') : [];
      const loadedEntries = saved.filter(entry => entry?.invocation && !entries.has(entry.invocation));
      const newer = [...entries.values()];
      entries.clear();
      for (const entry of [...loadedEntries, ...newer]) entries.set(entry.invocation, entry);
      trim();
    } catch {
      // A damaged history starts over; the host still owns every outcome.
    }
    notify();
  });

  function save() {
    if (!store) return saving;
    const kept = [...entries.values()].slice(-HISTORY_LIMIT);
    saving = saving.then(() => store.put(historyKey, JSON.stringify(kept))).catch(() => {});
    return saving;
  }

  function notify() {
    const snapshot = api.v1.getSnapshot();
    for (const listener of listeners) {
      try { listener(snapshot); } catch { /* a listener's failure is its own */ }
    }
  }

  function trim() {
    while (entries.size > HISTORY_LIMIT) entries.delete(entries.keys().next().value);
  }

  function record(entry) {
    entries.delete(entry.invocation);
    entries.set(entry.invocation, entry);
    trim();
    save();
    notify();
    return entry;
  }

  function apply(id, update, extra = {}) {
    const entry = entries.get(id);
    if (!entry || !update) return entry;
    return record({...entry, ...extra, state: update.state, update, terminal: TERMINAL.has(update.state), updatedAt: wallNow()});
  }

  /** Settle every waiter of `key` (one answer answers them all), or just `only`. */
  function settle(key, outcome, only) {
    const waiters = pending.get(key);
    if (!waiters) return;
    const settled = only ? waiters.filter(waiting => waiting === only) : waiters;
    const rest = waiters.filter(waiting => !settled.includes(waiting));
    if (rest.length) pending.set(key, rest); else pending.delete(key);
    for (const waiting of settled) {
      clearTimer(waiting.timer);
      outcome(waiting);
    }
  }

  function request(key, message) {
    const frame = JSON.stringify({version: REMOTE_ACTIONS_VERSION, message});
    return new Promise((resolve, reject) => {
      if (disposed) { reject(new ActionsError('closed', 'the peer was disposed')); return; }
      const waiting = {resolve, reject};
      // The host has `timeoutMs` to answer while this page can listen. A
      // timer that fires late ran behind a stall of the page: the answer may
      // have arrived meanwhile and still be queued behind that stall, so the
      // page first gets back the time it lost, bounded by one more timeout.
      let due = now() + timeoutMs;
      let extended = 0;
      const expire = () => {
        const late = now() - due;
        if (late > LATE_TIMER_MS && extended < timeoutMs) {
          const grace = Math.min(timeoutMs - extended, Math.max(late, STALL_GRACE_MS));
          extended += grace;
          due = now() + grace;
          waiting.timer = setTimer(expire, grace);
          return;
        }
        settle(key, ({reject: fail}) => fail(new ActionsError('timeout', 'the host did not answer')), waiting);
      };
      waiting.timer = setTimer(expire, timeoutMs);
      pending.set(key, [...(pending.get(key) ?? []), waiting]);
      try {
        peer.sendRemoteAction(room, hostDevice, frame);
      } catch (error) {
        settle(key, ({reject: fail}) => fail(error), waiting);
      }
    });
  }

  /** Take this batch of the session's remote-action events. */
  function pump() {
    for (const event of JSON.parse(peer.takeRemoteActionEvents())) {
      if (event.kind === 'undeliverable') {
        if (event.to !== hostDevice) continue;
        for (const key of [...pending.keys()]) settle(key, ({reject}) => reject(new ActionsError('undeliverable', event.reason)));
        continue;
      }
      if (event.kind !== 'remoteAction' || event.from !== hostDevice) continue;
      let frame;
      try { frame = JSON.parse(event.frame); } catch { continue; }
      if (frame.version !== REMOTE_ACTIONS_VERSION) continue;
      const message = frame.message ?? {};
      if (message.CatalogueResponse) {
        const {correlation: got, result} = message.CatalogueResponse;
        settle(`catalogue:${got}`, ({resolve, reject}) => result?.Ok ? resolve(result.Ok) : reject(hostError(result?.Err)));
      } else if (message.Receipt) {
        const {invocation, status} = message.Receipt;
        settle(`invoke:${invocation}`, ({resolve}) => resolve(status));
      } else if (message.Update) {
        const {invocation, state} = message.Update;
        settle(`observe:${invocation}`, ({resolve}) => resolve(state));
      } else if (message.CancelResponse) {
        const {invocation, result} = message.CancelResponse;
        settle(`cancel:${invocation}`, ({resolve, reject}) => result && 'Ok' in result ? resolve() : reject(hostError(result?.Err)));
      }
    }
  }

  async function catalogue(query = {}) {
    correlation += 1;
    const page = await request(`catalogue:${correlation}`, {CatalogueRequest: {correlation, query: {max_results: 200, ...query}}});
    if (!query.provider && !query.needle) {
      catalogueState = {state: 'ready', page};
      notify();
    }
    return page;
  }

  function send(entry) {
    const message = {Invoke: {
      invocation: entry.invocation,
      target: entry.target,
      argument: entry.argument,
      ...(entry.confirmation ? {confirmation: wireConfirmation(entry.confirmation)} : {}),
    }};
    return request(`invoke:${entry.invocation}`, message).then(status => {
      const update = receiptUpdate(entry.invocation, status);
      const next = apply(entry.invocation, update, update?.rejected ? {reason: update.error.code} : {});
      if (update?.rejected) throw Object.assign(hostError(status.Rejected.error), {entry: next});
      return next;
    }, error => {
      // The outcome is unknown: the host may have run it. Keep the id so a
      // retry observes the original instead of running it again.
      apply(entry.invocation, {state: 'unknown', invocation: entry.invocation}, {reason: error.code ?? 'unknown'});
      throw error;
    });
  }

  function ticket(entry, done) {
    return {
      id: entry.invocation,
      get entry() { return entries.get(entry.invocation); },
      done,
      status: () => status(entry.invocation),
      cancel: () => cancel(entry.invocation),
    };
  }

  /** Run one action; see the module comment. */
  function invoke(target, argument, {confirmation = null, invocation, label, resourceLabel, appId} = {}) {
    if (!target?.provider || !target?.address?.resource || !target?.address?.action) {
      throw new TypeError('target is {provider, address: {resource, action}} copied from a catalogue row');
    }
    const id = invocation ?? newId();
    const existing = entries.get(id);
    if (existing && !existing.terminal) return ticket(existing, send(existing));
    if (existing) return ticket(existing, Promise.resolve(existing));
    const entry = record({
      invocation: id, room, host: hostDevice,
      target: {provider: target.provider, address: {resource: target.address.resource, action: target.address.action}},
      argument: invokeArgument(argument), confirmation,
      label: label ?? null, resourceLabel: resourceLabel ?? null, appId: appId ?? null,
      state: 'unknown', reason: null, update: null, terminal: false, cancellationRequested: false,
      submittedAt: wallNow(), updatedAt: wallNow(),
    });
    return ticket(entry, saving.then(() => send(entry)));
  }

  async function status(id) {
    const entry = entries.get(id);
    if (!entry) throw new ActionsError('unknown_invocation', 'no such invocation in this page');
    const state = await request(`observe:${id}`, {Observe: {invocation: id}});
    if (state) return apply(id, updateOf(state.update));
    // The host never admitted it (lost on the way, or refused and
    // forgotten): send it again under the same id.
    if (entries.get(id)?.state === 'unknown') return send(entries.get(id)).catch(() => entries.get(id));
    return entries.get(id);
  }

  async function cancel(id) {
    const entry = entries.get(id);
    if (!entry) throw new ActionsError('unknown_invocation', 'no such invocation in this page');
    if (entry.terminal) return entry;
    await request(`cancel:${id}`, {Cancel: {invocation: id}});
    record({...entries.get(id), cancellationRequested: true});
    return status(id);
  }

  async function recover() {
    await loaded;
    for (const entry of [...entries.values()]) {
      if (!entry.terminal) await status(entry.invocation).catch(() => {});
    }
    return api.v1.getSnapshot();
  }

  // --- actionsV1: the frozen contract Cloud reads -----------------------

  const binding = () => ({room, sender: device, senderRoot: userPublicKey, target: hostDevice, targetRoot: userPublicKey, certificate: hostDevice});

  function surfaces(page) {
    const byProvider = new Map();
    for (const row of page.rows ?? []) {
      if (!row.action) continue;
      const provider = page.providers?.find(candidate => candidate.provider === row.provider);
      const surface = byProvider.get(row.provider) ?? {peer_id: row.provider, app_id: row.app_id, cluster_id: room,
        record_id: provider?.record_id ?? '', actions: []};
      const {address, ...descriptor} = row.action;
      surface.actions.push({...descriptor, resource: address.resource, action: address.action, revision: null, label: row.action.action_label});
      byProvider.set(row.provider, surface);
    }
    return [...byProvider.values()];
  }

  function capability() {
    if (catalogueState.state !== 'ready') return catalogueState;
    return {state: 'ready', host: hostDevice, binding: binding(), catalogue: {version: 1, surfaces: surfaces(catalogueState.page)}};
  }

  function privateInvocation(entry) {
    const [appId, recordId] = entry.target.provider.startsWith('record:')
      ? (([app, ...rest]) => [app, rest.join(':')])(entry.target.provider.slice('record:'.length).split(':'))
      : [entry.appId ?? '', ''];
    const argument = entry.argument === 'None' ? null : entry.argument.Text ?? entry.argument.Json ?? null;
    return {
      invocation: entry.invocation, host: entry.host, binding: binding(),
      intent: {type: 'invoke_bound', host: entry.host, invocation: entry.invocation, app_peer: entry.target.provider,
        app_id: appId, record_id: recordId, resource: entry.target.address.resource, action: entry.target.address.action,
        revision: null, argument, confirmation: entry.confirmation ?? null},
      state: entry.state, reason: entry.reason ?? null, update: entry.update ?? null,
      terminal: entry.terminal, cancellationRequested: !!entry.cancellationRequested,
      label: entry.label, resourceLabel: entry.resourceLabel,
    };
  }

  // The catalogue is discovered while someone watches the capability, and
  // refreshed as records open and change.
  let discovery;
  function discover() {
    if (disposed || !listeners.size || discovery) return;
    discovery = catalogue().catch(error => {
      if (catalogueState.state !== 'ready') {
        catalogueState = {state: 'unavailable', reason: error.code ?? 'unavailable'};
        notify();
      }
    }).finally(() => {
      discovery = disposed ? undefined : setTimer(() => { discovery = undefined; discover(); }, refreshMs);
    });
  }

  const v1 = {
    version: 1,
    getSnapshot: () => ({version: 1, capability: capability(), entries: [...entries.values()].map(privateInvocation)}),
    subscribe(listener) {
      listeners.add(listener);
      discover();
      return () => { listeners.delete(listener); };
    },
    async catalogue() {
      await catalogue();
      const ready = capability();
      if (ready.state !== 'ready') throw new ActionsError('remote_actions_unavailable', ready.reason);
      return ready;
    },
    async invoke(intent) {
      const surface = catalogueState.state === 'ready'
        ? surfaces(catalogueState.page).find(candidate => candidate.peer_id === intent.app_peer) : undefined;
      const descriptor = surface?.actions.find(action => action.resource === intent.resource && action.action === intent.action);
      const started = invoke({provider: intent.app_peer, address: {resource: intent.resource, action: intent.action}},
        intent.argument, {confirmation: intent.confirmation ?? null, invocation: intent.invocation,
          label: descriptor?.action_label, resourceLabel: descriptor?.resource_label, appId: intent.app_id});
      const entry = await started.done.catch(error => error.entry ?? entries.get(started.id));
      return privateInvocation(entry ?? entries.get(started.id));
    },
    status: async id => privateInvocation(await status(id)),
    cancel: async id => privateInvocation(await cancel(id)),
    recover,
  };

  // --- search palette provider ---------------------------------------------

  const provider = {
    id: ROOM_ACTIONS_PROVIDER,
    /** Hits for `needle`: one per action, `target.kind` `action`. */
    async query(needle = '') {
      if (!needle.trim()) return [];
      const page = await catalogue({needle, max_results: PALETTE_ROWS});
      return (page.rows ?? []).filter(row => row.action).map(row => ({
        kind: 'Action',
        provider: ROOM_ACTIONS_PROVIDER,
        id: `${row.provider}#${row.action.address.resource}#${row.action.address.action}`,
        title: row.action.action_label,
        subtitle: row.action.resource_label || row.resource?.label || row.app_id,
        score: score(needle, [[row.action.action_label, 1], [row.action.resource_label, 0.6], [row.app_id, 0.3]]),
        target: {kind: 'action', provider: row.provider, address: row.action.address,
          appId: row.app_id, confirmation: confirmationFor(row.action), descriptor: row.action},
      })).filter(hit => hit.score > 0).sort((a, b) => b.score - a.score);
    },
    /** Run a hit's action; `confirmation` must be given when `hit.target.confirmation` asks for one. */
    choose(hit, {argument, confirmation = null} = {}) {
      const target = hit?.target;
      if (target?.kind !== 'action') throw new TypeError('not an action hit');
      if (target.confirmation && !confirmation) throw new ActionsError('action_confirmation_required', 'confirm before running this action');
      return invoke({provider: target.provider, address: target.address}, argument,
        {confirmation, label: hit.title, resourceLabel: hit.subtitle, appId: target.appId});
    },
  };

  const api = {
    catalogue,
    invoke,
    status,
    cancel,
    recover,
    history: () => [...entries.values()],
    confirmation: confirmationFor,
    subscribe: v1.subscribe,
    provider,
    v1,
    loaded,
    pump,
    dispose() {
      disposed = true;
      if (discovery && typeof discovery.then !== 'function') clearTimer(discovery);
      for (const key of [...pending.keys()]) settle(key, ({reject}) => reject(new ActionsError('closed', 'the peer was disposed')));
      listeners.clear();
    },
  };
  return api;
}
