// The front entry of a workspace that takes people by invitation, in a
// browser that has never been here: the wait list shows instead of the setup
// screen, the address is left and confirmed through the mail the entry sends,
// and the tab stays on the wait list when it reloads. The site is the web
// entry's bundle served beside the relay, as the image serves it.
//
// usage: node wait-list.mjs SITE_URL RELAY_ORIGIN MAIL_DIR OUT_DIR [BEFORE_URL]
//
// Prints a JSON report and keeps the screenshots and the tab's console in
// OUT_DIR; exits non-zero when a step did not happen or the page failed.
import {mkdirSync, readdirSync, readFileSync, writeFileSync} from 'node:fs';
import {join} from 'node:path';
import {launch, pinnedCertificates, servedCertificate, sleep} from '../../../../tools/web/chrome.mjs';
import {stepLimit, shellMetrics} from './waits.mjs';

const [site, relay, mailDir, out, before] = process.argv.slice(2);
if (!site || !relay || !mailDir || !out) {
  console.error('usage: node wait-list.mjs SITE_URL RELAY_ORIGIN MAIL_DIR OUT_DIR [BEFORE_URL]');
  process.exit(2);
}
mkdirSync(out, {recursive: true});
const report = {site, relay, at: new Date().toISOString()};

// The entry's page loads the site's and the host's own certificates, and the
// "before" site's too, which the same mint signs. A browser loads these and no
// others, as a person's loads what it trusts.
const siteCertificate = await servedCertificate(site);
const browser = await launch({args: [pinnedCertificates([siteCertificate, await servedCertificate(relay)])]});
// A stranger's browser: nothing kept from an earlier visit, so the entry
// page opens on its first screen unless the entry takes its place.
const tab = await browser.tab({firstRun: true});
await tab.send('Page.enable', {});
await tab.send('Emulation.setDeviceMetricsOverride', shellMetrics);

const failures = () => tab.log.filter(line => /^(EXC|ERROR) /.test(line) || /panic/i.test(line));
// The first screen of a first visit (`#first-run` in the entry page) stays in
// the document once it is done with, hidden, so the person's card can show it
// again: what shows is whether the page set `data-first-run` and displayed it.
const firstScreenShown = `(document.documentElement.dataset.firstRun !== undefined
  && getComputedStyle(document.getElementById('first-run')).display !== 'none')`;
const shot = async name => {
  const png = await tab.screenshot().catch(() => null);
  if (png) writeFileSync(join(out, name), png);
};
const finish = async (code, message) => {
  if (message) report.error = message;
  report.failures = failures();
  report.console = tab.log.slice(-60);
  writeFileSync(join(out, 'console.log'), tab.log.join('\n'));
  writeFileSync(join(out, 'report.json'), JSON.stringify(report, null, 2));
  await browser.close().catch(() => {});
  if (message) {
    if (tab.log.length) console.error(`--- the page said:\n${tab.log.slice(-30).join('\n')}`);
    console.error(`FAIL: ${message}`);
  }
  console.log(JSON.stringify(report));
  process.exit(code);
};
const checkPage = async step => {
  const found = failures();
  if (found.length) await finish(1, `the page failed while ${step}: ${found.slice(0, 5).join(' | ')}`);
};
// Wait until `expression` holds in the page, or say what the entry showed
// instead. The page does the waiting: it reads the expression again on each
// change it shows, and a step fails the moment the page reports a failure.
const until = async (expression, what, timeout = stepLimit(4)) => {
  let stop = () => {};
  const broke = new Promise(resolve => {
    if (failures().length) return resolve(true);
    const listening = browser.on(({sessionId, method, params}) => {
      if (sessionId !== tab.sessionId) return;
      const reported = method === 'Runtime.exceptionThrown'
        || (method === 'Runtime.consoleAPICalled' && ['error', 'warning'].includes(params.type));
      if (!reported) return;
      // The tab's own listener logs this same event, in this same turn.
      if (failures().length) { listening(); resolve(true); }
    });
    stop = listening;
  });
  const answered = await Promise.race([
    tab.waitFor(expression, {timeout}).then(value => ({value}), () => ({})),
    broke.then(() => ({broke: true})),
  ]);
  stop();
  await checkPage(what);
  if (answered.value) return answered.value;
  const shown = await tab.evaluate(`JSON.stringify({
    arrival: document.documentElement.dataset.arrival ?? null,
    refusal: document.documentElement.dataset.refusal ?? null,
    waitList: document.documentElement.dataset.waitList ?? null,
    entry: document.documentElement.dataset.entry ?? null,
    firstScreen: ${firstScreenShown},
    status: document.getElementById('status')?.textContent?.trim().slice(0, 300) ?? null,
  })`).catch(() => 'unreadable');
  await finish(1, `${what} never happened: ${shown}`);
};

// 0. What a stranger saw before the wait list came back: the setup screen,
// at a site whose home nothing admits them to. Taken from a browser of its
// own, so neither visit sees the other's storage.
if (before) {
  const was = await browser.tab({firstRun: true});
  await was.send('Emulation.setDeviceMetricsOverride', shellMetrics);
  await was.navigate(before);
  await was.waitFor(firstScreenShown, {timeout: stepLimit(12)}).catch(() => false);
  const png = await was.screenshot().catch(() => null);
  if (png) writeFileSync(join(out, '0-before-today.png'), png);
  report.before = await was.evaluate(`({
    firstScreen: ${firstScreenShown},
    heading: document.querySelector('#first-run h1')?.textContent?.trim() ?? null,
    refusal: document.documentElement.dataset.refusal ?? null,
  })`).catch(() => null);
  console.log(`ok: the before shot (${report.before?.heading})`);
  await was.close().catch(() => {});
}

// 1. The wait list is the front entry: the setup screen is not over it.
await tab.navigate(site);
await until(`document.documentElement.dataset.refusal === 'wait-list'`, 'the entry showing its wait list');
await until(`document.documentElement.dataset.waitList === 'form'`, 'the wait list offering its field');
await until(`!${firstScreenShown}`, 'the setup screen leaving the entry');
report.heading = await tab.evaluate(`document.getElementById('arrival-refusal-heading')?.textContent?.trim() ?? null`);
report.body = await tab.evaluate(`document.getElementById('arrival-refusal-body')?.textContent?.trim() ?? null`);
report.note = await tab.evaluate(`document.getElementById('arrival-wait-note')?.textContent?.trim() ?? null`);
report.button = await tab.evaluate(`document.querySelector('#arrival-wait button')?.textContent?.trim() ?? null`);
report.invite = await tab.evaluate(`document.getElementById('arrival-invite-toggle')?.textContent?.trim() ?? null`);
await shot('1-wait-list.png');
console.log(`ok: the wait list is the front entry (${report.heading})`);

// 2. The address is left, and the page says to look in its inbox.
const email = 'ada@example.org';
await tab.evaluate(`(() => {
  const field = document.getElementById('arrival-wait-email');
  field.value = ${JSON.stringify(email)};
  field.dispatchEvent(new Event('input', {bubbles: true}));
  document.querySelector('#arrival-wait button').click();
})()`);
await until(`document.documentElement.dataset.waitList === 'inbox'`, 'the address being left, waiting to be confirmed');
await until(`document.documentElement.hasAttribute('data-wait-listed')`, 'the page saying where the address stands');
report.inbox = await tab.evaluate(`document.getElementById('arrival-wait-done')?.textContent?.trim() ?? null`);
await shot('2-waiting-for-confirmation.png');
console.log(`ok: the address was left (${report.inbox})`);

// 3. The confirmation mail went to the sink, and its link lists the address.
const mailSent = async () => {
  // The outbox sends in its own task; the first attempt is prompt.
  const deadline = Date.now() + stepLimit(2);
  for (;;) {
    const files = readdirSync(mailDir).filter(name => name.endsWith('.json')).sort();
    if (files.length) return JSON.parse(readFileSync(join(mailDir, files[0]), 'utf8'));
    if (Date.now() > deadline) return null;
    await sleep(250);
  }
};
const mail = await mailSent();
if (!mail) await finish(1, 'the entry mailed nothing to the sink');
report.mail = {to: mail.to, subject: mail.subject, from: mail.from};
const link = name => (mail.text.match(new RegExp(`https?://[^\\s"'<>]*${name}=[0-9a-f]{64}`)) ?? [])[0] ?? null;
const confirm = link('wait-list-confirm');
if (!confirm) await finish(1, `no confirmation link in the mail: ${mail.text.slice(0, 400)}`);
console.log(`ok: the confirmation mail reached ${mail.to}`);

// 4. The tab reloads: it stays on the wait list, now confirmed.
await tab.navigate(confirm);
await until(`document.documentElement.dataset.waitList === 'listed'`, 'the confirmation link listing the address');
report.listed = await tab.evaluate(`document.getElementById('arrival-wait-done')?.textContent?.trim() ?? null`);
await tab.navigate(site);
await until(`document.documentElement.dataset.refusal === 'wait-list'`, 'the entry after a reload');
await until(`document.documentElement.dataset.waitList === 'listed'`, 'the confirmed address on a reload');
report.reloadListed = await tab.evaluate(`document.getElementById('arrival-wait-done')?.textContent?.trim() ?? null`);
await shot('3-on-the-list.png');
console.log(`ok: a reload stays on the wait list (${report.reloadListed})`);

// 5. A phone, in German: the same entry, its field stacked under its button.
const phone = await browser.tab({firstRun: true});
await phone.send('Page.addScriptToEvaluateOnNewDocument', {
  source: `Object.defineProperty(navigator, 'language', {get: () => 'de-DE'});`,
});
await phone.send('Emulation.setDeviceMetricsOverride', {width: 390, height: 844, deviceScaleFactor: 2, mobile: true});
await phone.navigate(site);
await phone.waitFor(`document.documentElement.dataset.waitList === 'form'`, {timeout: stepLimit(24)}).catch(() => false);
report.phone = await phone.evaluate(`({
  waitList: document.documentElement.dataset.waitList ?? null,
  heading: document.getElementById('arrival-refusal-heading')?.textContent?.trim() ?? null,
  button: document.querySelector('#arrival-wait button')?.textContent?.trim() ?? null,
  note: document.getElementById('arrival-wait-note')?.textContent?.trim() ?? null,
  stacked: (() => {
    const form = document.getElementById('arrival-wait');
    return form ? getComputedStyle(form).gridTemplateColumns.split(' ').length > 1 : null;
  })(),
})`).catch(() => null);
const phoneShot = await phone.screenshot().catch(() => null);
if (phoneShot) writeFileSync(join(out, '4-phone-german.png'), phoneShot);
console.log(`ok: a phone in German (${report.phone?.heading})`);
await phone.close().catch(() => {});

// 6. Nothing behind the entry opened: no membership, no shell.
report.memberships = await tab.evaluate(`globalThis.localStorage?.getItem('tilo-host-memberships') ?? null`);
report.arrival = await tab.evaluate(`document.documentElement.dataset.arrival`);
if (report.memberships) await finish(1, `a stranger at the wait list was let into the workspace: ${report.memberships}`);
await checkPage('waiting at the entry');
await finish(0);
