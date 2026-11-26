/** Pair a person's next device from the workspace they opened on their current one. */
import {grammar} from './entry.mjs';
import {approvalHost, browserIdentity} from './identity.mjs';
import {createIdentityActions, PAIR} from './identity-actions.mjs';
import {assistantAddress, pairSecret} from './pairing.mjs';

function sheet(kind, title) {
  const dialog = document.createElement('dialog');
  dialog.className = 'tilo-pair-dialog';
  dialog.dataset[kind] = '';
  const heading = document.createElement('h2');
  heading.textContent = title;
  heading.id = 'tilo-pair-title';
  dialog.setAttribute('aria-labelledby', heading.id);
  const note = document.createElement('p');
  const controls = document.createElement('div');
  controls.className = 'tilo-question-actions';
  const button = (text, name, press) => {
    const item = document.createElement('button');
    item.type = 'button';
    item.textContent = text;
    item.dataset[name] = '';
    item.addEventListener('click', press);
    controls.append(item);
    return item;
  };
  dialog.append(heading, note, controls);
  document.body.append(dialog);
  const status = document.createElement('p');
  status.setAttribute('role', 'status');
  status.hidden = true;
  controls.before(status);
  return {dialog, note, controls, button, status, close: () => dialog.remove()};
}

function showLink(runtime, {link, copy, cancel}) {
  const card = sheet('pairLink', 'Pair a new device');
  card.note.textContent = 'Scan this code or paste the link into Tilo Assistant. Keep this page open to approve the device. The link works once, for ten minutes.';
  const input = document.createElement('input');
  Object.assign(input, {name: 'pairing-link', value: link, readOnly: true, spellcheck: false});
  input.setAttribute('aria-label', 'Pairing link');
  card.controls.before(input);
  try {
    const code = document.createElement('img');
    code.alt = 'Scan this pairing code with Tilo Assistant';
    code.width = code.height = 184;
    code.src = 'data:image/svg+xml,' + encodeURIComponent(runtime.qrSvg(link));
    input.before(code);
  } catch { /* The link can still be pasted or opened directly. */ }
  card.button('Cancel', 'pairCancel', cancel);
  const copied = card.button('Copy link', 'pairCopy', async () => {
    if (await copy()) copied.textContent = 'Copied';
    else { input.focus(); input.select(); }
  });
  const handoff = assistantAddress(link);
  if (handoff) card.button('Open in Tilo Assistant', 'pairOpen', () => { location.href = handoff; });
  card.dialog.addEventListener('cancel', event => { event.preventDefault(); cancel(); });
  card.dialog.showModal();
  return card;
}

function confirmDevice(request, decide) {
  const card = sheet('pairRequest', 'Approve a new device?');
  const label = request.label || 'This device';
  card.note.textContent = `Compare this code with the one on “${label}”. Approving lets that device act as you.`;
  const code = document.createElement('strong');
  code.dataset.pairCode = '';
  code.textContent = request.pairingCode;
  card.controls.before(code);
  const answer = async approved => {
    if (allow.disabled) return;
    allow.disabled = deny.disabled = true;
    try {
      await decide(approved);
      card.note.textContent = approved ? `“${label}” is now one of your devices.` : 'Device declined.';
      code.remove();
      card.controls.replaceChildren();
      card.button('Done', 'pairDone', card.close).focus();
    } catch (error) {
      card.note.textContent = error.message || 'Could not answer the device. Try again.';
      allow.disabled = deny.disabled = false;
    }
  };
  const deny = card.button('Decline', 'pairDeny', () => { void answer(false); });
  const allow = card.button('Approve', 'pairAllow', () => { void answer(true); });
  card.dialog.addEventListener('cancel', event => {
    if (allow.isConnected) { event.preventDefault(); void answer(false); }
    else card.close();
  });
  card.dialog.showModal();
  deny.focus();
  return card;
}

/** Only the unlocked root of the device that opened this workspace may sponsor a new device. */
export async function pairWorkspaceDevice({origin, workspace, person, base, signal}, {
  identity = browserIdentity, runtime = async () => (await import('./host.mjs')).loadRuntime(), hostAt = approvalHost,
  show = showLink, confirm = confirmDevice, controller = createIdentityActions,
} = {}) {
  if (!origin || !workspace || !person) throw new Error('Open your workspace before pairing a device.');
  signal?.throwIfAborted();
  const root = await identity({create: false});
  if (!root?.sign || root.locked || root.userPublicKey !== person) {
    throw new Error('Open this workspace in the browser where you created your identity or restored your recovery words.');
  }
  const [wasm, host] = await Promise.all([runtime(), hostAt(origin)]);
  signal?.throwIfAborted();
  const address = grammar(wasm, {base}).format({mode: 'desktop', target: {scope: {kind: 'workspace', host: origin, workspace}}});
  let card;
  const status = message => {
    if (!card?.status) return;
    card.status.hidden = !message;
    card.status.textContent = message;
  };
  const withdraw = () => actions.withdrawInvites();
  const pairing = {
    close: () => signal?.removeEventListener('abort', withdraw),
    link: invite => address + (new URL(address).hash.includes('?') ? '&' : '?') + 'pair=' + pairSecret(person, invite),
    pending: async () => {
      signal?.throwIfAborted();
      const pending = JSON.parse(await host.pending(root));
      status('');
      return pending.filter(request => !request.signIn);
    },
    answer: (digest, approved) => {
      signal?.throwIfAborted();
      return host.answer(root, digest, approved);
    },
  };
  const actions = controller({identity: root, pairing,
    showPairLink: options => (card = show(wasm, options)),
    askPairing: (request, decide) => (card = confirm(request, decide)),
    onError: error => {
      if (!card) throw error;
      status('Could not reach your host. Keep this page open to try again.');
    },
    copy: text => navigator.clipboard.writeText(text)});
  signal?.addEventListener('abort', withdraw, {once: true});
  await actions.perform(PAIR.id);
}
