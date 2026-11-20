// A person who joined the Lounge by invitation, whose certificate the
// workspace's server turns away, is back in without being asked anything, and
// Settings shows the server's answer afterwards.
//
// A fresh person opens an invitation link the Lounge's owner made
// (`stack.sh invite`), joins, and opens Settings at Agent models, which the
// server answers ("Choose a model provider" and the providers to link). Then
// the certificate this browser keeps is damaged as a lapsed one would be: one
// bit of its signature flipped where the page keeps it (its IndexedDB store),
// so the server refuses it when the page comes back. Reloaded, the page:
//
//   - was refused first, as the page's console says ("certificate-rejected");
//   - comes back connected by itself, with a certificate it holds anew
//     (neither the one it had nor the damaged one), for the same person,
//     still a member of the same workspace, with no card asking anything;
//   - shows the server's answer in Settings, not an error or a blank pane.
//
// A page refused as it loads is let in again through its entry, as a new
// device of the same person (the console says "joining as a new device"); a
// page refused while it is open certifies the device it is again with the
// person's root (`recover({kind: 'reauthenticate'})`). Which one it was is
// said in the report (`renewedAs`).
//
// Then the same again with the requests the page certifies through (where it
// learns the server's certificate) refused at the network: the page either
// says in plain words that this browser is not connected and to reload, never
// that it is ready, or it is connected for real; and a reload with the network
// back connects it.
//
// An open page cannot be made to meet the refusal from here: its session holds
// its certificate in memory from when it started, and a browser's certificate
// lasts 30 days. So the open page certifying again, and a Settings pane open
// across the session that replaced its own, are the tests' of peer.mjs,
// session-refusal.mjs and the session actions layer (`replaced_tests.rs`).
//
// usage: node resign-in.mjs SITE_URL SERVER_ORIGIN STACK OUT_DIR
// STACK is os/containers/tilo-sh/stack.sh of the stack the Lounge is on, whose
// owner makes the invitation. With TILO_SH_STATE set, the server's log is read
// for the refusal too. Screenshots of every state go under OUT_DIR.
import {execFileSync} from 'node:child_process';
import {existsSync, mkdirSync, readFileSync, statSync, writeFileSync} from 'node:fs';
import {launch, sleep} from '../../../../tools/web/chrome.mjs';
import {clock} from './timing.mjs';
import {machine, shellMetrics, stepLimit, SHELL} from './waits.mjs';
import {click, key, pageText, returning, searchChord, TREE_TEXT, type, found, pressNamed} from './shell.mjs';

const [site, server, stack, out] = process.argv.slice(2);
if (!site || !server || !stack || !out) {
  console.error('usage: node resign-in.mjs SITE_URL SERVER_ORIGIN STACK OUT_DIR');
  process.exit(2);
}
mkdirSync(out, {recursive: true});
const serverLog = process.env.TILO_SH_STATE ? `${process.env.TILO_SH_STATE}/logs/server.log` : null;
const report = {site, server, at: new Date().toISOString(), shots: []};
const mark = clock();

// What Agent models says once the server answered, and what it says when it
// could not get an answer.
const ANSWERED = ['Choose a model provider', 'ChatGPT', 'Another provider'];
const UNANSWERED = ['could not be read', 'Model provider administration is unavailable', 'Can’t read', 'Can\'t read',
  'The request was cancelled'];
// What the page says when it is not connected to its workspace any more
// (`connection-status.mjs` and `session-refusal.mjs`).
const NOT_CONNECTED = 'isn’t connected to';

const browser = await launch();
const tab = await browser.tab();
await tab.send('Emulation.setDeviceMetricsOverride', shellMetrics).catch(() => {});
await returning(tab);
await tab.send('Network.enable', {}).catch(() => {});
await tab.send('Log.enable', {}).catch(() => {});
// Every request the page sent to learn the server's certificate, when it
// went, and whether it was refused here.
const certifyAsks = [];
let refuseCertify = false;
browser.on(({method, sessionId, params}) => {
  if (sessionId !== tab.sessionId) return;
  // What the browser itself said: a request that failed, a resource it
  // logged, beside the page's console.
  if (method === 'Network.loadingFailed') tab.log.push(`NET ${params.requestId}: ${params.errorText}`);
  if (method === 'Log.entryAdded') tab.log.push(`LOG ${params.entry.level} ${params.entry.text}`.slice(0, 600));
  if (method === 'Network.requestWillBeSent' && /\/peer\/v1\/host$/.test(params.request.url)) {
    certifyAsks.push({at: Date.now(), url: params.request.url, refused: refuseCertify});
  }
  if (method === 'Fetch.requestPaused') {
    const answer = refuseCertify
      ? tab.send('Fetch.failRequest', {requestId: params.requestId, errorReason: 'ConnectionRefused'})
      : tab.send('Fetch.continueRequest', {requestId: params.requestId});
    answer.catch(() => {});
  }
});

const PAGE = `JSON.stringify({
  arrival: document.documentElement.dataset.arrival ?? null,
  entry: document.documentElement.dataset.entry ?? null,
  refusal: document.documentElement.dataset.refusal ?? null,
  connection: document.documentElement.dataset.connection ?? null,
  status: document.getElementById('status')?.textContent?.trim().slice(0, 300) ?? null,
  cards: [...document.querySelectorAll('.tilo-entry-card')].map(card => card.textContent.replace(/\\s+/g, ' ').trim().slice(0, 300)),
  windows: (globalThis.tiloMount?.navigation?.getSnapshot?.().entries ?? []).map(entry => entry.label),
})`;
const pageState = async () => JSON.parse(await tab.evaluate(PAGE).catch(() => '{}'));
const treeText = async () => {
  await tab.evaluate('globalThis.tiloMount?.requestSemantics?.()').catch(() => {});
  await sleep(300);
  return String(await tab.evaluate(TREE_TEXT).catch(() => ''));
};

/** A picture of the page and what it said, as `n-name.png` and `.json`. */
const shoot = async (name, what) => {
  const shown = {what, page: await pageState(), text: (await treeText()).slice(0, 3000),
    body: (await pageText(tab)).slice(0, 2000)};
  writeFileSync(`${out}/${name}.json`, JSON.stringify(shown, null, 2));
  const shot = await tab.screenshot().catch(() => null);
  if (shot) {
    writeFileSync(`${out}/${name}.png`, shot);
    report.shots.push({name: `${name}.png`, what});
  }
  console.log(`shot ${name}.png: ${what}`);
  return shown;
};

const failed = async message => {
  report.error = message;
  report.page = await pageState();
  report.certifyAsks = certifyAsks;
  writeFileSync(`${out}/report.json`, JSON.stringify(report, null, 2));
  writeFileSync(`${out}/page.log`, tab.log.join('\n'));
  await shoot('failed', message).catch(() => {});
  console.error(`--- the page said:\n${tab.log.slice(-40).join('\n')}`);
  console.error(`FAIL: ${message}`);
  console.error(`the machine as it failed: ${machine().text}`);
  await browser.close().catch(() => {});
  process.exit(1);
};

/** Wait until `expression` holds in the page, or fail saying `what` did not
 * happen, with what the page showed. */
const until = async (expression, what, timeout = stepLimit(3)) => {
  const value = await tab.waitFor(expression, {timeout}).catch(() => false);
  if (value) return value;
  await failed(`${what} did not happen in ${timeout / 1000} s: ${JSON.stringify(await pageState())}`);
};

// -- what this browser keeps of its certificate ------------------------------
// Evaluated in the page: the certificates the page keeps, each with its
// signature and whose it is; with `flip`, one bit of each one's signature
// flipped first. Every store of every database the page keeps is searched for
// the entry whose key names the certificate document. The certificate is
// edited as text: its nanosecond timestamps do not survive a JavaScript
// number, and the page starts its session from the exact text.
async function keptCertificates(flip) {
  const done = request => new Promise((resolve, reject) => {
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
  });
  const kept = [];
  // One store's certificates, pushed to `kept`.
  const search = async (db, store) => {
    const ids = (await done(db.transaction(store).objectStore(store).getAllKeys()))
      .filter(id => typeof id === 'string'
        && (id.includes('"document":"certificate"') || id.includes('\\"document\\":\\"certificate\\"')));
    for (const id of ids) {
      const row = await done(db.transaction(store).objectStore(store).get(id));
      const bytes = row?.bytes;
      const view = bytes instanceof ArrayBuffer ? new Uint8Array(bytes)
        : ArrayBuffer.isView(bytes) ? new Uint8Array(bytes.buffer, bytes.byteOffset, bytes.byteLength) : null;
      if (!view) { kept.push({id, unreadable: typeof bytes}); continue; }
      let text = new TextDecoder().decode(view);
      const own = text.indexOf('"cert":');
      const server = text.indexOf('"hostCert":');
      const at = text.indexOf('"signature":[', own);
      if (own < 0 || at < 0 || (server > own && at > server)) { kept.push({id, unreadable: 'no signature'}); continue; }
      const start = at + '"signature":['.length;
      const end = text.indexOf(']', start);
      const signature = text.slice(start, end).split(',').map(Number);
      if (flip) {
        signature[0] ^= 1;
        text = text.slice(0, start) + signature.join(',') + text.slice(end);
        const encoded = new TextEncoder().encode(text);
        const tx = db.transaction(store, 'readwrite');
        tx.objectStore(store).put({...row, bytes: bytes instanceof ArrayBuffer ? encoded.buffer : encoded});
        await new Promise((resolve, reject) => { tx.oncomplete = resolve; tx.onerror = () => reject(tx.error); });
      }
      const hex = list => (list ?? []).map(n => n.toString(16).padStart(2, '0')).join('');
      const cert = JSON.parse(text).cert ?? {};
      kept.push({id, signature: hex(signature), user: hex(cert.user_public_key), device: hex(cert.device_public_key),
        issued: text.match(/"issued_at_ns":(\d+)/)?.[1] ?? null});
    }
  };
  for (const name of (await indexedDB.databases()).map(entry => entry.name)) {
    const db = await done(indexedDB.open(name));
    try {
      for (const store of [...db.objectStoreNames]) await search(db, store);
    } finally {
      db.close();
    }
  }
  return kept;
}
const certificates = async (flip = false) =>
  JSON.parse(await tab.evaluate(`(${keptCertificates.toString()})(${flip}).then(JSON.stringify)`));
/** The certificates this browser keeps that read whole; with `flip`, each
 * one damaged first. A page that comes back as a new device of its person
 * keeps one more, under the new device. */
const readable = async (flip = false) => (await certificates(flip)).filter(entry => entry.signature);
/** A certificate this browser keeps whose signature is none of `seen`, or null. */
const newCertificate = async seen => (await readable()).find(entry => !seen.has(entry.signature)) ?? null;
/** Damage every certificate this browser keeps, and see that the damaged
 * copies stay kept while the page is open. Their entries, before and after. */
const damageAll = async () => {
  const before = await readable();
  const damaged = await readable(true);
  await sleep(1500);
  const kept = await readable();
  const signatures = list => list.map(entry => entry.signature).sort().join();
  if (!before.length || signatures(kept) !== signatures(damaged) || signatures(kept) === signatures(before)) {
    await failed(`the damaged certificates did not stay kept: before ${signatures(before)}, damaged ${signatures(damaged)}, kept ${signatures(kept)}`);
  }
  return {before, damaged};
};
/** The page arrived, connected and drawing, with nothing refused. */
const CONNECTED = `(() => {
  const d = document.documentElement.dataset;
  return d.arrival === 'done' && !d.refusal && !['disconnected', 'offline', 'admission'].includes(d.connection)
    && globalThis.tiloMount?.state?.()?.phase === 'presented' ? JSON.stringify({...d}) : '';
})()`;
// Who this browser joined as, and where.
const membership = async () => JSON.parse(await tab.evaluate(`(() => {
  const all = JSON.parse(globalThis.localStorage.getItem('tilo-host-memberships') ?? '{}');
  return JSON.stringify(all[${JSON.stringify(new URL(server).origin)}] ?? null);
})()`));

// The server's log from `from` bytes on: what it said about a refused device.
const logSize = () => (serverLog && existsSync(serverLog) ? statSync(serverLog).size : 0);
const refusalsLogged = from => {
  if (!serverLog || !existsSync(serverLog)) return [];
  return readFileSync(serverLog).subarray(from).toString('utf8').split('\n')
    .filter(line => /auth reject|certificate/i.test(line)).map(line => line.replace(/\x1b\[[0-9;]*m/g, '').slice(0, 400));
};

// -- Settings at Agent models ------------------------------------------------
/** The shell's search for `query`, choosing the row titled `title` wherever it
 * stands; false when the search offers no such row. */
const choose = async (query, title) => {
  for (let tries = 0; tries < 3; tries++) {
    if (await tab.evaluate('!!globalThis.tiloMount?.state()?.searchOpen').catch(() => false)) break;
    await click(tab, SHELL.empty.x, SHELL.empty.y);
    await key(tab, 'k', await searchChord(tab));
    if (await tab.waitFor('!!globalThis.tiloMount?.state()?.searchOpen', {timeout: stepLimit(1)}).catch(() => false)) break;
  }
  await until('!!globalThis.tiloMount?.state()?.searchOpen', `the search opening for "${query}"`, stepLimit(3));
  await type(tab, query);
  const card = JSON.parse(await until(`(() => {
    const s = globalThis.tiloMount?.state?.() ?? null;
    const card = s?.searchCard ?? null;
    return card && s.searchSearching === false && card.query === ${JSON.stringify(query)} ? JSON.stringify(card) : '';
  })()`, `the search answering "${query}"`, stepLimit(6)));
  const at = card.rows.findIndex(row => row.title === title);
  report[`search ${query}`] = card.rows.map(row => `${row.title} [${row.kind}]`);
  if (at < 0) {
    await key(tab, 'Escape');
    await tab.waitFor('!globalThis.tiloMount?.state()?.searchOpen', {timeout: stepLimit(1)}).catch(() => false);
    return false;
  }
  for (let n = 0; n < at; n++) { await key(tab, 'ArrowDown'); await sleep(300); }
  await key(tab, 'Enter');
  await tab.waitFor('!globalThis.tiloMount?.state()?.searchOpen', {timeout: stepLimit(4)}).catch(() => false);
  return true;
};
/** Settings at Agent models, opened as a person opens it from the search. */
const openModels = async () => {
  if (await choose('agent models', 'Agent models')) return;
  if (!await choose('settings', 'Settings')) await failed(`the search offers neither Agent models nor Settings: ${JSON.stringify(report)}`);
  await tab.evaluate('globalThis.tiloMount?.requestSemantics?.()').catch(() => {});
  await until(found('Agent models'), 'Settings offering Agent models', stepLimit(4));
  if (!await pressNamed(tab, 'Agent models')) await failed('Agent models is not where a person can press it');
};
const answered = text => ANSWERED.every(part => text.includes(part));
const unanswered = text => UNANSWERED.find(part => text.includes(part)) ?? null;
/** Agent models showing the server's answer, within `timeout`; the text the
 * pane showed last otherwise. */
const modelsAnswer = async timeout => {
  const deadline = Date.now() + timeout;
  let text = '';
  while (Date.now() < deadline) {
    text = await treeText();
    if (answered(text)) return {answered: true, text};
    await tab.changed(2000);
  }
  return {answered: false, text};
};
/** Watch for a new certificate after a reload for up to `timeout`, noting
 * when Agent models first showed its answer meanwhile, with a picture of the
 * page while it has none yet (`shot`: its name and what it shows). */
const watchRenewal = async (seen, timeout, shot) => {
  let renewed = null;
  let answeredAt = null;
  let shotTaken = false;
  for (const deadline = Date.now() + timeout; Date.now() < deadline && !renewed;) {
    const found = await newCertificate(seen);
    if (found) renewed = {...found, at: Date.now()};
    if (answeredAt === null && answered(await treeText())) answeredAt = Date.now();
    if (!renewed && !shotTaken) {
      shotTaken = true;
      await shoot(...shot);
    }
    if (!renewed) await sleep(1000);
  }
  return {renewed, answeredAt};
};

/** The page reloaded, into a new document that arrived. */
const reloaded = async what => {
  await tab.evaluate('globalThis.__resignInBefore = true').catch(() => {});
  await tab.reload().catch(() => {});
  return until(`globalThis.__resignInBefore !== true && ['done', 'stopped'].includes(document.documentElement.dataset.arrival)
    && document.documentElement.dataset.arrival`, what, stepLimit(6));
};

try {
  // -- 1. a fresh person joins from an invitation ------------------------------
  const said = execFileSync('bash', [stack, 'invite'], {encoding: 'utf8', stdio: ['ignore', 'pipe', 'inherit']});
  const link = said.match(/https?:\/\/\S+/)?.[0];
  if (!link) await failed(`the owner's invitation gave no link: ${said.slice(0, 400)}`);
  report.link = link.replace(/#.*/, '#…');
  await tab.navigate(link);
  // The card says what the invitation is to ("You’re invited to Lounge", or
  // who invited them where the inviter has a name) and offers Join.
  await until(`(() => {
    const card = [...document.querySelectorAll('form.tilo-entry-card')].at(-1);
    return !!card && /invited/.test(card.textContent)
      && [...card.querySelectorAll('button')].some(button => button.textContent.trim() === 'Join');
  })()`, 'the invitation card', stepLimit(4));
  await shoot('1-invitation', 'the invitation, read: where it is to, as what, and Join');
  const joined = await tab.evaluate(`(() => {
    const card = [...document.querySelectorAll('form.tilo-entry-card')].at(-1);
    const button = card && [...card.querySelectorAll('button')].find(button => button.textContent.trim() === 'Join');
    if (button) button.click();
    return !!button;
  })()`);
  if (!joined) await failed('the invitation card offers no Join');
  const arrived = await until(`['done', 'stopped'].includes(document.documentElement.dataset.arrival)
    && document.documentElement.dataset.arrival`, 'the invited person arriving', stepLimit(6));
  if (arrived !== 'done') await failed(`the invitation did not take: ${JSON.stringify(await pageState())}`);
  await until(`globalThis.tiloMount?.state?.()?.phase === 'presented'`, 'the desktop drawing', stepLimit(6));
  const member = await membership();
  if (!member?.userPublicKey || !member.workspace) await failed(`this browser joined as nobody: ${JSON.stringify(member)}`);
  report.member = member;
  console.log(`ok: ${member.userPublicKey.slice(0, 8)} joined ${member.workspace} from the invitation`);
  mark('joined by invitation');

  // -- 2. Settings at Agent models, answered by the server ---------------------
  await openModels();
  const first = await modelsAnswer(stepLimit(6));
  if (!first.answered) await failed(`Agent models never showed the server's answer: ${first.text.slice(0, 1500)}`);
  console.log('ok: Agent models shows the server\'s answer: Choose a model provider, ChatGPT, Another provider');
  await shoot('2-agent-models', 'Settings at Agent models, answered by the server');
  mark('Agent models answered');

  // -- 3. the certificate turned away, and the page signing in again -----------
  const first3 = await damageAll();
  const original = first3.before[0];
  report.original = original;
  report.damaged = first3.damaged.map(entry => entry.signature);
  console.log(`ok: the kept certificate damaged (signature ${original.signature.slice(0, 8)}… to ${first3.damaged[0].signature.slice(0, 8)}…)`);
  const seen = new Set([...first3.before, ...first3.damaged].map(entry => entry.signature));
  const logFrom = logSize();
  const consoleFrom = tab.log.length;
  const reloadedAt = Date.now();
  await reloaded('the page arriving again with the damaged certificate');
  const {renewed, answeredAt} = await watchRenewal(seen, stepLimit(8),
    ['3-refused-and-signing-in', 'the page back from the reload, its certificate refused, while it signs in again']);
  if (!renewed) {
    await failed(`the page never kept a new certificate after its damaged one was refused (it keeps ${JSON.stringify((await readable()).map(entry => entry.signature.slice(0, 16)))})`);
  }
  report.renewed = renewed;
  console.log(`ok: a new certificate kept ${((renewed.at - reloadedAt) / 1000).toFixed(1)} s after the reload (signature ${renewed.signature.slice(0, 8)}…, issued ${renewed.issued})`);
  if (renewed.user !== original.user) await failed(`the new certificate is another person's: ${renewed.user} for ${original.user}`);
  // How the page got its new certificate: certified again for the device it
  // was (the page's own re-sign-in), or let in again as a new device of the
  // same person (its entry's way back).
  const said3 = tab.log.slice(consoleFrom);
  report.renewedAs = renewed.device === original.device ? 'the same device, certified again' : 'a new device of the same person';
  report.renewedConsole = said3.filter(line => /certif|new device|join again|refused/i.test(line)).slice(0, 12);
  console.log(`ok: the new certificate is the same person's, for ${report.renewedAs}`);
  for (const line of report.renewedConsole) console.log(`  the page said: ${line.slice(0, 240)}`);
  mark('signed in again');

  // The refusal came first: the server says it turned the damaged
  // certificate away, or the page says its session was refused for it.
  const serverSaid = refusalsLogged(logFrom);
  const pageSaid = said3.filter(line => /certificate-rejected|refused this browser’s certificate/i.test(line));
  report.refusal = {server: serverSaid.slice(0, 10), page: pageSaid.slice(0, 10)};
  if (!serverSaid.length && !pageSaid.length) {
    await failed(`neither the server's log nor the page says the damaged certificate was refused (server log ${serverLog ?? 'unknown'})`);
  }
  console.log(`ok: the damaged certificate was refused first: ${(serverSaid[0] ?? pageSaid[0]).slice(0, 200)}`);

  // Connected, as the same member, with nothing asked of the person.
  report.back = JSON.parse(await until(CONNECTED, 'the page connected again', stepLimit(4)));
  const state = await pageState();
  if (state.cards.length) await failed(`the page asked something of the person while it signed in again: ${JSON.stringify(state.cards)}`);
  const memberAfter = await membership();
  if (memberAfter?.userPublicKey !== member.userPublicKey || memberAfter?.workspace !== member.workspace) {
    await failed(`the person is not the same member after signing in again: ${JSON.stringify(memberAfter)} for ${JSON.stringify(member)}`);
  }
  console.log(`ok: connected again as ${member.userPublicKey.slice(0, 8)} in ${member.workspace}, with nothing asked`);

  // Settings shows the server's answer: the pane the reload brought back, or
  // the one opened again.
  let models = await modelsAnswer(stepLimit(1));
  if (!models.answered && await tab.evaluate(found('Agent models'))) {
    // The Settings window came back with the page, at its first page: a
    // person presses Agent models in its sidebar.
    report.restoredPane = {text: models.text.slice(0, 600), windows: (await pageState()).windows};
    await shoot('3-restored-pane', 'the Settings window the reload brought back, before Agent models was pressed');
    await pressNamed(tab, 'Agent models');
    models = await modelsAnswer(stepLimit(6));
  }
  report.settingsRestored = models.answered;
  if (!models.answered) {
    const wrong = unanswered(models.text);
    report.restoredPane = {said: wrong, text: models.text.slice(0, 1500), windows: (await pageState()).windows};
    console.log(`note: the Settings pane the reload brought back did not show the server's answer${wrong ? ` (it said "${wrong}")` : ''}; its windows ${JSON.stringify(report.restoredPane.windows)}; opening Agent models again`);
    await shoot('3-pane-unanswered', 'the page after the reload, before Agent models was opened again from the search');
    await openModels();
    models = await modelsAnswer(stepLimit(6));
  }
  if (!models.answered) await failed(`Agent models did not show the server's answer after signing in again: ${models.text.slice(0, 1500)}`);
  if (unanswered(models.text)) await failed(`Agent models shows an error beside its answer: "${unanswered(models.text)}"`);
  report.answeredBeforeRenewed = answeredAt !== null && answeredAt < renewed.at;
  console.log(`ok: Agent models shows the server's answer after signing in again (${report.settingsRestored ? 'the pane the reload brought back' : 'opened again'}${report.answeredBeforeRenewed ? ', answered before the new certificate was kept' : ''})`);
  await shoot('4-signed-in-again', 'connected again with a new certificate, Agent models answered by the server');
  mark('Settings answered after signing in again');

  // -- 4. signing in again fails too: the page says it is not connected --------
  const second = await damageAll();
  for (const entry of [...second.before, ...second.damaged]) seen.add(entry.signature);
  const consoleFrom4 = tab.log.length;
  refuseCertify = true;
  await tab.send('Fetch.enable', {patterns: [{urlPattern: '*/peer/v1/host', requestStage: 'Request'}]});
  const refusedFrom = certifyAsks.length;
  await reloaded('the page arriving with a damaged certificate and no way to certify');
  // Either the page says it is not connected, or it got in some other way
  // than certifying: which one is reported, and each is judged as itself.
  const outcome = JSON.parse(await until(`(() => {
    const d = document.documentElement.dataset;
    const text = document.body.innerText || '';
    if (d.refusal || d.connection === 'disconnected' || text.includes(${JSON.stringify(NOT_CONNECTED)})) {
      return JSON.stringify({said: true, refusal: d.refusal ?? null, connection: d.connection ?? null, arrival: d.arrival ?? null});
    }
    const connected = ${CONNECTED};
    return connected ? JSON.stringify({said: false, connected: JSON.parse(connected)}) : '';
  })()`, 'the page saying it is not connected, or connecting', stepLimit(8)));
  // The words settle a moment after the state: the card, the bar or both.
  if (outcome.said) await tab.waitFor(`(document.body.innerText || '').includes(${JSON.stringify(NOT_CONNECTED)})`, {timeout: stepLimit(2)}).catch(() => false);
  const words = await pageText(tab);
  const failedState = await pageState();
  const refusedAsks = certifyAsks.slice(refusedFrom).filter(ask => ask.refused).length;
  const said4 = tab.log.slice(consoleFrom4);
  report.notConnected = {outcome, page: failedState, refusedAsks,
    console: said4.filter(line => /certif|new device|join again|refused|not connected/i.test(line)).slice(0, 12)};
  for (const line of report.notConnected.console) console.log(`  the page said: ${line.slice(0, 240)}`);
  if (outcome.said) {
    if (!words.includes(NOT_CONNECTED)) await failed(`the page refused, but does not say this browser is not connected: ${words.slice(0, 600)}`);
    if (!/Reload/.test(words)) await failed(`the page does not say to reload: ${words.slice(0, 600)}`);
    if (failedState.connection === 'online' || /\bready\b/i.test(failedState.status ?? '')) {
      await failed(`the page says it is connected while it is not: ${JSON.stringify(failedState)}`);
    }
    console.log(`ok: certifying again failed (${refusedAsks} asks refused) and the page says: ${words.match(/This browser isn’t connected[^.]*\.[^.]*\./)?.[0] ?? words.slice(0, 200)}`);
    await shoot('5-not-connected', 'certifying again failed: the page says this browser is not connected, and to reload');
  } else {
    const got = await newCertificate(seen);
    console.log(`note: with certifying refused (${refusedAsks} asks), the page still got in, ${got ? `with a certificate for ${got.device === original.device ? 'the same device' : 'another device'}` : 'with no new certificate'}`);
    await shoot('5-in-without-certifying', 'certifying refused at the network, and the page got in another way');
  }
  mark('certifying refused');

  // -- 5. the reload it asks for connects it again -----------------------------
  refuseCertify = false;
  await tab.send('Fetch.disable').catch(() => {});
  await reloaded('the page arriving again with the network back');
  await until(CONNECTED, 'the page connected after the reload it asked for', stepLimit(8));
  if ((await membership())?.userPublicKey !== member.userPublicKey) await failed('the reload made this browser someone else');
  let last = await modelsAnswer(stepLimit(1));
  if (!last.answered && await tab.evaluate(found('Agent models'))) {
    await pressNamed(tab, 'Agent models');
    last = await modelsAnswer(stepLimit(6));
  }
  if (!last.answered) {
    console.log(`note: after the last reload the Settings pane did not show the server's answer; its windows ${JSON.stringify((await pageState()).windows)}`);
    await shoot('6-restored-pane', 'the page after the last reload, before Agent models was opened again');
    await openModels();
    last = await modelsAnswer(stepLimit(6));
  }
  if (!last.answered) await failed(`Agent models did not show the server's answer after the reload: ${last.text.slice(0, 1500)}`);
  console.log('ok: the reload the page asked for connected it again, and Agent models answers');
  await shoot('6-reloaded', 'the reload the page asked for: connected again, Agent models answered');
  mark('connected after the reload');

  report.certifyAsks = certifyAsks.map(ask => ({...ask, at: ask.at - reloadedAt}));
  writeFileSync(`${out}/report.json`, JSON.stringify(report, null, 2));
  writeFileSync(`${out}/page.log`, tab.log.join('\n'));
  const thrown = tab.log.filter(line => /^EXC|panic/i.test(line));
  if (thrown.length) console.log(`the page threw meanwhile (not judged here): ${thrown.slice(0, 3).join(' | ').slice(0, 600)}`);
  mark();
} catch (error) {
  await failed(`the flow stopped: ${error?.stack ?? error}`);
} finally {
  await browser.close().catch(() => {});
}
