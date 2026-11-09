/**
 * The person's own workspace on the host this page came from: one per
 * person, made the first time they ask and answered again every time after,
 * kept by a host that keeps workspaces of people's own (`tilo-relay room
 * --own-workspaces`).
 *
 * `ensureOwnWorkspace(page)` asks the host through the peer the page holds
 * there (`page.peer`, in the site's home workspace): the host's own operation
 * `room.create-own` (provider `tilo.host`) makes the workspace, or finds the
 * one the person has, and answers an invitation only the person's devices
 * can redeem. The page joins with it as any invitation is joined
 * (`joinWorkspace`), becoming the workspace's administrator as it is
 * admitted, and remembers it among the workspaces it entered, so the
 * desktop's switcher offers it and a bare entry can reopen it. A workspace
 * this browser already entered is not joined again.
 *
 * Resolves to `{id, name, host, origin, peer}`: the workspace's id (a UUID,
 * what a link and every action name it by), its name, the host's name as a
 * link shows it (`tilo.sh`), its origin, and the peer in it when the page
 * joined just now (the caller opens it or disposes it), else `null`.
 *
 * A host that keeps no workspaces of people's own offers no such operation:
 * that, and every other refusal, rejects with an `OwnWorkspaceError`, whose
 * `refusal` is the host's typed refusal when it gave one
 * (`{reason, message, next}`, `host_account::Refusal`).
 */
import {joinWorkspace} from './peer.mjs';
import {enteredWorkspaces, hostDeviceOf, rememberEntered} from './workspaces.mjs';

/** The host's own operation that makes, or answers, the person's workspace. */
export const CREATE_OWN = Object.freeze({provider: 'tilo.host', address: {resource: 'host.rooms', action: 'room.create-own'}});

/** Why the person's own workspace could not be had. */
export class OwnWorkspaceError extends Error {
  constructor(message, {code = 'refused', refusal = null} = {}) {
    super(message);
    this.name = 'OwnWorkspaceError';
    this.code = code;
    this.refusal = refusal;
  }
}

/** The host's typed refusal `text` carries, if it carries one. */
function refusalIn(text) {
  try {
    const refusal = JSON.parse(text);
    return refusal && typeof refusal === 'object' && refusal.reason && typeof refusal.message === 'string' ? refusal : null;
  } catch {
    return null;
  }
}

/** Hex bytes as the text an invitation travels as (URL-safe base64, unpadded). */
export function invitationText(hex) {
  if (typeof hex !== 'string' || !/^(?:[0-9a-f]{2})+$/i.test(hex)) throw new TypeError('the host sent an invalid invitation');
  let binary = '';
  for (let at = 0; at < hex.length; at += 2) binary += String.fromCharCode(parseInt(hex.slice(at, at + 2), 16));
  return globalThis.btoa(binary).replaceAll('+', '-').replaceAll('/', '_').replace(/=+$/, '');
}

/**
 * Make or find the person's own workspace on the host of `page.peer` (or of
 * `page`, a peer itself), join it unless this browser entered it already,
 * and remember it. `name` names a new workspace ("My workspace" when
 * omitted; one the person has keeps its own). See the module.
 */
export async function ensureOwnWorkspace(page, {name = null, join = joinWorkspace, store = globalThis.localStorage, ...joinOptions} = {}) {
  const peer = page?.peer ?? page;
  if (!peer?.actions?.invoke || !peer.origin) throw new TypeError('the page holds no workspace on its host to ask from');
  const origin = new URL(peer.origin).origin;
  let entry;
  try {
    const ticket = peer.actions.invoke(CREATE_OWN, name ? {name} : {}, {label: 'Create your own workspace'});
    entry = await ticket.done;
  } catch (error) {
    const refusal = refusalIn(error?.message);
    throw new OwnWorkspaceError(refusal?.message ?? error?.message ?? String(error), {code: error?.code ?? 'refused', refusal});
  }
  if (entry?.state !== 'succeeded') {
    const message = entry?.update?.error?.message ?? 'the host did not make your workspace';
    throw new OwnWorkspaceError(message, {code: entry?.state ?? 'failed', refusal: refusalIn(message)});
  }
  const made = entry.update.output?.value ?? {};
  const id = made.workspace;
  if (typeof id !== 'string' || !id) throw new OwnWorkspaceError('the host answered no workspace', {code: 'malformed'});
  const named = typeof made.name === 'string' && made.name ? made.name : null;
  const hostDevice = hostDeviceOf(peer.host?.hostCert);
  const entered = enteredWorkspaces(store).some(kept => kept.origin === origin && kept.room === id);
  let joined = null;
  if (!entered) {
    if (!made.invitation?.invitation) throw new OwnWorkspaceError('the host issued no invitation to your workspace', {code: 'malformed'});
    joined = await join({
      ...joinOptions, origin, invitation: invitationText(made.invitation.invitation),
      ...(peer.displayName ? {displayName: peer.displayName} : {}),
    });
  }
  if (hostDevice) rememberEntered({origin, room: id, hostDevice, name: named}, store);
  return {id, name: named, host: new URL(origin).host, origin, peer: joined};
}
