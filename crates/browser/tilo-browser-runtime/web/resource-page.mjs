import {connectPeer, identityProfile, withCustody} from './peer.mjs';
import {loadRuntime} from './host.mjs';
import {idbStorage, openDeviceCustody} from './device-custody.mjs';
import {parseResourceLink, openResourceLink} from './resource-link.mjs';
const form = document.getElementById('open');
const status = document.getElementById('status');
const leave = document.getElementById('leave');
const surface = document.getElementById('surface');
let active;
async function close(operation = active) {
  operation?.controller.abort();
  await operation?.mount?.cleanup();
  await operation?.peer?.dispose();
  if (active === operation) {
    active = undefined;
    document.getElementById('entry').hidden = false;
    surface.replaceChildren();
    leave.hidden = true;
    form.querySelector('button').disabled = false;
  }
}
leave.addEventListener('click', async () => { await close(); status.textContent = 'Left the shared resource.'; });
addEventListener('pagehide', () => { void close(); });
try {
  const address = parseResourceLink(location.href);
  document.getElementById('destination').textContent = `Host: ${address.origin} · Workspace: ${address.room}`;
  form.addEventListener('submit', async event => {
    event.preventDefault();
    form.querySelector('button').disabled = true;
    const operation = active = {controller: new AbortController()};
    const signal = operation.controller.signal; leave.hidden = false;
    status.textContent = 'Checking your existing admission…';
    try {
      const userPublicKey = new FormData(form).get('account').toLowerCase();
      if (!/^[a-f0-9]{64}$/u.test(userPublicKey)) throw Error('Choose an existing public identity key');
      const profile = identityProfile({userPublicKey}, address.origin);
      const runtime = await loadRuntime();
      const custody = await openDeviceCustody({profile, storage: idbStorage()});
      operation.peer = await withCustody(custody, () => connectPeer({origin: address.origin, room: address.room, userPublicKey,
        profile, custody, runtime, requireExistingCertificate: true, live: false,
        signal}), signal);
      signal.throwIfAborted();
      operation.mount = await openResourceLink({peer: operation.peer, link: location.href, target: surface, signal});
      signal.throwIfAborted();
      document.getElementById('entry').hidden = true;
      status.textContent = 'Shared resource open. Leave to close this session.';
    } catch (error) {
      const wasCurrent = active === operation;
      await close(operation);
      if (wasCurrent) status.textContent = error?.message ?? 'Shared resource unavailable';
    }
  });
} catch (error) {
  form.hidden = true; status.textContent = error.message;
}
