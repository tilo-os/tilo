// The story of two people working together for the first time: Ada makes a
// new workspace and invites Grace, Grace opens the invitation in a browser
// that has never seen Tilo and joins, each sees the other in their people list,
// Grace follows Ada, and what Ada types in a note shows on Grace's screen.
//
// Both are people a stranger would be: fresh browser profiles on the stack's
// own site, with no key, membership or setup of their own beforehand. What
// they see is read from what the desktop publishes for a screen reader (its
// names and values) and from its People list, never from pixels; the pixels
// are recorded for the judge.
//
// usage: node invite-live.mjs SITE_URL OUT_DIR [SERVICE_LOG…]
// Writes OUT_DIR/story.json (stories/recorder.mjs) and exits non-zero when a
// step failed or ran over its budget.
import {sleep} from '../../../../tools/web/chrome.mjs';
import {stepLimit, SHELL} from '../integration/waits.mjs';
import {click, found, key, NODE_VALUES, openMarks, pageText, pressNamed, sealedLink, searcher, TREE_TEXT, until}
  from '../integration/shell.mjs';
import {recorder, StoryStopped} from './recorder.mjs';

const [site, out, ...logs] = process.argv.slice(2);
if (!site || !out) {
  console.error('usage: node invite-live.mjs SITE_URL OUT_DIR [SERVICE_LOG…]');
  process.exit(2);
}

const fail = message => { throw new Error(message); };
const {search} = searcher({until, failed: fail, dump: async () => {}});
const press = (tab, name) => pressNamed(tab, name);
// Wait until the desktop publishes a node named `name`, then press it.
const pressWhen = async (tab, name, what, timeout = stepLimit(3)) => {
  await tab.evaluate('globalThis.tiloMount?.requestSemantics?.()').catch(() => {});
  await until(tab, found(name), what, timeout);
  if (!await press(tab, name)) fail(`${what} is not where a person can press it`);
};
// The text the desktop shows, as a person would read it.
const shows = (tab, text, what, timeout) => tab.evaluate('globalThis.tiloMount?.requestSemantics?.()').catch(() => {})
  .then(() => until(tab, `(${TREE_TEXT}).includes(${JSON.stringify(text)})`, what, timeout));
// The rows of a page's People list: everyone here but the person themself.
const peopleRows = tab => tab.evaluate('JSON.stringify(globalThis.tiloMount?.people?.() ?? [])').then(JSON.parse).catch(() => []);
const arrived = `['done', 'stopped'].includes(document.documentElement.dataset.arrival)`;
// The workspace the page has open, by its id, once it is open under `name`.
const openWorkspace = name => `(() => {
  const chosen = JSON.parse(globalThis.localStorage.getItem('tilo-chosen-workspace') ?? 'null');
  const entered = JSON.parse(globalThis.localStorage.getItem('tilo-entered-workspaces') ?? '[]');
  const kept = (Array.isArray(entered) ? entered : []).find(entry => entry.workspace === chosen?.workspace);
  const open = document.documentElement.dataset.entry === 'open' && ${arrived};
  return open && kept?.name === ${JSON.stringify(name)} ? chosen.workspace : '';
})()`;
// Ctrl+K, the search chord (Meta on a Mac).
const chord = async tab => (await tab.evaluate('/Mac/.test(navigator.platform)')) ? 4 : 2;

const story = recorder({id: 'invite-live', out, logs});
const WORKSPACE = 'Garden club';
let error = null;
try {
  const ada = await story.person('Ada');
  const adaKeys = await chord(ada.tab);

  await story.step('Ada opens Tilo', async () => {
    const began = Date.now();
    await story.web.coldLoad(ada, site);
    await until(ada.tab, arrived, 'Ada’s page arriving', stepLimit(6));
    const arrival = await ada.tab.evaluate('document.documentElement.dataset.arrival');
    if (arrival !== 'done') fail(`Ada’s page stopped: ${(await pageText(ada.tab)).slice(0, 300)}`);
    story.timing('Ada’s page load, to the desktop', Date.now() - began);
    story.web.scriptSize(ada);
  });

  await story.step('Ada opens Tilo again', async () => {
    const began = Date.now();
    await story.web.warmLoad(ada);
    await until(ada.tab, arrived, 'Ada’s page arriving again', stepLimit(6));
    story.timing('Ada’s second page load, to the desktop', Date.now() - began);
  });

  await story.step('Ada gives their name', async () => {
    // The first thing Ada does on the desktop: the search chord, timed to
    // the search opening as the page's answer.
    await click(ada.tab, SHELL.empty.x, SHELL.empty.y);
    const pressed = Date.now();
    await key(ada.tab, 'k', adaKeys);
    await until(ada.tab, 'globalThis.tiloMount?.state()?.searchOpen === true', 'the search opening');
    story.timing('Ada’s first interaction (the search opening)', Date.now() - pressed);
    await search(ada.tab, adaKeys, 'set your name', 'Set your name');
    await until(ada.tab, `!!document.querySelector('input[name=display-name]')`, 'the name form');
    await ada.tab.evaluate(`(() => {
      const input = document.querySelector('input[name=display-name]');
      input.value = 'Ada';
      input.dispatchEvent(new Event('input', {bubbles: true}));
      input.closest('form').querySelector('button[type=submit]').click();
    })()`);
    await until(ada.tab, `!document.querySelector('input[name=display-name]')`, 'the name kept', stepLimit(4));
  });

  await story.step(`Ada makes a new workspace, “${WORKSPACE}”`, async () => {
    await search(ada.tab, adaKeys, 'new workspace', 'New workspace');
    await until(ada.tab, `document.querySelector('.tilo-entry-card input') !== null`, 'the card asking what to name it');
    await ada.tab.evaluate(`(() => {
      const input = document.querySelector('.tilo-entry-card input');
      input.value = ${JSON.stringify(WORKSPACE)};
      input.dispatchEvent(new Event('input', {bubbles: true}));
      const button = [...document.querySelectorAll('.tilo-entry-card button')]
        .find(candidate => candidate.textContent.trim() === 'Create' && !candidate.disabled);
      button?.click();
      return !!button;
    })()`) || fail('the card offers no Create');
    ada.workspace = await until(ada.tab, openWorkspace(WORKSPACE), `${WORKSPACE} opening`, stepLimit(10));
  });

  let link = '';
  await story.step('Ada makes an invitation link', async () => {
    await pressWhen(ada.tab, 'Invite', 'the Invite button');
    await pressWhen(ada.tab, 'Create link', 'the invite sheet’s Create link');
    await ada.tab.evaluate('globalThis.tiloMount?.requestSemantics?.()').catch(() => {});
    link = await until(ada.tab, `(() => {
      const values = JSON.parse(${NODE_VALUES});
      return values.find(value => new RegExp(${JSON.stringify(sealedLink.source)}).test(value)) ?? '';
    })()`, 'the invitation link');
  });

  const grace = await story.person('Grace');
  await story.step('Grace opens the invitation in a fresh browser', async () => {
    const began = Date.now();
    await grace.tab.navigate(link);
    await until(grace.tab, `(document.body.innerText || '').includes('invited you to')`, 'the invitation card', stepLimit(4));
    story.timing('Grace’s invitation load', Date.now() - began);
    const card = await pageText(grace.tab);
    if (!card.includes('Ada') || !card.includes(WORKSPACE)) fail(`the invitation does not say Ada invited Grace to ${WORKSPACE}: ${card.slice(0, 300)}`);
  });

  await story.step(`Grace joins ${WORKSPACE}`, async () => {
    const began = Date.now();
    const pressed = await grace.tab.evaluate(`(() => {
      const card = [...document.querySelectorAll('form.tilo-entry-card')].at(-1);
      const button = card && [...card.querySelectorAll('button')].find(button => button.textContent.trim() === 'Join');
      if (button) button.click();
      return !!button;
    })()`);
    if (!pressed) fail('the invitation offers no Join');
    await until(grace.tab, arrived, 'Grace’s page arriving', stepLimit(6));
    if (await grace.tab.evaluate('document.documentElement.dataset.arrival') !== 'done') {
      fail(`Grace’s page stopped: ${(await pageText(grace.tab)).slice(0, 300)}`);
    }
    // In once the page has the workspace drawn under its name, as its top bar
    // shows it (an invited browser keeps no entered list of its own to read;
    // that the workspace is Ada's is the next step's, where each sees the other).
    await until(grace.tab, `(() => {
      const state = globalThis.tiloMount?.state?.() ?? null;
      return state?.phase === 'presented' && state.workspaceName === ${JSON.stringify(WORKSPACE)};
    })()`, `${WORKSPACE} opening for Grace`, stepLimit(10));
    story.web.signIn(grace, Date.now() - began);
  });

  // Live: each sees the other the moment both are here.
  await story.step('Ada and Grace see each other in their people lists', async () => {
    const someone = '(globalThis.tiloMount?.people?.() ?? []).length > 0';
    await Promise.all([
      until(ada.tab, someone, 'Grace in Ada’s people list', stepLimit(3)),
      until(grace.tab, someone, 'Ada in Grace’s people list', stepLimit(3)),
    ]);
    const graceSees = await peopleRows(grace.tab);
    if (!graceSees.some(row => /Ada/.test(row.label ?? ''))) fail(`Grace’s people list does not name Ada: ${JSON.stringify(graceSees.map(row => row.label))}`);
  });

  await story.step('Grace follows Ada', async () => {
    // Follow is in the people panel, which the faces in the top bar open.
    await pressWhen(grace.tab, 'Everyone here', 'the faces in Grace’s top bar');
    await pressWhen(grace.tab, 'Follow Ada', 'Follow Ada in Grace’s people panel');
    await shows(grace.tab, 'Following Ada', 'Grace’s page saying they follow Ada', stepLimit(2));
    await shows(ada.tab, 'Following you', 'Ada’s page saying Grace follows them', stepLimit(2));
  });

  await story.step('Ada opens Notes and starts a note', async () => {
    await search(ada.tab, adaKeys, 'notes', 'Notes', 'so.tilo.notes');
    const opened = await openMarks(ada.tab);
    story.web.openApp(ada, opened.ok ? opened.presentMs : NaN, 'Notes');
    await ada.tab.evaluate('globalThis.tiloMount?.requestSemantics?.()').catch(() => {});
    await until(ada.tab, found('New note'), 'Notes’ New note');
    const pressed = Date.now();
    if (!await press(ada.tab, 'New note')) fail('Notes’ New note is not where a person can press it');
    await until(ada.tab, `JSON.stringify(globalThis.tiloMount?.state()?.semantics ?? null).includes('MultilineTextInput')`, 'the new note’s text field');
    story.web.action(ada, Date.now() - pressed, 'a new note');
    // Following Ada, Grace is shown Notes too.
    await until(grace.tab, `(globalThis.tiloMount?.navigation?.getSnapshot?.().entries ?? []).some(entry => entry.label === 'Notes')`,
      'Notes on Grace’s screen', stepLimit(3));
  });

  // The edit's whole way: Ada's keys, Ada's page, the workspace, Grace's page.
  // Grace's page is watched from before the first key, so what Grace waited
  // for is timed from Ada's last key.
  const words = `Seeds for spring ${Date.now().toString(36)}`;
  let seenByGrace = null, lastKey = 0;
  await story.step('Ada types a line in the note', async () => {
    await grace.tab.evaluate('globalThis.tiloMount?.requestSemantics?.()').catch(() => {});
    seenByGrace = grace.tab.waitFor(`(${TREE_TEXT}).includes(${JSON.stringify(words)})`, {timeout: 120_000})
      .then(() => Date.now(), () => null);
    lastKey = await story.web.type(ada, words);
    await shows(ada.tab, words, 'Ada’s words in Ada’s note', stepLimit(2));
  });

  await story.step('Grace sees Ada’s words', async () => {
    const seen = await seenByGrace;
    if (seen === null) fail('Ada’s words never showed on Grace’s screen');
    story.timing('Ada’s words on Grace’s screen, after Ada’s last key', seen - lastKey);
    return {tookMs: seen - lastKey};
  });
} catch (thrown) {
  if (!(thrown instanceof StoryStopped)) error = thrown;
}
// A last look at both screens, so the recording ends on what they saw.
await sleep(1500);
const result = await story.finish({error});
console.log(JSON.stringify({completed: result.completed, withinBudget: result.withinBudget, brokeAt: result.brokeAt, timings: result.timings}, null, 2));
process.exit(result.completed && result.withinBudget ? 0 : 1);
