// A person in their own headless Chromium on the local tilo.sh, as a
// visitor arrives there: a fresh profile opens the site and lands in the
// Lounge as a new person. Then, as asked:
//
//   --follow LINK   they follow an invitation link and press Join on its card;
//   --key NAME      they make an API key: `tilo keys create NAME` runs as a
//                   backend's terminal runs it, and the person approves the
//                   link it prints on the site's own /sign-in page, in this
//                   same browser, by pressing Allow;
//   --no-pages      the key they make on the door's grants page does not
//                   operate web pages in Browser: the box is unticked;
//   --root FILE     they are a person another program can hold: the page
//                   makes a recovery key for them (its own "save a recovery
//                   key") and FILE gets what opens them elsewhere, the
//                   envelope and that key, as `tilo-approve person` reads it.
//
// usage: node person.mjs SITE RELAY OUT_DIR [--follow LINK] [--key NAME --tilo BIN --door DOOR [--no-pages]] [--root FILE]
//
// Prints one JSON line: {arrivedMs, joinedMs, room, followed, key, entered}.
// `arrivedMs` is a fresh visitor's entry: from navigating to the site to the
// page done arriving and a member of the Lounge, which the entry-speed work
// measures. Saves the page's console and a screenshot in OUT_DIR, and exits
// non-zero, saying what never happened, when a step did not. With `--key` it
// also keeps what the person reads there, in both appearances: the sign-in
// card they allow, the door's grants page, and the key that page makes.
import {spawn} from 'node:child_process';
import {mkdirSync, writeFileSync} from 'node:fs';
import {join} from 'node:path';
import {launch, sleep} from '../../../../tools/web/chrome.mjs';

const [site, relay, out, ...rest] = process.argv.slice(2);
const option = name => {
  const at = rest.indexOf(name);
  return at < 0 ? null : rest[at + 1];
};
if (!site || !relay || !out) {
  console.error('usage: node person.mjs SITE RELAY OUT_DIR [--follow LINK] [--key NAME --tilo BIN --door DOOR [--no-pages]] [--root FILE]');
  process.exit(2);
}
const follow = option('--follow');
const keyName = option('--key');
const rootFile = option('--root');
const noPages = rest.includes('--no-pages');
const tiloBin = option('--tilo');
const door = option('--door');
mkdirSync(out, {recursive: true});

const report = {site, relay};
// The relay's certificate is the local Caddy's: accepted, as a person who
// trusts it would. A software GPU, and no sandbox where this runs as root.
const browser = await launch({args: [
  '--ignore-certificate-errors',
  '--enable-features=Vulkan', '--use-vulkan=swiftshader', '--use-angle=swiftshader',
  '--disable-dev-shm-usage',
  ...(process.getuid?.() === 0 ? ['--no-sandbox'] : []),
]});
const tab = await browser.tab();
await tab.send('Emulation.setDeviceMetricsOverride', {width: 1280, height: 800, deviceScaleFactor: 1, mobile: false})
  .catch(() => {});

const text = () => tab.evaluate(`(document.body?.innerText || '').replace(/\\s+/g, ' ').trim().slice(0, 600)`)
  .catch(() => '');
const finish = async (code, message) => {
  if (message) report.error = message;
  writeFileSync(join(out, 'console.log'), tab.log.join('\n'));
  const shot = await tab.screenshot().catch(() => null);
  if (shot) writeFileSync(join(out, 'page.png'), shot);
  await browser.close().catch(() => {});
  if (message) {
    const said = tab.log.slice(-30);
    if (said.length) console.error(`--- the page said:\n${said.join('\n')}`);
    console.error(`FAIL: ${message}`);
  }
  console.log(JSON.stringify(report));
  process.exit(code);
};
const until = async (expression, what, timeout = 180_000) => {
  const deadline = Date.now() + timeout;
  for (;;) {
    const value = await tab.evaluate(expression).catch(() => false);
    if (value) return value;
    if (Date.now() > deadline) await finish(1, `${what} never happened: ${await text()}`);
    await sleep(100);
  }
};
// The page as a person would look at it, kept in OUT_DIR as `name-light.png`
// and `name-dark.png`: what each surface the check visits says, in the two
// appearances it has.
const capture = async name => {
  for (const theme of ['light', 'dark']) {
    await tab.send('Emulation.setEmulatedMedia', {
      media: 'screen',
      features: [{name: 'prefers-color-scheme', value: theme}],
    }).catch(() => {});
    await sleep(150);
    const shot = await tab.screenshot().catch(() => null);
    if (shot) writeFileSync(join(out, `${name}-${theme}.png`), shot);
  }
  await tab.send('Emulation.setEmulatedMedia', {features: []}).catch(() => {});
};
const memberships = `JSON.parse(globalThis.localStorage.getItem('tilo-host-memberships') ?? '{}')`;
const entered = `JSON.parse(globalThis.localStorage.getItem('tilo-entered-workspaces') ?? '[]')`;

// A fresh visitor's entry.
const began = Date.now();
await tab.navigate(site);
await until(`['done', 'stopped'].includes(document.documentElement.dataset.arrival)`, 'the page arriving');
if (await tab.evaluate('document.documentElement.dataset.arrival') !== 'done') {
  await finish(1, `the page stopped arriving: ${await text()}`);
}
report.arrivedMs = Date.now() - began;
const membership = JSON.parse(await until(`(() => {
  const kept = ${memberships}[${JSON.stringify(new URL(relay).origin)}];
  return kept?.room ? JSON.stringify(kept) : '';
})()`, 'joining the Lounge'));
report.joinedMs = Date.now() - began;
report.room = membership.room;
console.error(`ok: a fresh visitor arrived in ${report.arrivedMs} ms and was in the Lounge (${report.room}) at ${report.joinedMs} ms`);

if (rootFile) {
  // The person this page is, opened elsewhere: the page's own identity module
  // makes a recovery key for them and keeps the envelope that goes with it.
  const held = await tab.evaluate(`(async () => {
    // The page's modules live under the build's own version directory
    // (/v/<hash>/), which its preload hints name: the identity module is one
    // of them, and importing the same address gives the module the page
    // itself holds its person in.
    const hint = document.querySelector('link[rel="modulepreload"][href], script[type="module"][src]');
    const base = new URL('.', hint.href || hint.src);
    const {browserIdentity} = await import(new URL('identity.mjs', base).href);
    const person = await browserIdentity();
    const recoveryKey = await person.createRecoveryKey();
    return JSON.stringify({person: person.userPublicKey, recovery_key: recoveryKey, envelope: person.envelope});
  })()`);
  const kept = JSON.parse(held);
  if (!kept.recovery_key || !kept.envelope) await finish(1, `the page made no way to hold this person: ${held}`);
  // The person held must be the person the page joined the Lounge as.
  if (membership.userPublicKey && membership.userPublicKey !== kept.person) {
    await finish(1, `the page holds ${kept.person}, but joined as ${membership.userPublicKey}`);
  }
  writeFileSync(rootFile, JSON.stringify(kept, null, 2), {mode: 0o600});
  report.person = kept.person;
  report.root = rootFile;
  console.error(`ok: the person ${kept.person} can be held from ${rootFile}`);
}

if (follow) {
  const before = JSON.stringify(await tab.evaluate(`JSON.stringify(${entered})`));
  await tab.navigate(follow);
  // The invitation's card names the workspace and the role; they press Join.
  await until(`(() => {
    const join = [...document.querySelectorAll('button')].find(button => button.textContent.trim() === 'Join');
    if (!join || join.disabled) return false;
    join.click();
    return true;
  })()`, 'the invitation offering to join');
  // Joined: the page entered a workspace it had not, other than the Lounge.
  report.followed = await until(`(() => {
    const now = ${entered};
    const found = (Array.isArray(now) ? now : Object.values(now)).find(entry =>
      (entry?.room ?? entry?.scope) && (entry.room ?? entry.scope) !== ${JSON.stringify(report.room)});
    return found ? JSON.stringify(found) : '';
  })()`, `joining the workspace the invitation names (entered before: ${before})`).then(JSON.parse);
  console.error(`ok: the invitation landed them in ${JSON.stringify(report.followed)}`);
}

if (keyName) {
  if (!tiloBin || !door) await finish(2, '--key needs --tilo and --door');
  // A backend's terminal: its own home, the local door.
  const home = join(out, 'terminal');
  mkdirSync(home, {recursive: true});
  const terminal = spawn(tiloBin, ['keys', 'create', keyName], {
    env: {...process.env, HOME: home, XDG_STATE_HOME: join(home, 'state'), XDG_CONFIG_HOME: join(home, 'config'),
      TILO_DOOR: door},
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  let said = '';
  let printed = '';
  terminal.stderr.on('data', chunk => { said += chunk; });
  terminal.stdout.on('data', chunk => { printed += chunk; });
  const ended = new Promise(resolve => terminal.on('exit', resolve));
  const deadline = Date.now() + 30_000;
  let link = null;
  while (!link) {
    link = said.match(/^\s*(https?:\/\/\S+)\s*$/m)?.[1] ?? null;
    if (!link && Date.now() > deadline) await finish(1, `tilo keys create printed no link: ${said}`);
    if (!link) await sleep(100);
  }
  report.approveLink = link;
  await tab.navigate(link);
  await until(`['asked', 'stopped'].includes(document.documentElement.dataset.signIn)`, 'the sign-in page asking');
  if (await tab.evaluate('document.documentElement.dataset.signIn') !== 'asked') {
    await finish(1, `the sign-in page stopped: ${await text()}`);
  }
  report.asked = await text();
  // The card the person reads before they allow: what the key reaches, and
  // what it may do in Browser.
  await capture('sign-in');
  await tab.evaluate(`document.getElementById('sign-in-allow').click()`);
  const code = await Promise.race([ended, sleep(120_000).then(() => 'timeout')]);
  if (code !== 0) {
    terminal.kill();
    await finish(1, `tilo keys create did not get the key (${code}): ${said} | the page: ${await text()}`);
  }
  report.key = printed.match(/TILO_API_KEY=(\S+)/)?.[1] ?? null;
  if (!report.key) await finish(1, `tilo keys create printed no key: ${printed}`);
  console.error(`ok: they approved "${keyName}" on the site's sign-in page and the terminal got its key`);

  // The grants page the same browser is now signed in to: the keys, each
  // saying whether it operates web pages in Browser, and the way to revoke
  // one; and the page that shows the key it makes once.
  await tab.navigate(`${door}/grants`);
  await until(`document.querySelector('ul.grants, form.new') !== null`, 'the grants page');
  await capture('grants');
  await tab.evaluate(`(() => {
    document.querySelector('form.new input[name="name"]').value = 'The grants page key';
    // What the person gives this key: a key they do not let operate web
    // pages asks them about each page action that acts on one.
    const box = document.querySelector('form.new input[name="browser_pages"]');
    if (box) box.checked = ${noPages ? 'false' : 'true'};
    document.querySelector('form.new').requestSubmit();
    return true;
  })()`);
  // The door asks the person to allow the key it would make (the same card
  // the site showed for the terminal's key); allowing it answers with the
  // key, which that page shows once.
  const making = await until(`document.querySelector('code.key') ? 'made'
    : (['asked', 'stopped'].includes(document.documentElement.dataset.signIn) ? 'asked' : '')`,
  'the card asking for the key or the key itself');
  if (making === 'asked') {
    await capture('key-ask');
    await tab.evaluate(`document.getElementById('sign-in-allow').click()`);
  }
  await until(`document.querySelector('code.key') !== null`, 'the key the grants page made');
  report.madeOnPage = await text();
  report.madeKey = await tab.evaluate(`document.querySelector('code.key').textContent.trim()`);
  if (noPages) {
    report.noPagesKey = report.madeKey;
    report.noPagesSaid = report.madeOnPage;
  }
  await capture('key');
}
await finish(0);
