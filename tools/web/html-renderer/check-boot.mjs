// The page mounts in a browser the parity runs only compare in: Firefox and
// Safari's own WebKit (Playwright's builds), which a person arrives in as much
// as in Chrome. A bundle that starts in Chromium can still fail to start
// here — a file store the browser gives a normal window, storage the session
// keeps or does not, an API the engine spells differently — and the only way
// to know is to load the page in it.
//
// Two boots per engine, both of the bundle's own page (`app.html`), which
// mounts one app of the bundle with the runtime directly:
//
//   real      a throwaway profile, as a person's window is: the desktop
//             mounts, draws a frame, and throws nothing.
//   private   an incognito context, as a private window is. WebKit gives one
//             no file store at all; Chromium and Firefox keep files there.
//             Either way the desktop mounts, draws, and reports what it has:
//             `files: 'unavailable'` exactly when the browser answered this
//             page no file store, so the page can say what its apps cannot do
//             here instead of failing to start.
//
// usage: node check-boot.mjs BUNDLE OUT_DIR [--browser webkit|firefox|chromium] [--app ID]
//
// Prints a JSON report, keeps each boot's console and a screenshot in OUT_DIR,
// and exits non-zero when a boot did not mount, drew nothing, threw, or said
// something other than what this window's file store allows.
import {mkdirSync, writeFileSync} from 'node:fs';
import {join} from 'node:path';
import {fileURLToPath} from 'node:url';
import {serve} from '../chrome.mjs';
import {open} from './engines.mjs';

/** This tool's own directory: its `app.html` is the page booted, served beside
 * the bundle it mounts (`pkg/`, `host.mjs`, the app modules). */
const here = fileURLToPath(new URL('.', import.meta.url));

const args = process.argv.slice(2);
const options = {browser: 'webkit', app: 'so.tilo.notes', out: null, bundle: null};
for (let index = 0; index < args.length; index += 1) {
  const arg = args[index];
  if (arg === '--browser') options.browser = args[++index];
  else if (arg === '--app') options.app = args[++index];
  else if (!options.bundle) options.bundle = arg;
  else if (!options.out) options.out = arg;
  else {
    console.error('usage: node check-boot.mjs BUNDLE OUT_DIR [--browser webkit|firefox|chromium] [--app ID]');
    process.exit(2);
  }
}
if (!options.bundle || !options.out) {
  console.error('usage: node check-boot.mjs BUNDLE OUT_DIR [--browser webkit|firefox|chromium] [--app ID]');
  process.exit(2);
}
mkdirSync(options.out, {recursive: true});

/** Every line the page logged as a failure: an exception, or a console error. */
const failures = tab => tab.log.filter(line => /^(EXC|ERROR) /.test(line));

/** One boot: mount the page in `session` and report what it did. */
async function boot(site, session) {
  const opened = await open({engine: options.browser, session, width: 1280, height: 800});
  const tab = opened.tab;
  const found = {session, mounted: false, presented: false, files: null, failures: []};
  try {
    await tab.navigate(`${site}/app.html?render=dom&app=${encodeURIComponent(options.app)}`);
    // `mountDesktop` resolves into `window.mount`; a mount that threw leaves it
    // unset and its message in the tab's log.
    found.mounted = await tab.waitFor('Boolean(window.mount)', {timeout: 180_000}).catch(() => false);
    found.presented = await tab.waitFor('(window.renderMarks?.presented?.length ?? 0) > 0', {timeout: 120_000})
      .catch(() => false);
    const marks = await tab.evaluate(`({firstPhase: window.renderMarks?.firstPhase ?? null,
      frames: window.renderMarks?.presented?.length ?? 0,
      files: window.renderMarks?.files ?? null,
      filesError: window.renderMarks?.filesError ?? null})`)
      .catch(() => ({}));
    found.firstPhase = marks.firstPhase ?? null;
    found.frames = marks.frames ?? 0;
    found.files = marks.files ?? null;
    found.filesError = marks.filesError ?? null;
    // What this session's browser answers the page when it asks for a file
    // store: the ground the page's own report is held to, rather than a table
    // of which engine does what, which moves between versions.
    found.fileStore = await tab.evaluate(`(async () => { try { await navigator.storage.getDirectory(); return true; }
      catch { return false; } })()`).catch(() => null);
    found.failures = failures(tab);
    found.text = await tab.evaluate(`(document.body.innerText || '').replace(/\\s+/g, ' ').trim().slice(0, 400)`)
      .catch(() => '');
  } finally {
    const shot = await tab.screenshot().catch(() => null);
    if (shot) writeFileSync(join(options.out, `${options.browser}-${session}.png`), shot);
    writeFileSync(join(options.out, `${options.browser}-${session}.log`), tab.log.join('\n'));
    await opened.close().catch(() => {});
  }
  return found;
}

const site = await serve(here, {also: [options.bundle]});
const report = {browser: options.browser, app: options.app, at: new Date().toISOString(), boots: []};
let failed = null;
try {
  for (const session of ['real', 'private']) {
    const found = await boot(site.url, session);
    report.boots.push(found);
    if (!found.mounted) {
      failed ??= `${session}: the desktop did not mount`;
      console.error(`${options.browser} (${session}): the desktop did not mount`);
    } else if (!found.presented) {
      failed ??= `${session}: the desktop mounted but drew no frame`;
      console.error(`${options.browser} (${session}): the desktop mounted but drew no frame`);
    }
    if (found.failures.length) {
      failed ??= `${session}: the page threw`;
      console.error(`${options.browser} (${session}): the page said:\n${found.failures.join('\n')}`);
    }
    // What the page reports has to be what this window actually offers: a
    // window the browser gives no file store must say so, and one it gives a
    // store must say nothing about files. A browser that did not answer at all
    // is not judged on it.
    const expectedFiles = found.fileStore === false ? 'unavailable' : null;
    if (found.fileStore !== null && found.files !== expectedFiles) {
      failed ??= `${session}: files read ${JSON.stringify(found.files)}, expected ${JSON.stringify(expectedFiles)}`;
      console.error(`${options.browser} (${session}): files read ${JSON.stringify(found.files)},`
        + ` expected ${JSON.stringify(expectedFiles)}`);
    }
    console.log(`${options.browser} (${session}): mounted ${found.mounted}, drew ${found.frames} frame(s),`
      + ` file store ${JSON.stringify(found.fileStore)}, files ${JSON.stringify(found.files)}`
      + `${found.filesError ? ` (${found.filesError})` : ''}`);
  }
} finally {
  await site.close();
}
report.ok = failed === null;
report.error = failed;
writeFileSync(join(options.out, 'boot.json'), JSON.stringify(report, null, 2));
if (failed) {
  console.error(`not ok: ${failed}`);
  process.exit(1);
}
console.log(`ok: ${options.browser} mounts the desktop in a normal window and in a private one`);
