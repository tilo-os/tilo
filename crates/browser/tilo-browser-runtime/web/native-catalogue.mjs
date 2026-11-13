/** Native app navigation uses the same certified, permission-filtered catalogue
 * as the page. No local app reply substitutes for that host's current Read.
 *
 * An app may also invoke the workspace's own membership actions (`remove-member`,
 * `ban`, `unban` on the workspace's governance resource, provider
 * `tilo.workspace`): they become the peer's `/rooms` requests
 * (`membership()`, see `peer.mjs`), which the host decides by the
 * workspace's policy. The answer is the invocation's receipt. Any other action
 * the workspace's host runs (a workspace record's, `record:<app>:<record>`, or the
 * workspace's own, `workspace:<workspace>`) goes to the host over the peer's carrier
 * (`peer.actions`), and the host admits it by the person's own standing:
 * the page grants nothing itself. The page itself takes the same membership actions through the mount's
 * door (`invokeMembershipAction`). */
const GOVERNANCE_PROVIDER = 'tilo.workspace';
const MEMBERSHIP = {'remove-member': 'remove', ban: 'ban', unban: 'unban'};
/** The membership request an action names, if it is one (never an inherited name). */
const membershipVerb = action => (typeof action === 'string' && Object.hasOwn(MEMBERSHIP, action) ? MEMBERSHIP[action] : undefined);
/** Taking someone out of the workspace: run only on the person's confirmation. */
const DESTRUCTIVE = new Set(['remove-member', 'ban']);
/** Whether the workspace's host runs `provider`'s actions (beside its governance). */
/** The person's answer to an action that asks first, as `peer.actions` takes it. */
const confirmed = confirmation => (confirmation === 'Confirmed' ? {kind: 'confirm'}
  : typeof confirmation?.Phrase === 'string' ? {kind: 'phrase', phrase: confirmation.Phrase} : null);
const hostRuns = provider => typeof provider === 'string' && (provider.startsWith('record:') || provider.startsWith('workspace:'));

/** The id of `workspace`'s governance resource (`workspace_governance_resource`). */
export const governanceResource = workspace => `workspace.permissions:${workspace}`;

const refusal = (code, reason) => Promise.reject(new Error(`${code}: ${reason}`));

/**
 * The workspace's membership actions through a mount's own action door
 * (`invokeAction`), as a script, an agent or assistive technology takes
 * them on the page: `remove-member`, `ban` and `unban` on `workspace`'s
 * governance resource, each `{person}` (the person's root key, hex). They
 * become the same `/rooms` requests an app's do (`membership`, see
 * `peer.mjs`), which the host decides by the workspace's policy; removing
 * and banning run only with `confirm`, as the door runs any action whose
 * contract asks first.
 *
 * `undefined` when `resource` and `action` name no such action of this
 * workspace, so the application's door answers them; else a promise of the
 * `ActionResult` (`{state: 'completed', output: {value: {room, members}}}`
 * once the host applied it), rejected `code: reason` as the door's errors
 * read when the host refused it.
 */
export function invokeMembershipAction({membership, workspace, resource, action, args = {}, confirm = false}) {
  const verb = membershipVerb(action);
  if (!verb || !workspace || resource !== governanceResource(workspace)) return undefined;
  if (!membership) return refusal('action_unavailable', 'this page holds no membership in the workspace');
  const person = args?.person;
  if (typeof person !== 'string' || !/^[0-9a-f]{64}$/.test(person)) {
    return refusal('invalid_arguments', 'person must be 64 lowercase hex characters');
  }
  if (Object.keys(args).some(name => name !== 'person')) return refusal('invalid_arguments', 'the only argument is person');
  if (DESTRUCTIVE.has(action) && !confirm) {
    return refusal('action_confirmation_required', `${action} takes ${person} out of the workspace; invoke it with confirm`);
  }
  return Promise.resolve().then(() => membership[verb](person)).then(
    changed => ({state: 'completed', output: {value: {room: changed?.room ?? workspace, members: changed?.members ?? []}}}),
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
  // The workspace's host runs the action as itself for the person it verified
  // on the carrier, deciding by their own standing (a chat message asking
  // for a page's `share-link`, a record's action from another app).
  function hostRun({invocation, target, argument, confirmation}) {
    const connection = actions();
    if (!connection || invoking.size >= 4) {
      rejected(invocation, 'Workspace actions unavailable or busy');
      return;
    }
    const key = JSON.stringify(invocation);
    if (invoking.has(key)) return;
    invoking.add(key);
    Promise.resolve().then(async () => {
      const ticket = connection.invoke({provider: target.provider, address: target.address}, argument ?? 'None',
        {invocation: typeof invocation === 'string' ? invocation : undefined, confirmation: confirmed(confirmation)});
      let entry = await ticket.done;
      for (let polls = 0; entry && !entry.terminal && polls < 20; polls++) entry = await ticket.status();
      const update = entry?.update;
      if (update?.state === 'succeeded') receipt(invocation, {Completed:{output:update.output}});
      else if (update?.state === 'failed') receipt(invocation, {Failed:{error:update.error}});
      else rejected(invocation, entry?.reason ?? 'The workspace\'s host did not answer');
    }).catch(error => rejected(invocation, String(error?.message ?? error)))
      .finally(() => invoking.delete(key));
  }
  function invoke(request) {
    const {invocation, target, argument} = request;
    if (hostRuns(target?.provider)) {
      hostRun(request);
      return;
    }
    const verb = membershipVerb(target?.address?.action);
    if (target?.provider !== GOVERNANCE_PROVIDER || !verb) {
      rejected(invocation, 'Only the workspace\'s host runs another app\'s actions here');
      return;
    }
    const person = argument?.Json?.person;
    const governed = membership();
    if (!governed || invoking.size >= 4) {
      rejected(invocation, 'Workspace membership unavailable or busy');
      return;
    }
    const key = JSON.stringify(invocation);
    if (invoking.has(key)) return;
    invoking.add(key);
    Promise.resolve().then(() => governed[verb](person)).then(
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
