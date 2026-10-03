// The page half of passkey_root.rs: headless Chrome with a virtual WebAuthn
// authenticator that supports PRF runs the production root-identity.mjs.
// It creates an identity, unlocks it with the passkey and with the recovery
// key, adds a second passkey on another authenticator and unlocks with it
// alone once the first is gone, then lends the root as a signer over stdio.
//
// usage: node driver.mjs
// stdout: one JSON line {envelope, recoveryKey}, then one hex signature per
// hex message read from stdin, until stdin closes. Exits non-zero on a
// failed expectation.
import assert from 'node:assert/strict';
import {fileURLToPath} from 'node:url';
import {createInterface} from 'node:readline';
import {launch, serve} from '../../../../../tools/web/chrome.mjs';

const web = fileURLToPath(new URL('../../../tilo-browser-runtime/web/', import.meta.url));
const server = await serve(web);
// WebAuthn takes a domain as relying party, never an IP address.
const origin = server.url.replace('127.0.0.1', 'localhost');
const browser = await launch({args: process.getuid?.() === 0 ? ['--no-sandbox'] : []});
let failed = false;
try {
  const tab = await browser.tab();
  await tab.navigate(`${origin}/root-identity.mjs`);
  await tab.waitFor('document.readyState === "complete"', {timeout: 30_000});
  const send = async (method, params) => {
    const reply = await tab.send(method, params);
    if (reply.error) throw new Error(`${method}: ${reply.error.message}`);
    return reply.result;
  };
  await send('WebAuthn.enable', {enableUI: false});
  const authenticator = {
    protocol: 'ctap2', ctap2Version: 'ctap2_1', transport: 'internal',
    hasResidentKey: true, hasUserVerification: true, isUserVerified: true,
    hasPrf: true, automaticPresenceSimulation: true,
  };
  const {authenticatorId} = await send('WebAuthn.addVirtualAuthenticator', {options: authenticator});
  const page = expression => tab.evaluate(`(async () => {
    const id = await import('/root-identity.mjs');
    const options = {rpId: 'localhost', userName: 'ada@example.com', userId: 'user-1'};
    ${expression}
  })()`);

  const created = await page(`
    const {envelope, recoveryKey, identity} = await id.createIdentity(options);
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
  assert.equal(await page(`
    const unlocked = await id.unlockIdentity(window.created.envelope, {passkey: true});
    ${probe}
    return verifies && unlocked.userPublicKey === window.created.envelope.root_public_key;
  `), true, 'the passkey unlocks');
  assert.equal(await page(`
    const unlocked = await id.unlockIdentity(window.created.envelope, {recoveryKey: window.created.recoveryKey});
    ${probe}
    return verifies;
  `), true, 'the recovery key unlocks');

  // The authenticator that holds the first passkey declines a second one;
  // another authenticator (a phone, a security key) takes it.
  assert.equal(await page(`
    return id.addPasskey(window.created.envelope, {identity: window.root, ...options}).then(() => 'added', error => error.name);
  `), 'InvalidStateError');
  await send('WebAuthn.removeVirtualAuthenticator', {authenticatorId});
  await send('WebAuthn.addVirtualAuthenticator', {options: authenticator});
  const envelope = await page(`
    window.created.envelope = await id.addPasskey(window.created.envelope, {identity: window.root, ...options});
    return window.created.envelope;
  `);
  assert.equal(envelope.wraps.length, 3);
  assert.equal(await page(`
    const unlocked = await id.unlockIdentity(window.created.envelope, {passkey: true});
    ${probe}
    window.root = unlocked;
    return verifies;
  `), true, 'the passkey on the other authenticator unlocks alone');
  // A login that already asserted the passkey with PRF unlocks without a
  // second prompt.
  assert.equal(await page(`
    const wrap = window.created.envelope.wraps[2];
    const assertion = await navigator.credentials.get({publicKey: {
      challenge: crypto.getRandomValues(new Uint8Array(32)), rpId: 'localhost', userVerification: 'required',
      allowCredentials: [{type: 'public-key', id: id.unbase64url(wrap.credential_id)}],
      extensions: {prf: {eval: {first: id.unbase64url(wrap.prf_salt)}}},
    }});
    const unlocked = await id.unlockIdentity(window.created.envelope, {
      prfOutput: assertion.getClientExtensionResults().prf.results.first, credentialId: assertion.rawId,
    });
    ${probe}
    return verifies;
  `), true, 'a PRF output from the page\'s own assertion unlocks');
  assert.equal(await page(`
    return id.unlockIdentity(window.created.envelope, {recoveryKey: '0000-0000-0000-0000-0000-0000-0000-0000'})
      .then(() => 'opened', error => error.code);
  `), 'rejected');

  process.stdout.write(`${JSON.stringify({envelope, recoveryKey: created.recoveryKey})}\n`);
  for await (const line of createInterface({input: process.stdin})) {
    if (!/^([0-9a-f]{2})+$/.test(line)) throw new Error(`not a hex message: ${line}`);
    const signature = await page(`return id.hex(await window.root.sign(id.unhex(${JSON.stringify(line)})));`);
    process.stdout.write(`${signature}\n`);
  }
  if (tab.log.length) console.error(tab.log.join('\n'));
} catch (error) {
  failed = true;
  console.error(error);
} finally {
  await browser.close();
  await server.close();
}
process.exit(failed ? 1 : 0);
