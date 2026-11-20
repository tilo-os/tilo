// The story of a person who connects their calendar, makes an event and
// invites someone, and of the someone who should receive the invitation.
//
// The calendar is a real Nextcloud's CalDAV, the one the check's server
// starts (crates/system/cloud-source-dav/tests/servers.sh), and Grace is a
// second person on it, with a calendar of her own. Ada does what the page
// shows: Settings from her own card, the Nextcloud account, the Calendar's
// editor and its guest field. Grace's receipt is looked for where an
// invitation could arrive: her calendar on that server.
//
// usage: node calendar-invite.mjs SITE_URL OUT_DIR [SERVICE_LOG…]
// Writes OUT_DIR/story.json (stories/recorder.mjs) and exits non-zero when a
// step failed or ran over its budget.
import {mkdirSync, writeFileSync} from 'node:fs';
import {join} from 'node:path';
import {sleep} from '../../../../tools/web/chrome.mjs';
import {stepLimit} from '../integration/waits.mjs';
import {
  found, key, openMarks, pressWhen, searcher, shows, TREE_TEXT, waitUntil, type,
} from '../integration/shell.mjs';
import {
  arrive, beginNextcloudSignIn, calendarEntries, newPageTarget, nodesNow, offered, openExternalAccounts,
  nextcloudSignIn, pageTargets, plainText,
} from './cloud.mjs';
import {recorder, StoryStopped} from './recorder.mjs';

const [site, out, ...logs] = process.argv.slice(2);
if (!site || !out) {
  console.error('usage: node calendar-invite.mjs SITE_URL OUT_DIR [SERVICE_LOG…]');
  process.exit(2);
}

const server = (process.env.TILO_TEST_NEXTCLOUD_URL ?? '').replace(/\/$/, '');
const user = process.env.TILO_TEST_NEXTCLOUD_USER ?? '';
const password = process.env.TILO_TEST_NEXTCLOUD_PASSWORD ?? '';
const caldav = (process.env.TILO_TEST_NEXTCLOUD_CALDAV ?? '').replace(/\/$/, '');
const grace = process.env.TILO_TEST_NEXTCLOUD_GRACE_EMAIL ?? 'grace@example.test';
const graceUser = process.env.TILO_TEST_NEXTCLOUD_GRACE_USER ?? 'grace';

const WORKSPACE = 'Garden';
const EVENT = `Planning ${Date.now().toString(36)}`;

// What a wait that runs out says: what it waited for, and where the page was.
const until = async (tab, expression, what, timeout = stepLimit(3)) => {
  const value = await tab.waitFor(expression, {timeout}).catch(() => false);
  if (value) return value;
  throw new Error(`${what} did not happen within ${(timeout / 1000).toFixed(1)} s; the page offers: ${JSON.stringify(await offered(tab))}`);
};
const fail = message => { throw new Error(message); };
const {search} = searcher({until, failed: fail, dump: async () => {}});
// Ctrl+K, the search chord (Meta on a Mac).
const chord = async tab => (await tab.evaluate('/Mac/.test(navigator.platform)')) ? 4 : 2;
// A picture of the page the browser opened, where the recording cannot show
// it: a person's video is of their Tilo page, not of another site's tab.
const shotOf = async (tab, name) => {
  mkdirSync(join(out, 'shots'), {recursive: true});
  const shot = await tab.screenshot().catch(() => null);
  if (!shot) return null;
  writeFileSync(join(out, 'shots', `${name}.png`), shot);
  return `shots/${name}.png`;
};

const story = recorder({id: 'calendar-invite', out, logs});
let error = null;
try {
  if (!(server && caldav && user && password)) {
    throw new Error('this story meets a real Nextcloud, and no seeded one is named: TILO_TEST_NEXTCLOUD_URL, _USER, _PASSWORD and _CALDAV (servers.sh start and seed)');
  }

  const ada = await story.person('Ada');
  const adaKeys = await chord(ada.tab);

  await story.step('Ada connects their calendar', async () => {
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
      await nextcloudSignIn(nextcloud, {user, password, beforeGrant: () => shotOf(nextcloud, '01-grant-access')});
      await sleep(1000);
    } finally {
      const shown = `${await nextcloud.evaluate('location.href').catch(() => '?')}\n${await plainText(nextcloud)}`;
      console.log(`the server's page: ${shown.slice(0, 1500)}`);
      writeFileSync(join(out, 'evidence', 'nextcloud-page.txt'), `${shown}\n`);
    }
    // The account arrives in Settings through the server's live reading of
    // it: a row named after the server, never the field the address was typed in.
    const host = new URL(server).hostname;
    await waitUntil(ada.tab, found(host), `the Nextcloud account listed as ${host}`, 60_000);
    await shotOf(ada.tab, '02-connected');
  });

  await story.step('Ada creates an event', async () => {
    await search(ada.tab, adaKeys, 'calendar', 'Calendar', 'so.tilo.calendar');
    const opened = await openMarks(ada.tab);
    story.web.openApp(ada, opened.ok ? opened.presentMs : NaN, 'Calendar');
    await pressWhen(ada.tab, 'New event', 'Calendar’s New event', stepLimit(6));
    await pressWhen(ada.tab, 'Edit event title', 'the event’s title');
    await type(ada.tab, EVENT);
    await shows(ada.tab, EVENT, 'the event named', stepLimit(4));
  });

  await story.step('Ada invites Grace to it', async () => {
    // The event's window is open on the event it just made; the guest field
    // is on the event's sheet.
    const opened = await ada.tab.evaluate(`(${TREE_TEXT}).includes('Invite attendee by email')`).catch(() => false);
    if (!opened) {
      await pressWhen(ada.tab, EVENT, 'the new event in Calendar', stepLimit(6));
    }
    await pressWhen(ada.tab, 'Invite attendee by email', 'the event’s guest field', stepLimit(6));
    await type(ada.tab, grace);
    await key(ada.tab, 'Enter');
    await shows(ada.tab, grace, 'Grace among the event’s guests', stepLimit(6));
    await shotOf(ada.tab, '03-guest-added');
  });

  await story.step('Grace receives the invitation', async () => {
    // Where an invitation could reach Grace: her own calendar on the server,
    // put there by the server's scheduling or by the app's mail. Look for it
    // the way a person's own client would see it arrive.
    const deadline = Date.now() + 30_000;
    let held = [];
    for (;;) {
      held = await calendarEntries({base: caldav, who: graceUser, password});
      if (held.some(text => text.includes(EVENT)) || Date.now() >= deadline) break;
      await sleep(2000);
    }
    writeFileSync(join(out, 'evidence', 'grace-calendar.txt'),
      held.length ? held.join('\n---\n') : '(Grace’s calendar on the server holds no event)\n');
    const received = held.find(text => text.includes(EVENT));
    if (!received) {
      fail(`Grace received nothing: her calendar on the server holds no event named “${EVENT}” 30 s after Ada added her as a guest, and nothing was sent to ${grace}`);
    }
    if (!received.includes(grace)) fail(`Grace’s calendar holds the event, but not as an invitation to ${grace}: ${received.slice(0, 600)}`);
  });
} catch (thrown) {
  if (!(thrown instanceof StoryStopped)) error = thrown;
}
// A last look at the screen, so the recording ends on what Ada saw.
await sleep(1500);
const result = await story.finish({error});
console.log(JSON.stringify({completed: result.completed, withinBudget: result.withinBudget, brokeAt: result.brokeAt, timings: result.timings}, null, 2));
process.exit(result.completed && result.withinBudget ? 0 : 1);
