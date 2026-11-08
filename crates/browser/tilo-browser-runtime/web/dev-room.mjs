/**
 * The page side of `tilo-app run --platform web --share`: open an app in the
 * development workspace the CLI runs, as a person of this page's own.
 *
 * The CLI runs the SDK's workspace host on this machine and is the workspace's door
 * (`/__tilo/workspace/…` on the page's own origin): it admits every person who
 * asks as an Editor. Each page creates a person, so two tabs are two
 * participants with their own `me`, name and history. Everyone opens the
 * workspace's one record of the app: the first page to arrive creates it, every
 * later page waits until it has reached it and opens that one, so two pages
 * never start the same record twice. The door also knows everyone's name,
 * which the page hands the app (`mount.setPeople`) so `cx.people()` names
 * each participant.
 */

import {joinWorkspace} from './peer.mjs';
import {createIdentity} from './identity.mjs';

const API = '/__tilo/room';

async function call(path, body, method = body === undefined ? 'GET' : 'POST') {
  const response = await fetch(API + path, {
    method,
    headers: {'content-type': 'application/json'},
    body: body === undefined ? undefined : JSON.stringify(body),
    cache: 'no-store',
  });
  const answer = await response.json().catch(() => ({}));
  if (!response.ok) throw new Error(answer.error ?? `the workspace answered ${response.status}`);
  return answer;
}

const pause = ms => new Promise(resolve => setTimeout(resolve, ms));

/**
 * Join the workspace as `name` and open its app in `surface`. `onStatus(phase)`
 * hears where joining is: `joining`, `waiting`, `loading`, then `opening`
 * once the app is about to present into `surface` (show it by then), and
 * `ready`. Resolves to `{peer, mount, record, stop}`.
 */
export async function openInRoom({surface, name, onStatus = () => {}}) {
  const workspace = await call('/');
  const wasm = await import('./pkg/tilo_browser_runtime.js');
  await wasm.default();
  onStatus('joining');
  const {identity} = await createIdentity({passkey: false});
  const offer = await call('/join', {userPublicKey: identity.userPublicKey, name});
  const peer = await joinWorkspace({
    ...offer, identity, displayName: name, label: name, runtime: wasm,
    profile: `tilo-dev-room:${identity.userPublicKey.slice(0, 16)}`,
  });
  await peer.whenReady();
  let claim = await call('/record', {app: workspace.app});
  while (!claim.record && !claim.open) {
    onStatus('waiting');
    await pause(300);
    claim = await call(`/record?app=${encodeURIComponent(workspace.app)}`);
  }
  let record = claim.record;
  if (record) {
    onStatus('loading');
    await peer.whenRecord(workspace.app, record, {timeoutMs: 120_000});
  }
  // The mount presents into `surface`, which has to be laid out by now.
  onStatus('opening');
  const mount = await peer.mountDesktop(surface, {
    apps: [workspace.app], app: workspace.app, ...(record ? {record} : {}), workspaceName: workspace.name,
  });
  if (!record) {
    record = mount.state().record;
    await call('/record', {app: workspace.app, record}, 'PUT');
  }
  // The workspace's door knows everyone's name: the app shows them by it.
  const names = async () => {
    try { mount.setPeople(await call('/people')); } catch {}
  };
  await names();
  const timer = setInterval(names, 2000);
  onStatus('ready');
  return {peer, mount, record, stop: () => clearInterval(timer)};
}
