// The page half of the browser tests that drive the real page modules in
// headless Chrome: serve the page's sources beside a built runtime bundle
// (`pkg/`, from `page-bundle.mjs`), open a tab, and run one mode's steps over
// stdio.
//
// usage: node support/page-driver.mjs node
//        node support/page-driver.mjs call FIXTURE_URL WHO...
//
// `node` creates an identity, prints `{origin, userPublicKey}`, then answers
// one JSON request per stdin line until stdin closes:
//   {"sign": "<hex>"}                            -> {"signature": "<hex>"}
//   {"info": {origin, devicePublicKey, installToken}}
//                                                -> what nodeInstallInfo resolves to
//   {"install": {origin, certificate, sessionCertificate?, installToken}}
//     (certificates canonical, hex)              -> what installNodeCertificate resolves to
// Any failure is answered as {"error": "...", "code": "..."}.
//
// `call` opens one tab per WHO at `FIXTURE_URL/page.html?who=WHO`, in a
// browser with a synthetic microphone, prints `{ready: [WHO...]}` once every
// tab says `window.ready`, then answers one JSON request per stdin line until
// stdin closes:
//   {"tab": WHO, "run": "<expression>", "gesture": bool}
//                                                -> {"value": ...}
//   {"tab": WHO, "waitFor": "<expression>", "timeout": ms}
//                                                -> {"value": true}
// A failure is answered as {"error": "...", "log": [what the tab logged]}.
import {createInterface} from 'node:readline';
import {fileURLToPath} from 'node:url';
import {launch, serve} from '../../../../../tools/web/chrome.mjs';
import {pageBundle} from '../../../../../tools/web/page-bundle.mjs';

const mode = process.argv[2];
if (mode === 'call') {
  await callMembers(process.argv[3], process.argv.slice(4));
  process.exit(process.exitCode ?? 0);
}
if (mode !== 'node') throw new Error('usage: page-driver.mjs node | call FIXTURE_URL WHO...');
const web = fileURLToPath(new URL('../../../../browser/tilo-browser-runtime/web/', import.meta.url));
const base64 = hex => Buffer.from(hex, 'hex').toString('base64');

const bundle = pageBundle();
const server = await serve(web, {also: [bundle.dir]});
const origin = server.url.replace('127.0.0.1', 'localhost');
const browser = await launch();
let failed = false;
try {
  const tab = await browser.tab();
  await tab.navigate(`${origin}/identity.mjs`);
  await tab.waitFor('document.readyState === "complete"', {timeout: 30_000});
  /** `expression` run in the page's identity module; a throw is answered as
   * `{error, code}`. */
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

  const created = await run(`
    const kept = new Map();
    const storage = {get: async key => kept.get(key), put: async (key, value) => { kept.set(key, value); }, delete: async key => { kept.delete(key); }};
    const identity = await id.browserIdentity({storage});
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

/** The `call` mode: one tab per call member, each the fixture's page for
 * `who`, driven over stdio. The browser is closed however the run ends. */
async function callMembers(base, members) {
  if (!base || !members.length) throw new Error('usage: page-driver.mjs call FIXTURE_URL WHO...');
  const browser = await launch({media: true});
  try {
    const tabs = new Map();
    for (const who of members) {
      const tab = await browser.tab();
      await tab.navigate(`${base}/page.html?who=${encodeURIComponent(who)}`);
      await tab.waitFor('window.ready === true', {timeout: 30_000});
      tabs.set(who, tab);
    }
    process.stdout.write(`${JSON.stringify({ready: members})}\n`);
    for await (const line of createInterface({input: process.stdin})) {
      const request = JSON.parse(line);
      const tab = tabs.get(request.tab);
      let answer;
      try {
        if (!tab) throw new Error(`no tab ${request.tab}`);
        if (request.waitFor !== undefined) {
          await tab.waitFor(request.waitFor, {timeout: request.timeout ?? 30_000});
          answer = {value: true};
        } else {
          const reply = await tab.send('Runtime.evaluate', {
            expression: request.run, awaitPromise: true, returnByValue: true, userGesture: request.gesture === true,
          });
          if (reply.error || reply.result.exceptionDetails) throw new Error(JSON.stringify(reply.error ?? reply.result.exceptionDetails));
          answer = {value: reply.result.result.value ?? null};
        }
      } catch (error) {
        answer = {error: String(error?.message ?? error), log: tab?.log.slice(-40) ?? []};
      }
      process.stdout.write(`${JSON.stringify(answer)}\n`);
    }
  } catch (error) {
    process.exitCode = 1;
    console.error(error);
  } finally {
    await browser.close();
  }
}
