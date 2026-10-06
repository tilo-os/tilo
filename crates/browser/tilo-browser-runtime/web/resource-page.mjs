import {connectPeer, withCustody} from './peer.mjs';
import {loadRuntime} from './host.mjs';
import {idbStorage, openDeviceCustody} from './device-custody.mjs';
import {parseResourceLink} from './resource-link.mjs';
import {openResourceOperation, closeResourceOperation} from './resource-session.mjs';
import {readBrowserAccounts, currentBrowserAccount} from './browser-accounts.mjs';
const form = document.getElementById('open');
const status = document.getElementById('status');
const leave = document.getElementById('leave');
const surface = document.getElementById('surface');
let active;
let choices = [];
let selectionGeneration = 0;
let refreshingAccounts = false;
const picker = form.elements.account;
async function close(operation = active) {
  try {
    await closeResourceOperation(operation);
  } finally {
    if (active === operation) {
      active = undefined;
      document.getElementById('entry').hidden = false;
      surface.replaceChildren();
      leave.hidden = true;
      form.querySelector('button').disabled = choices.length === 0;
    }
  }
}
leave.addEventListener('click', async () => {
  try { await close(); status.textContent = 'Left the shared resource.'; }
  catch { status.textContent = 'Leave cleanup failed.'; }
});
addEventListener('pagehide', () => { void close().catch(() => {}); });
try {
  const address = parseResourceLink(location.href);
  document.getElementById('destination').textContent = `Host: ${address.origin} · Workspace: ${address.room}`;
  const runtime = await loadRuntime();
  const readAccounts = () => readBrowserAccounts({origin: address.origin, runtime});
  async function refreshAccounts() {
    const generation = ++selectionGeneration;
    refreshingAccounts = true;
    await close();
    picker.disabled = true;
    form.querySelector('button').disabled = true;
    let fresh;
    try { fresh = await readAccounts(); }
    finally {
      if (generation === selectionGeneration) {
        refreshingAccounts = false;
        picker.disabled = false;
      }
    }
    if (generation !== selectionGeneration) return;
    choices = fresh;
    picker.replaceChildren(...choices.map(account => {
      const option = document.createElement('option');
      option.value = account.selection;
      option.textContent = account.displayName || account.userPublicKey.slice(0, 12);
      return option;
    }));
    form.querySelector('button').disabled = choices.length === 0;
    status.textContent = choices.length ? 'Choose an existing account to open this resource.'
      : 'No matching account is configured in Settings. Sign in through the usual account flow first.';
  }
  document.getElementById('accounts').addEventListener('click', () => {
    void refreshAccounts().catch(error => { status.textContent = error.message; });
  });
  picker.addEventListener('change', () => { void close().catch(error => { status.textContent = error.message; }); });
  await refreshAccounts();
  form.addEventListener('submit', async event => {
    event.preventDefault();
    if (refreshingAccounts || active) return;
    form.querySelector('button').disabled = true;
    const operation = active = {controller: new AbortController()};
    leave.hidden = false;
    status.textContent = 'Checking your existing admission…';
    try {
      const selected = choices.find(account => account.selection === picker.value);
      const account = await currentBrowserAccount(selected, readAccounts);
      operation.controller.signal.throwIfAborted();
      const {userPublicKey, profile, device} = account;
      await openResourceOperation({operation, link: location.href, target: surface,
        connect: async (address, signal) => {
          await currentBrowserAccount(account, readAccounts);
          signal.throwIfAborted();
          const custody = operation.custody = await openDeviceCustody({profile, storage: idbStorage(), requireExisting: true});
          signal.throwIfAborted();
          return withCustody(custody, () => connectPeer({origin: address.origin, room: address.room, userPublicKey,
            profile, device, custody, runtime, requireExistingCertificate: true, live: false, signal}), signal);
        }});
      await currentBrowserAccount(account, readAccounts);
      operation.controller.signal.throwIfAborted();
      document.getElementById('entry').hidden = true;
      status.textContent = 'Shared resource open. Leave to close this session.';
    } catch (error) {
      const wasCurrent = active === operation;
      await close(operation).catch(() => {});
      if (wasCurrent) status.textContent = error?.message ?? 'Shared resource unavailable';
    }
  });
} catch (error) {
  form.hidden = true; status.textContent = error.message;
}
