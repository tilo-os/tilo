// The front door of a workspace that takes people by invitation, in a
// browser that has never been here: the wait list shows instead of the setup
// screen, the address is left and confirmed through the mail the door sends,
// and the tab stays on the wait list when it reloads. The site is the web
// entry's bundle served beside the relay, as the image serves it.
//
// usage: node wait-list-door.mjs SITE_URL MAIL_DIR OUT_DIR [BEFORE_URL]
//
// Prints a JSON report and keeps the screenshots and the tab's console in
// OUT_DIR; exits non-zero when a step did not happen or the page failed.
import {mkdirSync, readdirSync, readFileSync, writeFileSync} from 'node:fs';
import {join} from 'node:path';
import {launch, sleep} from '../../../../tools/web/chrome.mjs';

const [site, mailDir, out, before] = process.argv.slice(2);
if (!site || !mailDir || !out) {
  console.error('usage: node wait-list-door.mjs SITE_URL MAIL_DIR OUT_DIR [BEFORE_URL]');
  process.exit(2);
}
mkdirSync(out, {recursive: true});
const report = {site, at: new Date().toISOString()};

const browser = await launch({args: [
  '--ignore-certificate-errors',
  '--enable-features=Vulkan', '--use-vulkan=swiftshader', '--use-angle=swiftshader',
  '--disable-dev-shm-usage',
  ...(process.getuid?.() === 0 ? ['--no-sandbox'] : []),
]});
// A stranger's browser: nothing kept from an earlier visit, so the entry
// page opens on its first screen unless the door takes its place.
const tab = await browser.tab({firstRun: true});
await tab.send('Page.enable', {});
await tab.send('Emulation.setDeviceMetricsOverride', {width: 1280, height: 800, deviceScaleFactor: 1, mobile: false});

const failures = () => tab.log.filter(line => /^(EXC|ERROR) /.test(line) || /panic/i.test(line));
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
const until = async (expression, what, timeout = 180_000) => {
  const deadline = Date.now() + timeout;
  for (;;) {
    await checkPage(what);
    const value = await tab.evaluate(expression).catch(() => false);
    if (value) return value;
    if (Date.now() > deadline) {
      const shown = await tab.evaluate(`JSON.stringify({
        arrival: document.documentElement.dataset.arrival ?? null,
        door: document.documentElement.dataset.door ?? null,
        waitList: document.documentElement.dataset.waitList ?? null,
        entry: document.documentElement.dataset.entry ?? null,
        firstScreen: Boolean(document.getElementById('first-run')),
        status: document.getElementById('status')?.textContent?.trim().slice(0, 300) ?? null,
      })`).catch(() => 'unreadable');
      await finish(1, `${what} never happened: ${shown}`);
    }
    await sleep(250);
  }
};

// 0. What a stranger saw before the wait list came back: the setup screen,
// at a site whose home nothing admits them to. Taken from a browser of its
// own, so neither visit sees the other's storage.
if (before) {
  const was = await browser.tab({firstRun: true});
  await was.send('Emulation.setDeviceMetricsOverride', {width: 1280, height: 800, deviceScaleFactor: 1, mobile: false});
  await was.navigate(before);
  for (let tries = 0; tries < 240; tries += 1) {
    const shown = await was.evaluate(`!!document.getElementById('first-run')`).catch(() => false);
    if (shown) break;
    await sleep(250);
  }
  const png = await was.screenshot().catch(() => null);
  if (png) writeFileSync(join(out, '0-before-today.png'), png);
  report.before = await was.evaluate(`({
    firstScreen: !!document.getElementById('first-run'),
    heading: document.querySelector('#first-run h1')?.textContent?.trim() ?? null,
    door: document.documentElement.dataset.door ?? null,
  })`).catch(() => null);
  console.log(`ok: the before shot (${report.before?.heading})`);
  await was.close().catch(() => {});
}

// 1. The wait list is the front door: the setup screen is not over it.
await tab.navigate(site);
await until(`document.documentElement.dataset.door === 'wait-list'`, 'the door showing its wait list');
await until(`document.documentElement.dataset.waitList === 'form'`, 'the wait list offering its field');
await until(`!document.getElementById('first-run')`, 'the setup screen leaving the door');
report.heading = await tab.evaluate(`document.getElementById('arrival-door-heading')?.textContent?.trim() ?? null`);
report.body = await tab.evaluate(`document.getElementById('arrival-door-body')?.textContent?.trim() ?? null`);
report.note = await tab.evaluate(`document.getElementById('arrival-wait-note')?.textContent?.trim() ?? null`);
report.button = await tab.evaluate(`document.querySelector('#arrival-wait button')?.textContent?.trim() ?? null`);
report.invite = await tab.evaluate(`document.getElementById('arrival-invite-toggle')?.textContent?.trim() ?? null`);
await shot('1-wait-list.png');
console.log(`ok: the wait list is the front door (${report.heading})`);

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
  const deadline = Date.now() + 60_000;
  for (;;) {
    const files = readdirSync(mailDir).filter(name => name.endsWith('.json')).sort();
    if (files.length) return JSON.parse(readFileSync(join(mailDir, files[0]), 'utf8'));
    if (Date.now() > deadline) return null;
    await sleep(250);
  }
};
const mail = await mailSent();
if (!mail) await finish(1, 'the door mailed nothing to the sink');
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
await until(`document.documentElement.dataset.door === 'wait-list'`, 'the door after a reload');
await until(`document.documentElement.dataset.waitList === 'listed'`, 'the confirmed address on a reload');
report.reloadListed = await tab.evaluate(`document.getElementById('arrival-wait-done')?.textContent?.trim() ?? null`);
await shot('3-on-the-list.png');
console.log(`ok: a reload stays on the wait list (${report.reloadListed})`);

// 5. A phone, in German: the same door, its field stacked under its button.
const phone = await browser.tab({firstRun: true});
await phone.send('Page.addScriptToEvaluateOnNewDocument', {
  source: `Object.defineProperty(navigator, 'language', {get: () => 'de-DE'});`,
});
await phone.send('Emulation.setDeviceMetricsOverride', {width: 390, height: 844, deviceScaleFactor: 2, mobile: true});
await phone.navigate(site);
for (let tries = 0; tries < 480; tries += 1) {
  const ready = await phone.evaluate(`document.documentElement.dataset.waitList === 'form'`).catch(() => false);
  if (ready) break;
  await sleep(250);
}
report.phone = await phone.evaluate(`({
  waitList: document.documentElement.dataset.waitList ?? null,
  heading: document.getElementById('arrival-door-heading')?.textContent?.trim() ?? null,
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

// 6. Nothing behind the door opened: no membership, no desk.
report.memberships = await tab.evaluate(`globalThis.localStorage?.getItem('tilo-host-memberships') ?? null`);
report.arrival = await tab.evaluate(`document.documentElement.dataset.arrival`);
if (report.memberships) await finish(1, `a stranger at the wait list was let into the workspace: ${report.memberships}`);
await checkPage('waiting at the door');
await finish(0);
