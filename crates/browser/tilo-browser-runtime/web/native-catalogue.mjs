/** Native app navigation uses the same certified, permission-filtered catalogue
 * as the page. No local app reply substitutes for that host's current Read.
 *
 * An app may also invoke the room's own membership actions (`remove-member`,
 * `ban`, `unban` on the room's governance resource, provider
 * `tilo.workspace`): they become the peer's `/rooms` requests
 * (`membership()`, see `membership.mjs`), which the host decides by the
 * room's policy. The answer is the invocation's receipt. It may also ask
 * the room's host for a record resource's `share-link` (`peer.actions`).
 * The page itself takes the same membership actions through the mount's
 * door (`invokeRoomMembership`). */
const GOVERNANCE_PROVIDER = 'tilo.workspace';
const MEMBERSHIP = {'remove-member': 'remove', ban: 'ban', unban: 'unban'};
/** The membership request an action names, if it is one (never an inherited name). */
const membershipVerb = action => (typeof action === 'string' && Object.hasOwn(MEMBERSHIP, action) ? MEMBERSHIP[action] : undefined);
/** Taking someone out of the room: run only on the person's confirmation. */
const DESTRUCTIVE = new Set(['remove-member', 'ban']);
const SHARE_LINK = 'share-link';

/** The id of `room`'s governance resource (`room_governance_resource`). */
export const roomGovernanceResource = room => `workspace.permissions:${room}`;

const refusal = (code, reason) => Promise.reject(new Error(`${code}: ${reason}`));

/**
 * The room's membership actions through a mount's own action door
 * (`invokeAction`), as a script, an agent or assistive technology takes
 * them on the page: `remove-member`, `ban` and `unban` on `room`'s
 * governance resource, each `{person}` (the person's root key, hex). They
 * become the same `/rooms` requests an app's do (`membership`, see
 * `membership.mjs`), which the host decides by the room's policy; removing
 * and banning run only with `confirm`, as the door runs any action whose
 * contract asks first.
 *
 * `undefined` when `resource` and `action` name no such action of this
 * room, so the application's door answers them; else a promise of the
 * `ActionResult` (`{state: 'completed', output: {value: {room, members}}}`
 * once the host applied it), rejected `code: reason` as the door's errors
 * read when the host refused it.
 */
export function invokeRoomMembership({membership, room, resource, action, args = {}, confirm = false}) {
  const verb = membershipVerb(action);
  if (!verb || !room || resource !== roomGovernanceResource(room)) return undefined;
  if (!membership) return refusal('action_unavailable', 'this page holds no membership in the room');
  const person = args?.person;
  if (typeof person !== 'string' || !/^[0-9a-f]{64}$/.test(person)) {
    return refusal('invalid_arguments', 'person must be 64 lowercase hex characters');
  }
  if (Object.keys(args).some(name => name !== 'person')) return refusal('invalid_arguments', 'the only argument is person');
  if (DESTRUCTIVE.has(action) && !confirm) {
    return refusal('action_confirmation_required', `${action} takes ${person} out of the room; invoke it with confirm`);
  }
  return Promise.resolve().then(() => membership[verb](person)).then(
    changed => ({state: 'completed', output: {value: {room: changed?.room ?? room, members: changed?.members ?? []}}}),
    error => refusal('action_failed', String(error?.message ?? error)),
  );
}

export function createNativeCatalogueBridge({take, deliver, actions, membership = () => undefined, current, onError = () => {}}) {
  let closed = false, scheduled = false;
  const pending = new Set();
  const invoking = new Set();
  const active = () => !closed && current();
  const unavailable = reason => ({Err:{Unavailable:{reason}}});
  const send = message => {
    if (active()) {
      try { deliver(message); }
      catch (error) { onError(error); }
    }
  };
  const answer = (correlation, result) => send({CatalogueResponse:{correlation, result}});
  const receipt = (invocation, status) => send({Receipt:{invocation, status}});
  const rejected = (invocation, reason) => receipt(invocation, {Rejected:{error:{Unavailable:{reason}}}});
  // `share-link` reads where a resource is shown and grants nothing, so an
  // app may ask the room's host for it as the person (a chat message
  // carrying a page). The host decides by the person's own grant.
  function shareLink(invocation, target, argument) {
    const connection = actions();
    if (!connection || invoking.size >= 4) {
      rejected(invocation, 'Room actions unavailable or busy');
      return;
    }
    const key = JSON.stringify(invocation);
    if (invoking.has(key)) return;
    invoking.add(key);
    Promise.resolve().then(async () => {
      const ticket = connection.invoke({provider: target.provider, address: target.address}, argument ?? 'None',
        {invocation: typeof invocation === 'string' ? invocation : undefined});
      let entry = await ticket.done;
      for (let polls = 0; entry && !entry.terminal && polls < 20; polls++) entry = await ticket.status();
      const update = entry?.update;
      if (update?.state === 'succeeded') receipt(invocation, {Completed:{output:update.output}});
      else if (update?.state === 'failed') receipt(invocation, {Failed:{error:update.error}});
      else rejected(invocation, entry?.reason ?? 'The room\'s host did not answer');
    }).catch(error => rejected(invocation, String(error?.message ?? error)))
      .finally(() => invoking.delete(key));
  }
  function invoke(request) {
    const {invocation, target, argument} = request;
    if (target?.address?.action === SHARE_LINK && target.provider?.startsWith('record:')) {
      shareLink(invocation, target, argument);
      return;
    }
    const verb = membershipVerb(target?.address?.action);
    if (target?.provider !== GOVERNANCE_PROVIDER || !verb) {
      rejected(invocation, 'Only the room\'s membership actions run from an app here');
      return;
    }
    const person = argument?.Json?.person;
    const rooms = membership();
    if (!rooms || invoking.size >= 4) {
      rejected(invocation, 'Room membership unavailable or busy');
      return;
    }
    const key = JSON.stringify(invocation);
    if (invoking.has(key)) return;
    invoking.add(key);
    Promise.resolve().then(() => rooms[verb](person)).then(
      () => receipt(invocation, {Completed:{output:{}}}),
      error => receipt(invocation, {Failed:{error:{code:'refused', message:String(error?.message ?? error), retryable:false}}}),
    ).finally(() => invoking.delete(key));
  }
  function pump() {
    if (!active() || scheduled) return;
    scheduled = true;
    // Runtime status callbacks hold the mount borrow; enter after they return.
    queueMicrotask(() => {
      scheduled = false;
      if (!active()) return;
      let requests;
      try { requests = take(); } catch (error) { onError(error); return; }
      for (const message of requests) {
        if (message.Invoke) { invoke(message.Invoke); continue; }
        const request = message.CatalogueRequest;
        if (!request || !Number.isSafeInteger(request.correlation)) continue;
        const {correlation, query} = request;
        if (pending.has(correlation)) continue;
        const connection = actions();
        if (!connection || pending.size >= 16) {
          answer(correlation, unavailable('Certified catalogue unavailable or busy'));
          continue;
        }
        pending.add(correlation);
        Promise.resolve().then(() => connection.catalogue(query)).then(page => {
          answer(correlation, actions() === connection ? {Ok:page}
            : unavailable('Certified catalogue connection changed'));
        }, error => answer(correlation, unavailable(String(error?.message ?? error))))
          .finally(() => pending.delete(correlation));
      }
    });
  }
  return {pump, dispose(){closed = true;}};
}
