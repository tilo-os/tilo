// The story of a person who keeps their files in Nextcloud: they sign in with
// it from Settings, choose the folders to bring, the files arrive in their
// workspace, and a file changed in Nextcloud changes in the workspace too.
//
// The Nextcloud is the check's own (crates/system/cloud-source-dav/tests/
// servers.sh, seeded with Ada's Documents and Photos), so what the person
// picks and what changes afterwards are a real server's. Everything Ada does
// is what the page shows; the change in Nextcloud is made on the server, as
// another Nextcloud client would, and looked for in the Files app's window.
//
// usage: node nextcloud-files.mjs SITE_URL OUT_DIR [SERVICE_LOG…]
// Writes OUT_DIR/story.json (stories/recorder.mjs) and exits non-zero when a
// step failed or ran over its budget.
import {mkdirSync, writeFileSync} from 'node:fs';
import {join} from 'node:path';
import {sleep} from '../../../../tools/web/chrome.mjs';
import {stepLimit} from '../integration/waits.mjs';
import {
  found, openMarks, pressWhen, searcher, shows, TREE_TEXT, waitUntil,
} from '../integration/shell.mjs';
import {
  arrive, beginNextcloudSignIn, newPageTarget, nodesNow, offered, openExternalAccounts, pageTargets,
  nextcloudSignIn, plainText,
} from './cloud.mjs';
import {recorder, StoryStopped} from './recorder.mjs';

const [site, out, ...logs] = process.argv.slice(2);
if (!site || !out) {
  console.error('usage: node nextcloud-files.mjs SITE_URL OUT_DIR [SERVICE_LOG…]');
  process.exit(2);
}

const server = (process.env.TILO_TEST_NEXTCLOUD_URL ?? '').replace(/\/$/, '');
const user = process.env.TILO_TEST_NEXTCLOUD_USER ?? '';
const password = process.env.TILO_TEST_NEXTCLOUD_PASSWORD ?? '';
const files = (process.env.TILO_TEST_NEXTCLOUD_FILES ?? '').replace(/\/$/, '');

const WORKSPACE = 'Garden';
// A file the seed put in Ada's Nextcloud, and what a change to it says.
const HELD = 'plan.txt';
const CHANGED = `Spring planting, changed ${Date.now().toString(36)}`;

// What a wait that runs out says: what it waited for, and where the page was.
const until = async (tab, expression, what, timeout = stepLimit(3)) => {
  const value = await tab.waitFor(expression, {timeout}).catch(() => false);
  if (value) return value;
  throw new Error(`${what} did not happen within ${(timeout / 1000).toFixed(1)} s; the page offers: ${JSON.stringify(await offered(tab))}`);
};
const fail = message => { throw new Error(message); };
const {search} = searcher({until, failed: fail, dump: async () => {}});
const chord = async tab => (await tab.evaluate('/Mac/.test(navigator.platform)')) ? 4 : 2;
const shotOf = async (tab, name) => {
  mkdirSync(join(out, 'shots'), {recursive: true});
  const shot = await tab.screenshot().catch(() => null);
  if (!shot) return null;
  writeFileSync(join(out, 'shots', `${name}.png`), shot);
  return `shots/${name}.png`;
};
// Every name and value the page publishes now, for a failure to say what the
// product offers instead of the control it lacks.
const namesNow = async tab => {
  await tab.evaluate('globalThis.tiloMount?.requestSemantics?.()').catch(() => {});
  await sleep(400);
  return (await tab.evaluate(TREE_TEXT).catch(() => '')).split('\n').map(line => line.trim()).filter(Boolean);
};
// A file changed in Nextcloud: a WebDAV write of the same path, as another
// Nextcloud client would make.
const changeInNextcloud = async () => {
  const answer = await fetch(`${files}/Documents/${HELD}`, {method: 'PUT',
    headers: {authorization: 'Basic ' + Buffer.from(`${user}:${password}`).toString('base64'),
      'content-type': 'text/plain'}, body: `${CHANGED}\n`});
  if (!answer.ok) throw new Error(`changing ${HELD} in Nextcloud answered ${answer.status}`);
};

const story = recorder({id: 'nextcloud-files', out, logs});
let error = null;
try {
  if (!(server && files && user && password)) {
    throw new Error('this story meets a real Nextcloud, and no seeded one is named: TILO_TEST_NEXTCLOUD_URL, _USER, _PASSWORD and _FILES (servers.sh start and seed)');
  }

  const ada = await story.person('Ada');
  const adaKeys = await chord(ada.tab);

  await story.step('Ada chooses Nextcloud and signs in', async () => {
    await arrive({story, person: ada, site, name: 'Ada', workspace: WORKSPACE, command: adaKeys, search});
    await openExternalAccounts(ada.tab, adaKeys, search);
    const before = await pageTargets(ada.launched);
    try {
      await beginNextcloudSignIn(ada.tab, server);
    } catch (thrown) {
      // What the page really publishes, kept with the recording: a control a
      // person can see and the driver cannot name is the driver's problem.
      writeFileSync(join(out, 'evidence', 'settings-nodes.json'),
        JSON.stringify(await nodesNow(ada.tab), null, 2));
      throw thrown;
    }
    await pressWhen(ada.tab, 'Sign in with Nextcloud', 'the sign-in', stepLimit(4));
    // The product opens the server's own page for the person to sign in on.
    const target = await newPageTarget(ada.launched, before, 30_000);
    if (!target) {
      fail(`pressing “Sign in with Nextcloud” opened no page on ${server}; Settings offers: ${JSON.stringify(await offered(ada.tab))}`);
    }
    // Where the press sent the person, said in the log (kept longer than the
    // evidence), and the page driven within a deadline of its own.
    console.log(`the sign-in opened ${target.url || '(a page with no address yet)'}`);
    const nextcloud = await Promise.race([
      ada.launched.tab({target: target.targetId, firstScreen: true}),
      sleep(30_000).then(() => fail(`the page the sign-in opened (${target.url}) could not be driven within 30 s`)),
    ]);
    try {
      await nextcloudSignIn(nextcloud, {user, password});
      await sleep(1000);
    } finally {
      const shown = `${await nextcloud.evaluate('location.href').catch(() => '?')}\n${await plainText(nextcloud)}`;
      console.log(`the server's page: ${shown.slice(0, 1500)}`);
      writeFileSync(join(out, 'evidence', 'nextcloud-page.txt'), `${shown}\n`);
    }
    // Nextcloud says the window can be closed, and Ada goes back to her
    // Tilo tab: a tab behind another draws no frames, so it would show
    // nothing new until she looks.
    await ada.tab.send('Page.bringToFront').catch(() => {});
    const host = new URL(server).hostname;
    await waitUntil(ada.tab, found(host), `the Nextcloud account listed as ${host}`, 60_000);
    await shotOf(ada.tab, '01-connected');
  });

  await story.step('Ada picks the folders to bring', async () => {
    // What the product offers for an account's files: a Files switch on the
    // account, a way to choose files to import, or the account's folders in
    // the Files app. Press the first the page offers.
    const account = await namesNow(ada.tab);
    const choose = account.find(name => /^(Files|Choose files to import…?|Import once…?)$/i.test(name));
    if (choose) {
      await pressWhen(ada.tab, choose, `“${choose}” on the account`);
      return;
    }
    // Files' window: the workspace's own tree, and whatever the account put
    // beside it.
    await search(ada.tab, adaKeys, 'files', 'Files', 'so.tilo.file-explorer');
    const opened = await openMarks(ada.tab);
    story.web.openApp(ada, opened.ok ? opened.presentMs : NaN, 'Files');
    await waitUntil(ada.tab, `(globalThis.tiloMount?.navigation?.getSnapshot?.().entries ?? []).some(entry => entry.label === 'Files')`,
      'Files opening', stepLimit(6));
    const inFiles = await namesNow(ada.tab);
    const folders = inFiles.find(name => /^(Documents|Photos|Nextcloud|Import…?|Choose files)/i.test(name));
    if (folders) {
      await pressWhen(ada.tab, folders, `“${folders}” in Files`);
      return;
    }
    fail(`nothing lets Ada bring folders from Nextcloud: the account offers ${JSON.stringify(account.slice(0, 40))}, and Files offers ${JSON.stringify(inFiles.slice(0, 40))}`);
  });

  await story.step('The files arrive in the workspace', async () => {
    await waitUntil(ada.tab, `(${TREE_TEXT}).includes(${JSON.stringify(HELD)})`, `“${HELD}” in the workspace`, stepLimit(20));
    await shows(ada.tab, HELD, `“${HELD}” in the workspace`);
    await shotOf(ada.tab, '02-files-arrived');
  });

  await story.step('A file changed in Nextcloud changes in the workspace too', async () => {
    await changeInNextcloud();
    await waitUntil(ada.tab, `(${TREE_TEXT}).includes(${JSON.stringify(CHANGED)})`,
      `Nextcloud’s change to “${HELD}” in the workspace`, stepLimit(60));
    await shotOf(ada.tab, '03-change-arrived');
  });
} catch (thrown) {
  if (!(thrown instanceof StoryStopped)) error = thrown;
}
// A last look at the screen, so the recording ends on what Ada saw.
await sleep(1500);
const result = await story.finish({error});
console.log(JSON.stringify({completed: result.completed, withinBudget: result.withinBudget, brokeAt: result.brokeAt, timings: result.timings}, null, 2));
process.exit(result.completed && result.withinBudget ? 0 : 1);
