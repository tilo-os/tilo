/**
 * `peer.agents`: the agents working in this room, seen and steered from the
 * page.
 *
 * Agents are participants of the room like this page: each is a device of
 * the user with a root-signed delegation (label, cursor colour, rooms,
 * apps). The host supervises them and answers the agent vocabulary of
 * `tilo_relay_protocol::agents` as `tilo_actions` remote actions addressed
 * to its own device — the same frames `actionsV1` uses for apps:
 *
 * - `status()` → `AgentsStatus` `{version, agents: AgentInfo[], grants}`;
 * - `requestGrant({label, rooms, apps, expiresInSecs, color?, mayConfirm?})`
 *   → `{grant, pairingCode}`: the host asks the user's approving device,
 *   which approves the agent and signs its delegation in one step; the
 *   grant's `state` in `status()` goes `pending` → `approved` (or `denied`);
 * - `spawn({grant, task, room?, id?})` → `{agentId, taskId}` and
 *   `message(agentId, text, {id?})` → `{taskId}`. `id` (a UUID) is the
 *   caller's idempotency id: it names the invocation, so a call resent with
 *   the same id after a timeout or a reload gets the host's first answer
 *   back while the host retains its receipt (the last 1024 invocations,
 *   shared across callers, including status polls, until host restart). This is a recent retry
 *   window, not a durable exactly-once guarantee. A page binds up to 1024
 *   explicit ids until disposal; conflicting reuse is refused even after a
 *   timeout, and new ids beyond that limit return `invocation_capacity`.
 *   Without `id` every call is new;
 * - `cancelTask(agentId, taskId)` marks that task cancelled: queued tasks
 *   are skipped and running tasks stop at the next task-state check. An
 *   already-started step may finish and publish changes after the receipt;
 *   planning and accepted app operations are not interrupted. The receipt
 *   acknowledges cancellation state, not execution quiescence. The agent
 *   goes on with the next task; `pause`, `resume`, `stop(agentId)`
 *   (every task, and the agent takes no more); `revoke(grantId)` withdraws
 *   a grant (its agent stops and the host refuses its device).
 *
 * `actions(agentDevice)` is `actionsV1` for the apps an agent has open,
 * over the same frames addressed to the agent's device:
 *
 * - `catalogue(query)` → `CataloguePage`: one provider per open record
 *   (`record:<app>:<record>`) with every action it publishes, declared and
 *   projected controls, and the room's provider offering `app.<app>#open`;
 * - `invoke(target, argument, {confirm})` → `ActionOutput`, where `target`
 *   is `{provider, address}` copied from a row.
 *
 * An agent's edits arrive as ordinary durable changes; its cursor arrives on
 * the live plane as presence whose `participant.kind` is `agent`
 * (`cursors()`). `verify(info)` checks an agent's delegation against the
 * user root in wasm before the page calls it "yours".
 */

export const AGENTS_PROVIDER = 'agents';
export const SUPERVISOR_RESOURCE = 'agents';
export const agentResource = agentId => `agent:${agentId}`;
const REMOTE_ACTIONS_VERSION = 1;
const DEFAULT_TIMEOUT_MS = 30_000;
const MAX_INVOCATION_BINDINGS = 1024;
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

/** An error from the host: `code` is `refused`, `unknown_action`, … or `timeout`/`undeliverable`. */
export class AgentsError extends Error {
  constructor(code, message) {
    super(message);
    this.name = 'AgentsError';
    this.code = code;
  }
}

function hostError(error) {
  if (typeof error === 'string') {
    const code = error.replace(/[A-Z]/g, (letter, index) => `${index ? '_' : ''}${letter.toLowerCase()}`);
    return new AgentsError(code, code.replaceAll('_', ' '));
  }
  const [variant, detail] = Object.entries(error ?? {})[0] ?? ['Refused', {}];
  return new AgentsError(variant.replace(/[A-Z]/g, (letter, index) => `${index ? '_' : ''}${letter.toLowerCase()}`),
    detail?.reason ?? variant);
}

/**
 * @param options.peer           the wasm `WasmPeer` (`sendRemoteAction`, `takeRemoteActionEvents`)
 * @param options.runtime        the wasm module (`verifyAgentDelegation`)
 * @param options.room           room uuid
 * @param options.hostDevice     the host's device key (hex): `host.hostCert.device_public_key`
 * @param options.userPublicKey  the user root (hex) delegations must verify against
 * @param options.live           the room's live layer, for `cursors()`
 */
export function createAgents({
  peer, runtime, room, hostDevice, userPublicKey, live,
  newId = () => globalThis.crypto.randomUUID(),
  wallNow = () => Date.now(),
  timeoutMs = DEFAULT_TIMEOUT_MS,
  setTimer = (callback, delay) => globalThis.setTimeout(callback, delay),
  clearTimer = handle => globalThis.clearTimeout(handle),
}) {
  if (!peer || !room || !hostDevice) throw new TypeError('peer, room and hostDevice are required');
  const pending = new Map(); // invocation id or `catalogue:<n>` -> {resolve, reject, timer, to}
  let correlation = 0;
  let closed = false;
  // Explicit caller ids stay bound after timeout so a delayed receipt cannot
  // answer another request. Bound memory by refusing new ids, never eviction.
  const bindings = new Map();

  function settle(invocation, outcome) {
    const waiting = pending.get(invocation);
    if (!waiting) return;
    pending.delete(invocation);
    clearTimer(waiting.timer);
    outcome(waiting);
  }

  /** Take the session's remote-action events; call on every peer change. */
  function pump() {
    if (closed) return;
    for (const event of JSON.parse(peer.takeRemoteActionEvents())) {
      if (event.kind === 'undeliverable') {
        for (const [key, waiting] of [...pending]) {
          if (waiting.to === event.to) settle(key, ({reject}) => reject(new AgentsError('undeliverable', event.reason)));
        }
        continue;
      }
      if (event.kind !== 'remoteAction') continue;
      let frame;
      try { frame = JSON.parse(event.frame); } catch { continue; }
      if (frame.version !== REMOTE_ACTIONS_VERSION) continue;
      const catalogue = frame.message?.CatalogueResponse;
      if (catalogue) {
        const key = `catalogue:${catalogue.correlation}`;
        if (pending.get(key)?.to !== event.from) continue;
        settle(key, ({resolve, reject}) => {
          if (catalogue.result?.Ok) resolve(catalogue.result.Ok);
          else reject(hostError(catalogue.result?.Err));
        });
        continue;
      }
      const receipt = frame.message?.Receipt;
      if (!receipt || pending.get(receipt.invocation)?.to !== event.from) continue;
      const status = receipt.status;
      settle(receipt.invocation, ({resolve, reject, whole}) => {
        if (status?.Completed) resolve(whole ? status.Completed.output : status.Completed.output?.value ?? null);
        else if (status?.Rejected) reject(hostError(status.Rejected.error));
        else if (status?.Failed) reject(new AgentsError(status.Failed.error.code, status.Failed.error.message));
        else if (status?.Cancelled) reject(new AgentsError('cancelled', status.Cancelled.message ?? 'cancelled'));
        else resolve(null);
      });
    }
  }

  function request(to, key, message, extra = {}) {
    if (closed) return Promise.reject(new AgentsError('closed', 'the peer was disposed'));
    const frame = JSON.stringify({version: REMOTE_ACTIONS_VERSION, message});
    let entry;
    const promise = new Promise((resolve, reject) => {
      const timer = setTimer(() => settle(key, waiting => waiting.reject(
        new AgentsError('timeout', 'the device did not answer'))), timeoutMs);
      entry = {resolve, reject, timer, to, ...extra};
      pending.set(key, entry);
    });
    entry.promise = promise;
    try {
      peer.sendRemoteAction(room, to, frame);
    } catch (error) {
      settle(key, waiting => waiting.reject(error));
    }
    return promise;
  }

  /** `id`: the caller's idempotency id; a call already waiting under it is joined, not resent. */
  function invokeOn(to, target, argument = 'None', {confirm = false, whole = false, id} = {}) {
    if (closed) return Promise.reject(new AgentsError('closed', 'the peer was disposed'));
    if (id !== undefined && (typeof id !== 'string' || !UUID.test(id))) {
      return Promise.reject(new TypeError('id must be a UUID'));
    }
    const invocation = id?.toLowerCase() ?? newId();
    const message = {Invoke: {invocation, target, argument, ...(confirm ? {confirmation: 'Confirmed'} : {})}};
    const signature = JSON.stringify([to, message, whole]);
    if (id !== undefined) {
      const bound = bindings.get(invocation);
      if (bound !== undefined && bound !== signature) return Promise.reject(new AgentsError('invocation_conflict', 'an invocation id already names another request'));
      if (bound === undefined && bindings.size >= MAX_INVOCATION_BINDINGS) return Promise.reject(new AgentsError('invocation_capacity', 'the peer has reached its caller invocation limit'));
      bindings.set(invocation, signature);
    }
    const waiting = pending.get(invocation);
    if (waiting) {
      if (waiting.signature !== signature) return Promise.reject(new AgentsError('invocation_conflict', 'an invocation id already names another request'));
      return waiting.promise;
    }
    return request(to, invocation, message, {whole, signature});
  }

  function invoke(resource, action, argument = 'None', options = {}) {
    return invokeOn(hostDevice, {provider: AGENTS_PROVIDER, address: {resource, action}}, argument, options);
  }

  /** `actionsV1` for the apps the agent at `device` (hex) has open. */
  function actions(device) {
    return {
      catalogue(query = {}) {
        correlation += 1;
        const key = `catalogue:${correlation}`;
        return request(device, key, {CatalogueRequest: {correlation, query: {max_results: 200, ...query}}});
      },
      invoke(target, argument = 'None', options = {}) {
        return invokeOn(device, target, argument, {...options, whole: true});
      },
    };
  }

  /** The delegation `info.delegation` grants, verified against the user root, or `null`. */
  function verify(info) {
    if (!userPublicKey || !runtime?.verifyAgentDelegation) return null;
    try {
      const granted = JSON.parse(runtime.verifyAgentDelegation(info.delegation, userPublicKey, wallNow()));
      return granted.device === info.device && granted.agentId === info.id ? granted : null;
    } catch {
      return null;
    }
  }

  return {
    status: () => invoke(SUPERVISOR_RESOURCE, 'status'),
    requestGrant: ({label, rooms, apps = [], expiresInSecs, color, mayConfirm = false}) =>
      invoke(SUPERVISOR_RESOURCE, 'request-grant', {Json: {label, rooms, apps, expiresInSecs, mayConfirm, ...(color ? {color} : {})}}),
    spawn: ({grant, task, room, id}) => invoke(SUPERVISOR_RESOURCE, 'spawn', {Json: {grant, task, ...(room ? {room} : {})}}, {id}),
    message: (agentId, text, {id} = {}) => invoke(agentResource(agentId), 'message', {Text: String(text)}, {id}),
    cancelTask: (agentId, taskId) => {
      if (typeof taskId !== 'string' || !taskId) return Promise.reject(new TypeError('taskId must name a task'));
      return invoke(agentResource(agentId), 'cancel-task', {Text: taskId}).then(() => undefined);
    },
    pause: agentId => invoke(agentResource(agentId), 'pause').then(() => undefined),
    resume: agentId => invoke(agentResource(agentId), 'resume').then(() => undefined),
    stop: agentId => invoke(agentResource(agentId), 'stop').then(() => undefined),
    revoke: grantId => invoke(agentResource(grantId), 'revoke').then(() => undefined),
    verify,
    actions,
    /**
     * Every agent cursor in the room now: the live layer's interpolated
     * remote state of publishers whose presence says `agent`.
     */
    cursors(nowMs) {
      if (!live) return [];
      return live.remoteState(nowMs)
        .filter(state => state.participant?.kind === 'agent')
        .map(state => ({
          device: state.publisher,
          label: state.participant.label,
          color: state.participant.color,
          activity: state.participant.activity,
          focus: state.focus,
          pointer: state.pointer,
          selection: state.selection,
          stale: state.stale,
        }));
    },
    pump,
    dispose() {
      closed = true;
      bindings.clear();
      for (const invocation of [...pending.keys()]) {
        settle(invocation, ({reject}) => reject(new AgentsError('closed', 'the peer was disposed')));
      }
    },
  };
}
