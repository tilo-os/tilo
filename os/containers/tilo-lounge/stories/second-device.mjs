// The story of a person's second device: Ada works in Tilo on her laptop's
// browser, pairs her phone's browser to herself with "Pair a new device", and
// from then on both show the same workspace. A Chat message and a Tasks issue
// written on either shows on the other within moments, and the phone still
// shows them after it opened Tilo again.
//
// Both browsers are fresh profiles on the stack's own site: the phone holds no
// key of Ada's before the pairing link makes it one of her devices. What they
// show is read from what the desktop publishes for a screen reader, never from
// pixels; the pixels are recorded for the judge.
//
// usage: node second-device.mjs SITE_URL OUT_DIR [SERVICE_LOG…]
// Writes OUT_DIR/story.json (stories/recorder.mjs) and exits non-zero when a
// step failed or ran over its budget.
import {sleep} from '../../../../tools/web/chrome.mjs';
import {stepLimit} from '../integration/waits.mjs';
import {found, key, openMarks, pageText, pressNamed, searcher, TREE_TEXT, typeText, until}
  from '../integration/shell.mjs';
import {recorder, StoryStopped} from './recorder.mjs';

const [site, out, ...logs] = process.argv.slice(2);
if (!site || !out) {
  console.error('usage: node second-device.mjs SITE_URL OUT_DIR [SERVICE_LOG…]');
  process.exit(2);
}

const fail = message => { throw new Error(message); };
const {search} = searcher({until, failed: fail, dump: async () => {}});
const semantics = tab => tab.evaluate('globalThis.tiloMount?.requestSemantics?.()').catch(() => {});
const pressWhen = async (tab, name, what, timeout = stepLimit(3)) => {
  await semantics(tab);
  await until(tab, found(name), what, timeout);
  if (!await pressNamed(tab, name)) fail(`${what} is not where a person can press it`);
};
const shows = (tab, text, what, timeout = stepLimit(3)) => semantics(tab)
  .then(() => until(tab, `(${TREE_TEXT}).includes(${JSON.stringify(text)})`, what, timeout));
const chord = async tab => (await tab.evaluate('/Mac/.test(navigator.platform)')) ? 4 : 2;
const arrived = `['done', 'stopped'].includes(document.documentElement.dataset.arrival)`;
const workspaceName = tab => tab.evaluate('globalThis.tiloMount?.state?.()?.workspaceName ?? ""').catch(() => '');
const front = app => `(globalThis.tiloMount?.navigation?.getSnapshot?.().entries ?? []).some(entry => entry.app === ${JSON.stringify(app)})`;

/** `app` on the person's screen, opened from search unless it is open already. */
const openApp = async (one, keys, name, app) => {
  await search(one.tab, keys, name.toLowerCase(), name, app);
  await openMarks(one.tab).catch(() => null);
  await until(one.tab, front(app), `${name} on ${one.name}’s screen`, stepLimit(4));
};

/** Watch `tab` for `text` from now on, answering when it showed (or null). */
const watch = async (tab, text) => {
  await semantics(tab);
  return tab.waitFor(`(${TREE_TEXT}).includes(${JSON.stringify(text)})`, {timeout: stepLimit(4)})
    .then(() => Date.now(), () => null);
};

/** A Chat message from `one`, in the conversation open on their screen; answers when it was sent. */
const say = async (one, words) => {
  await pressWhen(one.tab, 'Message composer', `${one.name}’s message composer`);
  await typeText(one.tab, words);
  await shows(one.tab, words, `${one.name}’s words in the composer`);
  const sent = Date.now();
  await pressWhen(one.tab, 'Send message', `${one.name}’s Send message`);
  return sent;
};

/** A Tasks issue from `one`, in Tasks open on their screen; answers when it was made. */
const file = async (one, title) => {
  await pressWhen(one.tab, 'New issue', `Tasks’ New issue on ${one.name}’s screen`);
  await typeText(one.tab, title);
  const made = Date.now();
  await key(one.tab, 'Enter');
  return made;
};

const story = recorder({id: 'second-device', out, logs});
const run = Date.now().toString(36);
const FROM_LAPTOP = `On the laptop ${run}`;
const FROM_PHONE = `On the phone ${run}`;
const AFTER = `After the phone reopened ${run}`;
const LAPTOP_TASK = `Book the ferry ${run}`;
const PHONE_TASK = `Pack the tent ${run}`;
let error = null;

/** `writer` writes with `write`; answers how long `reader` took to show `text`, failing past the wait. */
const across = async (writer, reader, text, write, what) => {
  const seen = watch(reader.tab, text);
  const wrote = await write();
  const at = await seen;
  if (at === null) fail(`${what} never showed on ${reader.name}’s screen`);
  const took = Math.max(0, at - wrote);
  story.measure('change-on-other-device-ms', took, `${what}, on ${reader.name}’s screen after ${writer.name} wrote it`);
  return {tookMs: took};
};

try {
  const laptop = await story.person('Ada’s laptop');
  const laptopKeys = await chord(laptop.tab);

  await story.step('Ada opens Tilo on her laptop', async () => {
    await story.web.coldLoad(laptop, site);
    await until(laptop.tab, arrived, 'the laptop’s page arriving', stepLimit(6));
    if (await laptop.tab.evaluate('document.documentElement.dataset.arrival') !== 'done') {
      fail(`the laptop’s page stopped: ${(await pageText(laptop.tab)).slice(0, 300)}`);
    }
  });

  let link = '';
  await story.step('Ada asks her laptop for a link to pair a new device', async () => {
    await search(laptop.tab, laptopKeys, 'pair a new device', 'Pair a new device');
    link = await until(laptop.tab, `document.querySelector('input[name=pairing-link]')?.value ?? ''`,
      'the pairing link', stepLimit(4));
  });

  const phone = await story.person('Ada’s phone', {share: true});
  const phoneKeys = await chord(phone.tab);
  await story.step('Ada opens the link on her phone and approves it on her laptop', async () => {
    await phone.tab.navigate(link);
    // The phone asks to be Ada's device; if it first asks whose device it
    // becomes, she says hers. The laptop then asks her, naming the code the
    // phone shows, and she approves it. Each is pressed when it shows.
    await Promise.all([
      until(phone.tab, `(() => {
        const go = document.querySelector('[data-card="pair-whose"] .tilo-entry-primary');
        if (go && !go.dataset.pressed) { go.dataset.pressed = '1'; go.click(); }
        return ${arrived};
      })()`, 'the phone’s page arriving as Ada’s device', stepLimit(8)),
      until(laptop.tab, `(() => {
        const approve = [...document.querySelectorAll('[data-card="pair-ask"] button')]
          .find(button => button.textContent.startsWith('Approve'));
        if (approve && !approve.dataset.pressed) { approve.dataset.pressed = '1'; approve.click(); }
        return !!approve;
      })()`, 'the laptop asking Ada to approve her phone', stepLimit(6)),
    ]);
    // Paired, Ada closes the laptop's pairing sheet.
    await laptop.tab.evaluate(`[...document.querySelectorAll('[data-card="pair-link"] button')]
      .find(button => button.textContent.trim() === 'Close')?.click()`).catch(() => {});
    if (await phone.tab.evaluate('document.documentElement.dataset.arrival') !== 'done') {
      fail(`the phone’s page stopped: ${(await pageText(phone.tab)).slice(0, 300)}`);
    }
    const [mine, theirs] = await Promise.all([workspaceName(laptop.tab), workspaceName(phone.tab)]);
    if (!mine || mine !== theirs) fail(`the phone shows “${theirs}”, the laptop “${mine}”`);
  });

  await story.step('Ada opens Chat on both', async () => {
    await Promise.all([openApp(laptop, laptopKeys, 'Chat', 'so.tilo.chat'), openApp(phone, phoneKeys, 'Chat', 'so.tilo.chat')]);
    await Promise.all([laptop, phone].map(one => semantics(one.tab).then(() =>
      until(one.tab, found('Message composer'), `Chat’s composer on ${one.name}’s screen`, stepLimit(4)))));
  });

  await story.step('A message from the laptop shows on the phone', () =>
    across(laptop, phone, FROM_LAPTOP, () => say(laptop, FROM_LAPTOP), 'the laptop’s message'));
  await story.step('A message from the phone shows on the laptop', () =>
    across(phone, laptop, FROM_PHONE, () => say(phone, FROM_PHONE), 'the phone’s message'));

  await story.step('Ada opens Tasks on both', async () => {
    await Promise.all([openApp(laptop, laptopKeys, 'Tasks', 'so.tilo.tasks'), openApp(phone, phoneKeys, 'Tasks', 'so.tilo.tasks')]);
    await Promise.all([laptop, phone].map(one => semantics(one.tab).then(() =>
      until(one.tab, found('New issue'), `Tasks’ New issue on ${one.name}’s screen`, stepLimit(4)))));
  });

  await story.step('An issue from the laptop shows on the phone', () =>
    across(laptop, phone, LAPTOP_TASK, () => file(laptop, LAPTOP_TASK), 'the laptop’s issue'));
  await story.step('An issue from the phone shows on the laptop', () =>
    across(phone, laptop, PHONE_TASK, () => file(phone, PHONE_TASK), 'the phone’s issue'));

  await story.step('The phone opens Tilo again and still shows both issues and messages', async () => {
    await story.web.warmLoad(phone);
    await until(phone.tab, arrived, 'the phone’s page arriving again', stepLimit(6));
    if (!await phone.tab.evaluate(front('so.tilo.tasks')).catch(() => false)) await openApp(phone, phoneKeys, 'Tasks', 'so.tilo.tasks');
    await shows(phone.tab, LAPTOP_TASK, 'the laptop’s issue on the reopened phone', stepLimit(4));
    await shows(phone.tab, PHONE_TASK, 'the phone’s issue on the reopened phone', stepLimit(4));
    await openApp(phone, phoneKeys, 'Chat', 'so.tilo.chat');
    await shows(phone.tab, FROM_LAPTOP, 'the laptop’s message on the reopened phone', stepLimit(4));
    await shows(phone.tab, FROM_PHONE, 'the phone’s message on the reopened phone', stepLimit(4));
  });

  await story.step('A message from the laptop shows on the reopened phone', async () => {
    await openApp(laptop, laptopKeys, 'Chat', 'so.tilo.chat');
    return across(laptop, phone, AFTER, () => say(laptop, AFTER), 'the laptop’s message after the phone reopened');
  });
} catch (thrown) {
  if (!(thrown instanceof StoryStopped)) error = thrown;
}
await sleep(1500);
const result = await story.finish({error});
console.log(JSON.stringify({completed: result.completed, withinBudget: result.withinBudget, brokeAt: result.brokeAt, timings: result.timings}, null, 2));
process.exit(result.completed && result.withinBudget ? 0 : 1);
