// One page on the web entry while the live plane cannot be reached: it says
// so where its members would be, once, and says it in its console — the page
// a silent retry makes look like a workspace nobody is in.
//
// usage: node live-note.mjs SITE_URL LIVE_HOST OUT_DIR
// LIVE_HOST is the host the live plane's origin names, resolved here so the
// page's attempt goes where a deployment's would. Prints a JSON report and
// saves what the page showed; exits non-zero when the page said nothing.
import {mkdirSync, writeFileSync} from 'node:fs';
import {launch, sleep} from '../../../../tools/web/chrome.mjs';

const [site, liveHost, out] = process.argv.slice(2);
if (!site || !liveHost || !out) {
  console.error('usage: node live-note.mjs SITE_URL LIVE_HOST OUT_DIR');
  process.exit(2);
}
mkdirSync(out, {recursive: true});

const browser = await launch({args: [
  '--ignore-certificate-errors',
  '--enable-features=Vulkan', '--use-vulkan=swiftshader', '--use-angle=swiftshader',
  `--host-resolver-rules=MAP ${liveHost} 127.0.0.1`,
  '--disable-dev-shm-usage',
  ...(process.getuid?.() === 0 ? ['--no-sandbox'] : []),
]});
const report = {site, liveHost, at: new Date().toISOString()};
const failed = message => {
  report.error = message;
  writeFileSync(`${out}/report.json`, JSON.stringify(report, null, 2));
  console.error(`FAIL: ${message}`);
  process.exit(1);
};
const until = async (read, timeout) => {
  const deadline = Date.now() + timeout;
  for (;;) {
    const value = await read().catch(() => null);
    if (value) return value;
    if (Date.now() > deadline) return null;
    await sleep(250);
  }
};
try {
  const context = await browser.context();
  const tab = await browser.tab({context});
  await tab.send('Emulation.setDeviceMetricsOverride',
    {width: 1280, height: 800, deviceScaleFactor: 1, mobile: false}).catch(() => {});
  await tab.navigate(site);
  const arrival = await until(async () => {
    const said = await tab.evaluate(`document.documentElement.dataset.arrival ?? null`);
    return ['done', 'stopped'].includes(said) ? said : null;
  }, 240_000);
  if (arrival !== 'done') failed(`the page did not arrive: ${arrival}`);
  report.arrival = arrival;

  // The plane fails twice before the page says so (it asks again within a few
  // seconds), so this is the page's own answer and not a flash.
  const warning = await until(async () => tab.log.find(line => /^WARNING live: /.test(line)), 120_000);
  if (!warning) {
    const said = await tab.evaluate(`JSON.stringify({people: globalThis.tiloMount?.people?.() ?? null,
      channels: globalThis.tiloChannels?.() ?? ''})`);
    failed(`the page shows no live presence and says nothing: ${said}`);
  }
  report.note = warning.replace(/^WARNING live: /, '');
  report.people = JSON.parse(await tab.evaluate('JSON.stringify(globalThis.tiloMount?.people?.() ?? null)'));
  report.channels = await tab.evaluate('globalThis.tiloChannels?.() ?? ""');
  report.log = tab.log.slice(-20);

  if (!report.note) failed(`the page logged an empty note: ${warning}`);
  if (!Array.isArray(report.people) || report.people.length) {
    failed(`the page lists people without a live plane: ${JSON.stringify(report.people)}`);
  }
  if (!/not joined — no live plane was reached/.test(report.channels)) {
    failed(`the page's channels do not name the plane it could not reach: ${report.channels}`);
  }
  const warned = report.log.filter(line => /^WARNING live: /.test(line));
  if (warned.length !== 1) {
    failed(`the page said it ${warned.length} time(s), not once: ${report.log.join(' | ')}`);
  }
  const shot = await tab.screenshot();
  if (shot) writeFileSync(`${out}/page.png`, shot);
  writeFileSync(`${out}/report.json`, JSON.stringify(report, null, 2));
  console.log(JSON.stringify(report, null, 2));
} catch (error) {
  failed(String(error?.stack ?? error));
} finally {
  await browser.close().catch(error => console.error('close:', error.message));
}
process.exit(0);
