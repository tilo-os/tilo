// The page half of the browser tests that drive the real page modules in
// headless Chrome: serve the page's sources beside a built runtime bundle
// (`pkg/`, from `page-bundle.mjs`), open a tab with a virtual WebAuthn
// authenticator that answers PRF, and run one mode's steps over stdio.
//
// usage: node support/page-driver.mjs passkey|node
//
// `passkey` creates an identity, unlocks it with its passkey, its recovery
// key and a PRF output the page asserted itself, adds a second passkey on
// another authenticator, proves a person this browser kept before the
// envelope moved into the runtime still opens (`legacy-root-record.json`),
// then prints `{envelope, recoveryKey}` and one hex signature per hex
// message read from stdin, until stdin closes.
//
// `node` creates an identity, prints `{origin, userPublicKey}`, then answers
// one JSON request per stdin line until stdin closes:
//   {"sign": "<hex>"}                            -> {"signature": "<hex>"}
//   {"info": {origin, devicePublicKey, installToken}}
//                                                -> what nodeInstallInfo resolves to
//   {"install": {origin, certificate, sessionCertificate?, installToken}}
//     (certificates canonical, hex)              -> what installNodeCertificate resolves to
// Any failure is answered as {"error": "...", "code": "..."}.
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {createInterface} from 'node:readline';
import {fileURLToPath} from 'node:url';
import {launch, serve} from '../../../../../tools/web/chrome.mjs';
import {pageBundle} from '../../../../../tools/web/page-bundle.mjs';

const mode = process.argv[2];
if (!['passkey', 'node'].includes(mode)) throw new Error('usage: page-driver.mjs passkey|node');
const web = fileURLToPath(new URL('../../../../browser/tilo-browser-runtime/web/', import.meta.url));
const legacy = JSON.parse(readFileSync(fileURLToPath(new URL('./legacy-root-record.json', import.meta.url)), 'utf8'));
/** `text`, base64url, as the bytes it names. */
const bytes = text => Array.from(Buffer.from(text.replace(/-/g, '+').replace(/_/g, '/'), 'base64'));
const base64 = hex => Buffer.from(hex, 'hex').toString('base64');
/** Bytes in a page expression. */
const array = values => `Uint8Array.from(${JSON.stringify(values)})`;

const bundle = pageBundle();
const server = await serve(web, {also: [bundle.dir]});
// WebAuthn takes a domain as relying party, never an IP address.
const origin = server.url.replace('127.0.0.1', 'localhost');
const browser = await launch({args: process.getuid?.() === 0 ? ['--no-sandbox'] : []});
const authenticator = {
  protocol: 'ctap2', ctap2Version: 'ctap2_1', transport: 'internal',
  hasResidentKey: true, hasUserVerification: true, isUserVerified: true,
  hasPrf: true, automaticPresenceSimulation: true,
};
let failed = false;
try {
  const tab = await browser.tab();
  await tab.navigate(`${origin}/identity.mjs`);
  await tab.waitFor('document.readyState === "complete"', {timeout: 30_000});
  const send = async (method, params) => {
    const reply = await tab.send(method, params);
    if (reply.error) throw new Error(`${method}: ${reply.error.message}`);
    return reply.result;
  };
  await send('WebAuthn.enable', {enableUI: false});
  const {authenticatorId} = await send('WebAuthn.addVirtualAuthenticator', {options: authenticator});
  /** `expression` run in the page with `id`, the identity module; a throw is
   * answered as `{error, code}` for modes that report per request. */
  const page = expression => tab.evaluate(`(async () => {
    const id = await import('/identity.mjs');
    try { ${expression} } catch (error) { return {error: String(error?.message ?? error), code: error?.code}; }
  })()`);
  /** `expression` run in the page, a throw failing the run. */
  const run = async expression => {
    const answer = await page(expression);
    if (answer?.error !== undefined) throw new Error(`${answer.error}${answer.code ? ` (${answer.code})` : ''}`);
    return answer;
  };

  if (mode === 'passkey') {
    const created = await run(`
      const {envelope, recoveryKey, identity} = await id.createIdentity({rpId: 'localhost', userName: 'ada@example.com', userId: 'user-1'});
      window.created = {envelope, recoveryKey};
      window.root = identity;
      return {envelope, recoveryKey};
    `);
    assert.deepEqual(created.envelope.wraps.map(wrap => wrap.kind), ['passkey', 'recovery']);
    assert.equal(created.envelope.wraps[0].rp_id, 'localhost');

    const probe = `
      const message = new TextEncoder().encode('probe');
      const key = await crypto.subtle.importKey('raw', id.unhex(window.created.envelope.root_public_key), {name: 'Ed25519'}, false, ['verify']);
      const verifies = await crypto.subtle.verify({name: 'Ed25519'}, key, await unlocked.sign(message), message);
    `;
    assert.equal(await run(`
      const unlocked = await id.unlockIdentity(window.created.envelope, {passkey: true});
      ${probe}
      return verifies && unlocked.userPublicKey === window.created.envelope.root_public_key;
    `), true, 'the passkey unlocks');
    assert.equal(await run(`
      const unlocked = await id.unlockIdentity(window.created.envelope, {recoveryKey: window.created.recoveryKey});
      ${probe}
      return verifies;
    `), true, 'the recovery key unlocks');

    // The authenticator that holds the first passkey declines a second one;
    // another authenticator (a phone, a security key) takes it.
    assert.equal(await run(`
      return id.addPasskey(window.created.envelope, {identity: window.root, rpId: 'localhost'})
        .then(() => 'added', error => error.name);
    `), 'InvalidStateError');
    await send('WebAuthn.removeVirtualAuthenticator', {authenticatorId});
    await send('WebAuthn.addVirtualAuthenticator', {options: authenticator});
    const envelope = await run(`
      window.created.envelope = await id.addPasskey(window.created.envelope, {identity: window.root, rpId: 'localhost'});
      return window.created.envelope;
    `);
    assert.equal(envelope.wraps.length, 3);
    assert.equal(await run(`
      const unlocked = await id.unlockIdentity(window.created.envelope, {passkey: true});
      ${probe}
      window.root = unlocked;
      return verifies;
    `), true, 'the passkey on the other authenticator unlocks alone');
    // A login that already asserted the passkey with PRF unlocks without a
    // second prompt.
    assert.equal(await run(`
      const bytes = text => Uint8Array.from(atob(text.replace(/-/g, '+').replace(/_/g, '/') + '='.repeat((4 - text.length % 4) % 4)), char => char.charCodeAt(0));
      const wrap = window.created.envelope.wraps[2];
      const assertion = await navigator.credentials.get({publicKey: {
        challenge: crypto.getRandomValues(new Uint8Array(32)), rpId: 'localhost', userVerification: 'required',
        allowCredentials: [{type: 'public-key', id: bytes(wrap.credential_id)}],
        extensions: {prf: {eval: {first: bytes(wrap.prf_salt)}}},
      }});
      const unlocked = await id.unlockIdentity(window.created.envelope, {
        prfOutput: assertion.getClientExtensionResults().prf.results.first, credentialId: assertion.rawId,
      });
      ${probe}
      return verifies;
    `), true, 'a PRF output from the page\'s own assertion unlocks');
    assert.equal(await run(`
      return id.unlockIdentity(window.created.envelope, {recoveryKey: '0000-0000-0000-0000-0000-0000-0000-0000'})
        .then(() => 'opened', error => error.code);
    `), 'rejected');

    // A person made on a first visit is kept from the start: the next load
    // opens the same root, and it still signs.
    assert.deepEqual(await run(`
      const kept = new Map();
      const storage = {get: async key => kept.get(key), put: async (key, value) => { kept.set(key, value); }, delete: async key => { kept.delete(key); }};
      const made = await id.browserIdentity({storage, persist: null});
      const again = await id.browserIdentity({storage, create: false, persist: null});
      const message = new TextEncoder().encode('again');
      const key = await crypto.subtle.importKey('raw', id.unhex(made.userPublicKey), {name: 'Ed25519'}, false, ['verify']);
      return {same: again?.userPublicKey === made.userPublicKey, signs: await crypto.subtle.verify({name: 'Ed25519'}, key, await again.sign(message), message)};
    `), {same: true, signs: true}, 'a person made on a first visit is the same person on the next load');

    // A person this browser kept while the page held the root: the record
    // its IndexedDB holds (the root wrapped under the page's own AES-GCM
    // key) still opens, and its envelope still opens with its recovery key.
    assert.deepEqual(await run(`
      const record = {
        rootPublicKey: ${JSON.stringify(legacy.rootPublicKey)},
        held: {
          key: await crypto.subtle.importKey('raw', ${array(bytes(legacy.held.key))}, {name: 'AES-GCM'}, false, ['wrapKey', 'unwrapKey']),
          nonce: ${array(bytes(legacy.held.nonce))},
          wrapped: ${array(bytes(legacy.held.wrapped))},
        },
        envelope: ${JSON.stringify(legacy.envelope)},
      };
      const identity = await id.browserIdentity({storage: {get: async () => record, put: async () => {}, delete: async () => {}}, persist: null});
      const message = new TextEncoder().encode('probe');
      const key = await crypto.subtle.importKey('raw', id.unhex(record.envelope.root_public_key), {name: 'Ed25519'}, false, ['verify']);
      const verifies = await crypto.subtle.verify({name: 'Ed25519'}, key, await identity.sign(message), message);
      const opened = await id.unlockIdentity(record.envelope, {recoveryKey: ${JSON.stringify(legacy.recoveryKey)}});
      return {verifies, userPublicKey: identity.userPublicKey, kept: identity.kept, recoveryKey: opened.userPublicKey};
    `), {verifies: true, userPublicKey: legacy.rootPublicKey, kept: true, recoveryKey: legacy.rootPublicKey},
      'a person this browser kept before the runtime holds the root still opens');

    process.stdout.write(`${JSON.stringify({envelope, recoveryKey: created.recoveryKey})}\n`);
    for await (const line of createInterface({input: process.stdin})) {
      if (!/^([0-9a-f]{2})+$/.test(line)) throw new Error(`not a hex message: ${line}`);
      const signature = await run(`return id.hex(await window.root.sign(id.unhex(${JSON.stringify(line)})));`);
      process.stdout.write(`${signature}\n`);
    }
  } else {
    const created = await run(`
      const {identity} = await id.createIdentity({rpId: 'localhost', userName: 'ada@example.com', userId: 'user-1'});
      window.root = identity;
      return {userPublicKey: identity.userPublicKey};
    `);
    process.stdout.write(`${JSON.stringify({origin, ...created})}\n`);
    for await (const line of createInterface({input: process.stdin})) {
      const request = JSON.parse(line);
      const answer = request.sign !== undefined
        ? await page(`return {signature: id.hex(await window.root.sign(id.unhex(${JSON.stringify(request.sign)})))};`)
        : request.info !== undefined
          ? await page(`return await id.nodeInstallInfo(${JSON.stringify(request.info)});`)
          : await page(`return await id.installNodeCertificate(${JSON.stringify({
            ...request.install,
            // What certifyDevice resolves to: the canonical encoding, base64.
            certificate: base64(request.install.certificate),
            sessionCertificate: request.install.sessionCertificate && base64(request.install.sessionCertificate),
          })});`);
      process.stdout.write(`${JSON.stringify(answer)}\n`);
    }
  }
  if (tab.log.length) console.error(tab.log.join('\n'));
} catch (error) {
  failed = true;
  console.error(error);
} finally {
  await browser.close();
  await server.close();
  bundle.close();
}
process.exit(failed ? 1 : 0);
