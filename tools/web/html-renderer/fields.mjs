// A sign-in form's fields on a page shown as HTML: Passwords' new-item form
// mounted alone, its username and password fields shown as the page's own
// inputs. Checks what a browser needs to fill them in and to offer passkeys
// on them, types into them as a person and as autofill does, and asks for a
// passkey on the username field through a virtual authenticator.
//
// usage: node tools/web/html-renderer/fields.mjs BUNDLE_DIR OUT_DIR
import {mkdirSync, writeFileSync} from 'node:fs';
import {join, resolve} from 'node:path';
import {launch, serve, sleep} from '../chrome.mjs';

const bundle = resolve(process.argv[2]);
const out = resolve(process.argv[3]);
mkdirSync(out, {recursive: true});
const sandbox = process.getuid?.() === 0 ? ['--no-sandbox'] : [];

// Each field the page shows as its own input: what it is, where it is, and
// whether it is what the pointer finds there.
const FIELDS = `JSON.stringify([...document.querySelectorAll('input[data-tilo-field]')].map(input => {
  const r = input.getBoundingClientRect();
  return {type: input.type, autocomplete: input.getAttribute('autocomplete'), value: input.type === 'password' ? input.value.length : input.value,
    rect: [r.left, r.top, r.width, r.height].map(Math.round), opacity: getComputedStyle(input).opacity,
    hit: document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2) === input,
    font: getComputedStyle(input).fontFamily, focused: document.activeElement === input};
}))`;
// The semantic tree's text fields: what the app holds.
const TREE = `(() => { const found = [];
  const walk = n => { if (!n) return; const s = n.semantics;
    if (s && s.role === 'TextInput') found.push({value: s.value ?? null, autofill: s.state?.autofill ?? null, protected: Boolean(s.state?.protected), focused: Boolean(s.state?.focused)});
    for (const c of n.children ?? []) walk(c); };
  walk(window.mount.state()?.semantics?.root); return JSON.stringify(found); })()`;

const server = await serve(bundle);
// A passkey's relying party is a domain: localhost is one, 127.0.0.1 is not.
const origin = server.url.replace('127.0.0.1', 'localhost');
const browser = await launch({args: ['--window-size=1280,800', ...sandbox]});
const result = {};
try {
  const tab = await browser.tab();
  await tab.send('Emulation.setDeviceMetricsOverride', {width: 1280, height: 800, deviceScaleFactor: 1, mobile: false});
  await tab.navigate(`${origin}/app.html?app=so.tilo.passwords`);
  // Who moves the focus: each focus() and blur() with where it came from.
  await tab.evaluate(`(() => { const log = window.__focusCalls = []; for (const method of ['focus', 'blur']) { const original = HTMLElement.prototype[method];
    HTMLElement.prototype[method] = function (...args) { if (log.length < 40) log.push([method, this.localName, (new Error().stack ?? '').split(String.fromCharCode(10)).slice(2, 9).map(l => l.trim().replace(/https?:[^ )]*\//, '')).join(' < ')]); return original.apply(this, args); }; }
    addEventListener('error', e => log.push(['error', String(e.message), (e.error?.stack ?? '').slice(0, 600)]), true);
    return true; })()`).catch(() => {});
  await tab.waitFor('window.renderMarks?.presented[0]', {timeout: 90000});
  await sleep(2000);
  // A new item, through the app's own action, as its toolbar does.
  result.add = await tab.evaluate(`(async () => {
    const add = window.mount.actions().find(a => a.address.action === 'add');
    if (!add) return 'no add action: ' + window.mount.actions().map(a => a.address.action).join(',');
    return JSON.stringify(await window.mount.invokeAction(add.address.resource, add.address.action, {}));
  })()`).catch(error => String(error));
  await sleep(1500);
  result.fields = JSON.parse(await tab.evaluate(FIELDS));
  result.tree = JSON.parse(await tab.evaluate(TREE));
  writeFileSync(join(out, 'fields-form.png'), await tab.screenshot());
  const username = result.fields.find(field => field.autocomplete === 'username webauthn');
  const password = result.fields.find(field => field.type === 'password');
  if (username) {
    // A person clicks the field and types: the browser's caret and text.
    const tree = ops => { const out = []; const stack = []; for (const op of JSON.parse(ops)) { if (op[0] === 'k') { out.push(`${'  '.repeat(stack.length)}${op[1]} @${op[2]},${op[3]}`); stack.push(op[1]); } else if (op[0] === 'ke') stack.pop(); } return out; };
    const keysBefore = tree(await tab.evaluate('window.__tiloDom.stats.ops'));
    const [x, y, w, h] = username.rect;
    for (const type of ['mousePressed', 'mouseReleased']) await tab.send('Input.dispatchMouseEvent', {type, x: x + w / 2, y: y + h / 2, button: 'left', clickCount: 1});
    await sleep(300);
    const keysAfter = tree(await tab.evaluate('window.__tiloDom.stats.ops'));
    const before = new Set(keysBefore), after = new Set(keysAfter);
    result.keysChanged = {gone: keysBefore.filter(k => !after.has(k)).slice(0, 30), new: keysAfter.filter(k => !before.has(k)).slice(0, 30),
      rootsBefore: keysBefore.filter(k => !k.startsWith(' ')), rootsAfter: keysAfter.filter(k => !k.startsWith(' '))};
    for (const ch of 'ada') {
      await tab.send('Input.dispatchKeyEvent', {type: 'keyDown', key: ch, text: ch, unmodifiedText: ch});
      await tab.send('Input.dispatchKeyEvent', {type: 'keyUp', key: ch});
      await sleep(60);
    }
    // Autofill replaces the value in one go, as a password manager does:
    // the browser sets it and says so with an input event, no key pressed.
    await tab.evaluate(`(() => { const log = window.__inputs = []; for (const t of ['beforeinput', 'input', 'change', 'keydown'])
      document.addEventListener(t, e => log.push([t, e.inputType ?? e.key ?? '', e.target.value ?? '', e.defaultPrevented]), true); return true; })()`);
    await tab.evaluate(`(() => { const input = document.activeElement; input.select(); return true; })()`);
    await tab.send('Input.insertText', {text: 'ada@example.org'});
    await sleep(800);
    result.inserted = JSON.parse(await tab.evaluate('JSON.stringify({inputs: window.__inputs, value: document.activeElement.value})'));
    await tab.evaluate(`(() => { const input = document.activeElement; input.value = 'ada@example.net'; input.dispatchEvent(new InputEvent('input', {bubbles: true, inputType: 'insertReplacementText'})); input.dispatchEvent(new Event('change', {bubbles: true})); return true; })()`);
    await sleep(800);
    result.filled = JSON.parse(await tab.evaluate('JSON.stringify({inputs: window.__inputs.slice(-6), value: document.activeElement.value})'));
    result.typed = {fields: JSON.parse(await tab.evaluate(FIELDS)), tree: JSON.parse(await tab.evaluate(TREE)),
      // The value is the input's alone: no painted text shows it under it.
      paintedValue: await tab.evaluate(`[...document.querySelectorAll('.tilo-dom span')].some(s => s.textContent.includes('ada@'))`)};
    writeFileSync(join(out, 'fields-typed.png'), await tab.screenshot());
  }
  if (password) {
    const [x, y, w, h] = password.rect;
    for (const type of ['mousePressed', 'mouseReleased']) await tab.send('Input.dispatchMouseEvent', {type, x: x + w / 2, y: y + h / 2, button: 'left', clickCount: 1});
    await sleep(300);
    await tab.send('Input.insertText', {text: 'correct horse'});
    await sleep(800);
    result.password = {fields: JSON.parse(await tab.evaluate(FIELDS)), tree: JSON.parse(await tab.evaluate(TREE))};
  }
  // A passkey: a virtual platform authenticator holding one for this site,
  // and a conditional request, which offers it on the username field.
  await tab.send('WebAuthn.enable', {enableUI: false});
  const {result: {authenticatorId}} = await tab.send('WebAuthn.addVirtualAuthenticator', {options: {
    protocol: 'ctap2', transport: 'internal', hasResidentKey: true, hasUserVerification: true,
    isUserVerified: true, automaticPresenceSimulation: true}});
  result.passkey = JSON.parse(await tab.evaluate(`(async () => {
    const out = {conditional: await PublicKeyCredential.isConditionalMediationAvailable?.() ?? false};
    const id = crypto.getRandomValues(new Uint8Array(16));
    const created = await navigator.credentials.create({publicKey: {
      challenge: crypto.getRandomValues(new Uint8Array(32)), rp: {id: 'localhost', name: 'Tilo'},
      user: {id, name: 'ada@example.org', displayName: 'Ada'}, pubKeyCredParams: [{type: 'public-key', alg: -7}],
      authenticatorSelection: {residentKey: 'required', userVerification: 'required'}}}).catch(e => String(e));
    out.created = typeof created === 'string' ? created : created.type;
    window.__conditional = navigator.credentials.get({mediation: 'conditional', publicKey: {
      challenge: crypto.getRandomValues(new Uint8Array(32)), rpId: 'localhost', userVerification: 'required'}})
      .then(c => 'resolved ' + c.type, e => 'rejected ' + e.name);
    return JSON.stringify(out);
  })()`));
  if (username) {
    const [x, y, w, h] = username.rect;
    for (const type of ['mousePressed', 'mouseReleased']) await tab.send('Input.dispatchMouseEvent', {type, x: x + w / 2, y: y + h / 2, button: 'left', clickCount: 1});
  }
  result.passkey.outcome = await tab.evaluate(`Promise.race([window.__conditional, new Promise(r => setTimeout(() => r('pending'), 4000))])`);
  result.passkey.credentials = (await tab.send('WebAuthn.getCredentials', {authenticatorId})).result.credentials.length;
  result.focusCalls = JSON.parse(await tab.evaluate('JSON.stringify(window.__focusCalls ?? [])'));
  result.log = tab.log.filter(line => /^(EXC|ERROR)/.test(line)).slice(0, 10);
} catch (error) {
  result.error = String(error?.stack ?? error);
} finally {
  await server.close();
  await browser.close().catch(() => {});
}
console.log(JSON.stringify(result, null, 1));
writeFileSync(join(out, 'fields.json'), JSON.stringify(result, null, 1));
