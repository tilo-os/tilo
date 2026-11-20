// The story of people who were invited: Ada makes a workspace and invites
// Grace and Lin into it, sees who is here and what each is working on,
// follows Grace to watch her screen live, mentions Lin in Chat, and Lin is
// told of the mention and opens it.
//
// All three are people a stranger would be: fresh browser profiles on the
// stack's own site, with no key, membership or setup of their own beforehand.
// What they see is read from what the desktop publishes for a screen reader
// (its names and values) and from its people panel, never from pixels; the
// pixels are recorded for the judge.
//
// usage: node invited-activity.mjs SITE_URL OUT_DIR [SERVICE_LOG…]
// Writes OUT_DIR/story.json (stories/recorder.mjs) and exits non-zero when a
// step failed or ran over its budget.
import {sleep} from '../../../../tools/web/chrome.mjs';
import {stepLimit, SHELL} from '../integration/waits.mjs';
import {click, found, foundMatching, key, NODE_VALUES, openMarks, pageText, pressMatching, pressNamed, sealedLink, searcher, TREE_TEXT, typeText, until}
  from '../integration/shell.mjs';
import {recorder, StoryStopped} from './recorder.mjs';

const [site, out, ...logs] = process.argv.slice(2);
if (!site || !out) {
  console.error('usage: node invited-activity.mjs SITE_URL OUT_DIR [SERVICE_LOG…]');
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
// The same, for a control the page names by its own words (`pattern`, a
// RegExp source): the bar names controls after the workspace, the strip, the
// person or the count they are about.
const pressNaming = async (tab, pattern, what, timeout = stepLimit(3)) => {
  await tab.evaluate('globalThis.tiloMount?.requestSemantics?.()').catch(() => {});
  await until(tab, foundMatching(pattern), what, timeout);
  if (!await pressMatching(tab, pattern)) fail(`${what} is not where a person can press it`);
};
// The text the desktop shows, as a person would read it.
const shows = (tab, text, what, timeout) => tab.evaluate('globalThis.tiloMount?.requestSemantics?.()').catch(() => {})
  .then(() => until(tab, `(${TREE_TEXT}).includes(${JSON.stringify(text)})`, what, timeout));
// Everyone here, as the bar offers it: its people button, or, where the faces
// fit, the faces themselves, which the one control covers. A face opens that
// person's card in the roster's place, and pressing it again opens everyone
// here, so a person presses until the rows stand where the card was.
const opensHere = async tab => {
  const standing = `(() => {
    const out = [];
    const node = n => { if (n.name) out.push(n.name); (n.children || []).forEach(node); };
    const walk = n => { if (n.semantics) node(n.semantics); (n.children || []).forEach(walk); };
    const root = globalThis.tiloMount?.state()?.semantics?.root;
    if (root) walk(root);
    const names = out.join('\\n');
    if (/^People here now$/m.test(names)) return 'roster';
    return /^.+[’']s profile$/m.test(names) ? 'card' : '';
  })()`;
  for (let attempt = 1; attempt <= 3; attempt += 1) {
    await pressNaming(tab, '^(Everyone here|.*Show who is here)$', 'the faces in the top bar');
    const opened = await tab.waitFor(standing, {timeout: stepLimit(2)}).catch(() => '');
    if (opened === 'roster') return;
    if (opened === 'card') console.log(`the press opened a card where everyone here stands (${attempt}); pressing again`);
  }
  fail('the faces in the top bar opened no roster');
};
// The name form every person meets first: the page's own card, filled in and
// kept.
const givesName = async (tab, command, name) => {
  await search(tab, command, 'set your name', 'Set your name');
  await until(tab, `!!document.querySelector('input[name=display-name]')`, 'the name form');
  await tab.evaluate(`(() => {
    const input = document.querySelector('input[name=display-name]');
    input.value = ${JSON.stringify(name)};
    input.dispatchEvent(new Event('input', {bubbles: true}));
    input.closest('form').querySelector('button[type=submit]').click();
  })()`);
  await until(tab, `!document.querySelector('input[name=display-name]')`, 'the name kept', stepLimit(4));
};
// The invitation card a link opens, as the person it is opened for reads it:
// who invited them, and to what.
const opensInvitation = async (tab, link, who) => {
  await tab.navigate(link);
  await until(tab, `(document.body.innerText || '').includes('invited you to')`, `${who}’s invitation card`, stepLimit(4));
  const card = await pageText(tab);
  if (!card.includes('Ada') || !card.includes('Garden club')) {
    fail(`the invitation does not say Ada invited ${who} to Garden club: ${card.slice(0, 300)}`);
  }
};
// The Join on that card, pressed where the person presses it.
const joins = async (tab, who) => {
  const pressed = await tab.evaluate(`(() => {
    const card = [...document.querySelectorAll('form.tilo-entry-card')].at(-1);
    const button = card && [...card.querySelectorAll('button')].find(button => button.textContent.trim() === 'Join');
    if (button) button.click();
    return !!button;
  })()`);
  if (!pressed) fail(`the invitation offers ${who} no Join`);
};
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

const story = recorder({id: 'invited-activity', out, logs});
const WORKSPACE = 'Garden club';
const stamp = Date.now().toString(36);
const words = `the seed packets are in the shed ${stamp}`;
const mention = `@Lin ${words}`;
let error = null;
// What Ada's people panel said of each person, kept with the story.
let seenHere = [];
try {
  const ada = await story.person('Ada');
  const adaKeys = await chord(ada.tab);
  let grace = null, lin = null;
  // A browser of one's own, started before the step that person is in: what
  // starting one costs is no part of what they do, and a launch that never
  // starts is said rather than waited on for ever.
  const opens = (who) => Promise.race([
    // A fresh profile of the browser already open: three people, one browser,
    // each their own storage as a browser of their own has.
    story.person(who, {share: true}),
    new Promise((_, refuse) => setTimeout(() => refuse(new Error(`${who}’s browser did not start within 8 minutes`)), 480_000)),
  ]).then(person => { console.log(`${who}’s browser is up`); return person; });

  let invitation = '', linInvitation = '';
  await story.step('Ada invites Grace and Lin into a workspace', async () => {
    const began = Date.now();
    await story.web.coldLoad(ada, site);
    await until(ada.tab, arrived, 'Ada’s page arriving', stepLimit(6));
    const arrival = await ada.tab.evaluate('document.documentElement.dataset.arrival');
    if (arrival !== 'done') fail(`Ada’s page stopped: ${(await pageText(ada.tab)).slice(0, 300)}`);
    story.timing('Ada’s page load, to the desktop', Date.now() - began);
    story.web.scriptSize(ada);
    // Ada gives their name: the first thing a person does here, timed to the
    // search opening as the page's answer.
    await click(ada.tab, SHELL.empty.x, SHELL.empty.y);
    const pressed = Date.now();
    await key(ada.tab, 'k', adaKeys);
    await until(ada.tab, 'globalThis.tiloMount?.state()?.searchOpen === true', 'the search opening');
    story.timing('Ada’s first interaction (the search opening)', Date.now() - pressed);
    await givesName(ada.tab, adaKeys, 'Ada');
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
    // The invitations: a link each, as an invitation admits one person. Each
    // is made where the sheet shows it, and read as the page hands it over.
    await pressWhen(ada.tab, 'Invite', 'the Invite button');
    await pressWhen(ada.tab, 'Create link', 'the invite sheet’s Create link');
    const links = `(() => {
      const values = JSON.parse(${NODE_VALUES});
      return JSON.stringify(values.filter(value => new RegExp(${JSON.stringify(sealedLink.source)}).test(value)));
    })()`;
    invitation = (await until(ada.tab, `(() => {
      const found = ${links};
      return found === '[]' ? '' : found;
    })()`, 'the invitation link').then(JSON.parse))[0];
    await pressWhen(ada.tab, 'Create another link', 'the invite sheet’s Create another link');
    linInvitation = await until(ada.tab, `(() => {
      const found = JSON.parse(${links});
      return found.find(link => link !== ${JSON.stringify(invitation)}) ?? '';
    })()`, 'the second invitation link');
    // The sheet is done with: Escape puts it away, as a person does (the
    // sheet's Close is a footer action, published without a place to press).
    await key(ada.tab, 'Escape');
    await until(ada.tab, `(() => {
      const values = JSON.parse(${NODE_VALUES});
      return !values.some(value => new RegExp(${JSON.stringify(sealedLink.source)}).test(value));
    })()`, 'the invite sheet closing', stepLimit(2));
  });

  grace = await opens('Grace');
  lin = await opens('Lin');

  await story.step('Grace and Lin join the workspace', async () => {
    // Each arrives from the same invitation in a browser of their own, gives
    // their name, and opens an app to work in: the workspace's desktop is the
    // one every member sees, so what each opens is what everyone here finds.
    for (const who of ['Grace', 'Lin']) {
      const person = who === 'Grace' ? grace : lin;
      const keys = await chord(person.tab);
      const began = Date.now();
      await opensInvitation(person.tab, who === 'Grace' ? invitation : linInvitation, who);
      story.timing(`${who}’s invitation load`, Date.now() - began);
      const pressed = Date.now();
      await joins(person.tab, who);
      await until(person.tab, arrived, `${who}’s page arriving`, stepLimit(6));
      if (await person.tab.evaluate('document.documentElement.dataset.arrival') !== 'done') {
        fail(`${who}’s page stopped: ${(await pageText(person.tab)).slice(0, 300)}`);
      }
      await until(person.tab, `(() => {
        const state = globalThis.tiloMount?.state?.() ?? null;
        return state?.phase === 'presented' && state.workspaceName === ${JSON.stringify(WORKSPACE)};
      })()`, `${WORKSPACE} opening for ${who}`, stepLimit(10));
      story.web.signIn(person, Date.now() - pressed);
      await givesName(person.tab, keys, who);
      // Grace writes a note; Lin works in Tasks. Each is a window of the
      // workspace's one desktop, so both are on everyone's screen.
      if (who === 'Grace') {
        await search(person.tab, keys, 'notes', 'Notes');
        await person.tab.evaluate('globalThis.tiloMount?.requestSemantics?.()').catch(() => {});
        await until(person.tab, found('New note'), 'Notes’ New note');
        if (!await press(person.tab, 'New note')) fail('Notes’ New note is not where a person can press it');
        await until(person.tab, `JSON.stringify(globalThis.tiloMount?.state()?.semantics ?? null).includes('MultilineTextInput')`, 'Grace’s new note’s text field');
      } else {
        await search(person.tab, keys, 'tasks', 'Tasks');
        await until(person.tab, `(globalThis.tiloMount?.navigation?.getSnapshot?.().entries ?? [])
          .some(entry => entry.label === 'Tasks')`, 'Tasks opening for Lin');
      }
      // What they opened is on Ada's screen too: the workspace's desktop is
      // shared, and each person's window is everyone's.
      await ada.tab.evaluate('globalThis.tiloMount?.requestSemantics?.()').catch(() => {});
      await until(ada.tab, `(globalThis.tiloMount?.navigation?.getSnapshot?.().entries ?? []).length >= ${who === 'Grace' ? 1 : 2}`,
        `${who}’s window on Ada’s screen`, stepLimit(4));
    }
  });

  await story.step('Ada sees who is here and what each is working on', async () => {
    // The faces in the top bar open who is here.
    await opensHere(ada.tab);
    // Each row says where they are and what they are doing: what each is
    // viewing in the workspace's desktop.
    const working = `(() => {
      const out = [];
      const node = n => { out.push({name: n.name ?? '', description: n.description ?? ''}); (n.children || []).forEach(node); };
      const walk = n => { if (n.semantics) node(n.semantics); (n.children || []).forEach(walk); };
      const root = globalThis.tiloMount?.state()?.semantics?.root;
      if (root) walk(root);
      const rows = ['Grace', 'Lin'].map(who => out.find(n => n.name === who && /Viewing |Now: /.test(n.description ?? '')));
      return rows.every(Boolean) ? JSON.stringify(rows) : '';
    })()`;
    const panelNames = `(() => {
      const out = [];
      const node = n => { if (n.name || n.description) out.push([n.name ?? '', n.description ?? '']); (n.children || []).forEach(node); };
      const walk = n => { if (n.semantics) node(n.semantics); (n.children || []).forEach(walk); };
      const root = globalThis.tiloMount?.state?.()?.semantics?.root;
      if (root) walk(root);
      return JSON.stringify(out.filter(([name, said]) => name === 'Grace' || name === 'Lin' || /people|loading/i.test(name + ' ' + said)));
    })()`;
    await ada.tab.evaluate('globalThis.tiloMount?.requestSemantics?.()').catch(() => {});
    try {
      seenHere = JSON.parse(await until(ada.tab, working,
        'Grace and Lin in Ada’s people panel, each named with what they are working on', stepLimit(3)));
    } catch (thrown) {
      fail(`${thrown.message}; Ada’s panel said: ${await ada.tab.evaluate(panelNames).catch(() => 'unreadable')}`);
    }
  });

  await story.step('Ada follows Grace and sees Grace’s screen live', async () => {
    // Follow is on Grace's row of the panel Ada just opened.
    await ada.tab.evaluate('globalThis.tiloMount?.requestSemantics?.()').catch(() => {});
    if (!await ada.tab.evaluate(foundMatching('^Follow Grace$'))) {
      await opensHere(ada.tab);
    }
    // The window Grace works in, and what it shows, as both desktops name it.
    const active = `globalThis.tiloMount?.navigation?.getSnapshot?.()?.active ?? null`;
    const at = await grace.tab.evaluate(`JSON.stringify(${active})`);
    const label = await grace.tab.evaluate(`(() => {
      const s = globalThis.tiloMount?.navigation?.getSnapshot?.() ?? null;
      return (s?.entries ?? []).find(entry => entry.route?.tab === s?.active?.tab)?.label ?? '';
    })()`);
    if (!label) fail(`Grace’s screen names no window she is in: ${at}`);
    await pressWhen(ada.tab, 'Follow Grace', 'Follow Grace in Ada’s people panel');
    await shows(ada.tab, 'Following Grace', 'Ada’s page saying Ada follows Grace', stepLimit(2));
    // Ada's view follows Grace's: the window Grace is in comes forward here,
    // and what it shows is on Ada's screen.
    await until(ada.tab, `JSON.stringify(${active}) === ${JSON.stringify(at)}`,
      `Grace’s window (${label}) brought forward on Ada’s screen`, stepLimit(4));
    await shows(ada.tab, label, `what Grace works in (${label}) on Ada’s screen`, stepLimit(4));
  });

  await story.step('Ada mentions Lin in Chat', async () => {
    await search(ada.tab, adaKeys, 'chat', 'Chat', 'so.tilo.chat');
    const opened = await openMarks(ada.tab);
    story.web.openApp(ada, opened.ok ? opened.presentMs : NaN, 'Chat');
    await ada.tab.evaluate('globalThis.tiloMount?.requestSemantics?.()').catch(() => {});
    await until(ada.tab, found('Message composer'), 'Chat’s message composer');
    if (!await press(ada.tab, 'Message composer')) fail('Chat’s message composer is not where a person can press it');
    // A mention is typed and picked from the list Chat offers under the `@`,
    // as a person writes one: the name, the pick, then the words.
    await typeText(ada.tab, '@Li');
    await key(ada.tab, 'Enter');
    await typeText(ada.tab, ` ${mention.slice('@Lin '.length)}`);
    // The pick puts the name in; the words after it are typed. What the
    // composer holds is read as what it says, never as the exact spacing a
    // pick and a typed space make together.
    await shows(ada.tab, words, 'Ada’s words in Chat’s composer', stepLimit(3));
    if (!await ada.tab.evaluate(`(${TREE_TEXT}).includes('@Lin')`)) fail('the composer holds no mention of Lin');
    await pressWhen(ada.tab, 'Send message', 'Chat’s Send message');
    // Sent, so the conversation holds it and the composer does not.
    await until(ada.tab, `!(${TREE_TEXT}).includes('No messages yet')`, 'Ada’s message in #chat', stepLimit(3));
    await shows(ada.tab, words, 'Ada’s words in the conversation', stepLimit(3));
    await shows(ada.tab, '@Lin', 'the mention of Lin in the conversation', stepLimit(3));
  });

  await story.step('Lin is told of the mention and opens it', async () => {
    // Lin looks at their own screen now, as a person does when told something:
    // their page is the one the browser draws, so what they see is read at
    // the pace they see it.
    await lin.tab.send('Page.bringToFront').catch(() => {});
    // The bell counts what Lin has not seen; the notice is one of them.
    const told = Date.now();
    await pressNaming(lin.tab, '^Notifications, 1 unread$', 'Lin’s bell counting the mention', stepLimit(6));
    story.timing('Ada’s mention reaching Lin’s page, to the bell', Date.now() - told);
    // What the centre lists it as, then its own Open.
    await shows(lin.tab, 'Ada mentioned you', 'the mention in Lin’s notifications', stepLimit(2));
    await pressWhen(lin.tab, 'Open', 'the notification’s Open');
    // It opens what it told: the conversation in front on Lin's screen, and
    // the notice settled off the list.
    await until(lin.tab, `(() => {
      const s = globalThis.tiloMount?.navigation?.getSnapshot?.() ?? null;
      const front = (s?.entries ?? []).find(entry => entry.route?.tab === s?.active?.tab);
      return front?.label === 'chat';
    })()`, 'the conversation in front on Lin’s screen', stepLimit(4));
    await until(lin.tab, `!(${TREE_TEXT}).includes('Ada mentioned you')`, 'the notice settled', stepLimit(2));
  });
} catch (thrown) {
  if (!(thrown instanceof StoryStopped)) {
    error = thrown;
    console.error(`FAIL: ${thrown?.stack ?? thrown}`);
  }
}
// A last look at every screen, so the recording ends on what they saw.
await sleep(1500);
const result = await story.finish({error, extra: {people: seenHere}});
console.log(JSON.stringify({completed: result.completed, withinBudget: result.withinBudget, brokeAt: result.brokeAt, timings: result.timings}, null, 2));
process.exit(result.completed && result.withinBudget ? 0 : 1);
