// The page half of node_workspace.rs: headless Chrome with a virtual
// WebAuthn authenticator (PRF) runs the production root-identity.mjs. It
// creates the person's identity with a passkey, then answers one JSON
// request per stdin line until stdin closes:
//   {"sign": "<hex>"}                              -> {"signature": "<hex>"}
//   {"info": {origin, devicePublicKey, installToken}}
//                                                  -> what nodeInstallInfo resolves to
//   {"install": {origin, certificate (canonical, hex), installToken}}
//   {"install": {origin, certificate (JSON), sessionCertificate (JSON), installToken}}
//                                                  -> what installNodeCertificate resolves to
// Any failure is answered as {"error": "..."}.
//
// usage: node driver.mjs
// stdout: {"origin": "<page origin>", "userPublicKey": "<hex>"}, then one
// answer line per request.
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
  await send('WebAuthn.addVirtualAuthenticator', {options: {
    protocol: 'ctap2', ctap2Version: 'ctap2_1', transport: 'internal',
    hasResidentKey: true, hasUserVerification: true, isUserVerified: true,
    hasPrf: true, automaticPresenceSimulation: true,
  }});
  const page = expression => tab.evaluate(`(async () => {
    const id = await import('/root-identity.mjs');
    try { ${expression} } catch (error) { return {error: String(error?.message ?? error)}; }
  })()`);
  const created = await page(`
    const {identity} = await id.createIdentity({rpId: 'localhost', userName: 'ada@example.com', userId: 'user-1'});
    window.root = identity;
    return {userPublicKey: identity.userPublicKey};
  `);
  if (created.error) throw new Error(created.error);
  process.stdout.write(`${JSON.stringify({origin, ...created})}\n`);
  for await (const line of createInterface({input: process.stdin})) {
    const request = JSON.parse(line);
    const answer = request.sign !== undefined
      ? await page(`return {signature: id.hex(await window.root.sign(id.unhex(${JSON.stringify(request.sign)})))};`)
      : request.info !== undefined
        ? await page(`return await id.nodeInstallInfo(${JSON.stringify(request.info)});`)
        : await page(`return await id.installNodeCertificate(${JSON.stringify(request.install.sessionCertificate ? request.install : {
          ...request.install,
          // What certifyDevice resolves to: the canonical encoding, base64.
          certificate: Buffer.from(request.install.certificate, 'hex').toString('base64'),
        })});`);
    process.stdout.write(`${JSON.stringify(answer)}\n`);
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
