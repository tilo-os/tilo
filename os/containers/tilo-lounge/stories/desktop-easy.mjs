// The story of someone new to Tilo, finding their way around the desktop
// without being told how: they open their workspace, add a strip, switch to
// another workspace and back, and find their settings.
//
// Every step is done through what the screen shows: the controls the desktop
// publishes for a screen reader, pressed where they stand. No keyboard chord
// a newcomer would not know is used to find anything, so what the recording
// proves is that each thing was findable. The pixels are recorded for the
// judge.
//
// usage: node desktop-easy.mjs SITE_URL OUT_DIR [SERVICE_LOG…]
// Writes OUT_DIR/story.json (stories/recorder.mjs) and exits non-zero when a
// step failed or ran over its budget.
import {execFileSync} from 'node:child_process';
import {sleep} from '../../../../tools/web/chrome.mjs';
import {stepLimit} from '../integration/waits.mjs';
import {click, found, foundMatching, key, pressMatching, pressNamed, typeText, until} from '../integration/shell.mjs';
import {recorder, StoryStopped} from './recorder.mjs';

const [site, out, ...logs] = process.argv.slice(2);
if (!site || !out) {
  console.error('usage: node desktop-easy.mjs SITE_URL OUT_DIR [SERVICE_LOG…]');
  process.exit(2);
}

const fail = message => { throw new Error(message); };
// The stack's own owner, for the one action only they take in this story: the
// test-user-stories runner hands their script over. A page's person on this
// stack is a guest, and a guest is an editor; adding a strip belongs to whoever
// governs the workspace.
const stack = process.env.STORY_STACK ?? '';
// The person this page joined as, by the root key its membership names.
const publicKey = async tab => {
  const kept = await tab.evaluate(`(() => {
    const all = JSON.parse(globalThis.localStorage.getItem('tilo-host-memberships') ?? '{}');
    return JSON.stringify(Object.values(all).flatMap(entry => (entry?.userPublicKey ? [entry.userPublicKey] : [])));
  })()`);
  const [user] = JSON.parse(kept);
  if (!user) fail(`the page joined no workspace of its own: ${kept}`);
  return user;
};
// Wait until the desktop publishes a node named `name`, then press it.
const pressWhen = async (tab, name, what, timeout = stepLimit(3)) => {
  await tab.evaluate('globalThis.tiloMount?.requestSemantics?.()').catch(() => {});
  await until(tab, found(name), what, timeout);
  if (!await pressNamed(tab, name)) fail(`${what} is not where a person can press it`);
};
// The same, for a control the page names by its own words (`pattern`, a
// RegExp source): the bar names its workspace and the strip after them.
const pressNaming = async (tab, pattern, what, timeout = stepLimit(3)) => {
  await tab.evaluate('globalThis.tiloMount?.requestSemantics?.()').catch(() => {});
  await until(tab, foundMatching(pattern), what, timeout);
  if (!await pressMatching(tab, pattern)) fail(`${what} is not where a person can press it`);
};
// The switcher, open: the workspace's name in the bar lists the workspaces the
// person can be in, and a new one. A press that landed on something else first
// is pressed again, as a person would.
const opensSwitcher = async tab => {
  for (let attempt = 1; attempt <= 2; attempt += 1) {
    await pressNaming(tab, '^Switch workspace, now in .+$', 'the workspace name in the bar');
    if (await tab.waitFor(found('New workspace'), {timeout: stepLimit(2)}).catch(() => false)) return;
  }
  fail(`the workspace name opened no switcher: ${await names(tab)}`);
};
// Wait until the desktop says `source` (a RegExp source, by whole line) among
// the names and values it publishes.
const says = (tab, source, what, timeout) => tab.evaluate('globalThis.tiloMount?.requestSemantics?.()').catch(() => {})
  .then(() => until(tab, `new RegExp(${JSON.stringify(source)}, 'm').test((() => {
    const out = [];
    const node = n => { for (const part of [n.name, n.value]) if (part) out.push(part); (n.children || []).forEach(node); };
    const walk = n => { if (n.semantics) node(n.semantics); (n.children || []).forEach(walk); };
    const root = globalThis.tiloMount?.state()?.semantics?.root;
    if (root) walk(root);
    return out.join('\\n');
  })())`, what, timeout));
// The workspace the page has presented, as the bar names it.
const present = name => `(() => {
  const state = globalThis.tiloMount?.state?.() ?? null;
  return state?.phase === 'presented' && state.workspaceName === ${JSON.stringify(name)};
})()`;
// Every name and value the desktop publishes now: what a person saw, kept
// with a failure that waited for something that did not come.
const names = tab => tab.evaluate(`(() => {
  const out = [];
  const node = n => { for (const part of [n.name, n.value, n.description]) if (part) out.push(part); (n.children || []).forEach(node); };
  const walk = n => { if (n.semantics) node(n.semantics); (n.children || []).forEach(walk); };
  const root = globalThis.tiloMount?.state()?.semantics?.root;
  if (root) walk(root);
  return JSON.stringify(out);
})()`).catch(() => 'unreadable');

const story = recorder({id: 'desktop-easy', out, logs});
let error = null;
// The workspace the newcomer arrived in, as the bar names it.
let first = '';
try {
  const robin = await story.person('Robin');

  await story.step('A newcomer opens their workspace', async () => {
    const began = Date.now();
    await story.web.coldLoad(robin, site);
    await until(robin.tab, `['done', 'stopped'].includes(document.documentElement.dataset.arrival)`,
      'Robin’s page arriving', stepLimit(6));
    if (await robin.tab.evaluate('document.documentElement.dataset.arrival') !== 'done') {
      fail(`Robin’s page stopped: ${(await robin.tab.evaluate("document.getElementById('status')?.textContent ?? ''")).slice(0, 300)}`);
    }
    story.timing('Robin’s page load, to the desktop', Date.now() - began);
    story.web.scriptSize(robin);
    // The workspace they land in, as its name in the bar says.
    first = await until(robin.tab, `(() => {
      const state = globalThis.tiloMount?.state?.() ?? null;
      return state?.phase === 'presented' && (state.workspaceName ?? '').trim() ? state.workspaceName : '';
    })()`, 'their workspace, named in the bar', stepLimit(4));
  });

  // The deployment's owner makes the newcomer an owner of the workspace they
  // arrived in: the normal action (`tilo workspace people add`), and what a
  // person who keeps a shared workspace's layout is. Nothing of it shows on
  // their screen.
  const newcomer = await publicKey(robin.tab);
  if (!stack) fail('the stack’s own owner is not at hand (STORY_STACK names stack.sh)');
    execFileSync('bash', [stack, 'owner', newcomer], {stdio: ['ignore', 'ignore', 'inherit']});
  console.log(`the workspace's owner made ${newcomer} an owner; the bar is read again`);

  // The workspace answers a role it was not told yet on its own time: the
  // column is read until it offers New strip. What the page did meanwhile is
  // logged, so a page that stopped answering reads apart from one that never
  // offered the button. Waiting for what the deployment's owner allowed is no
  // part of what the newcomer does.
  const wait = Date.now();
  for (let tries = 0; ; tries += 1) {
    const seen = await robin.tab.evaluate(`(() => {
      const state = globalThis.tiloMount?.state?.() ?? null;
      const out = [];
      const node = n => { if (n.name) out.push(n.name); (n.children || []).forEach(node); };
      const walk = n => { if (n.semantics) node(n.semantics); (n.children || []).forEach(walk); };
      if (state?.semantics?.root) walk(state.semantics.root);
      return JSON.stringify({frames: state?.frames ?? null, strip: out.includes('New strip'), invite: out.includes('Invite')});
    })()`).catch(error => `unreadable (${error?.message ?? error})`);
    if (tries % 8 === 0) console.log(`the strip column after ${Math.round((Date.now() - wait) / 1000)} s: ${seen}`);
    if (typeof seen === 'string' && seen.startsWith('{') && JSON.parse(seen).strip) break;
    if (Date.now() - wait > stepLimit(9)) {
      fail(`the bar offered no New strip after ${Math.round((Date.now() - wait) / 1000)} s: ${seen} (at ${await names(robin.tab)})`);
    }
    await sleep(500);
  }
  story.timing('The workspace’s owner allowed, to the strip column offering', Date.now() - wait);

  await story.step('They add a strip', async () => {
    // The New strip button stands at the foot of the strip column, at rest.
    await pressWhen(robin.tab, 'New strip', 'the New strip button by the strips');
    const began = Date.now();
    await until(robin.tab, found('Strip name'), 'the new strip’s name, asked in place', stepLimit(2));
    story.timing('The strip made, to its name being asked', Date.now() - began);
    await typeText(robin.tab, 'Writing');
    await key(robin.tab, 'Enter');
    await says(robin.tab, '^Show strips, now on Writing$', 'the bar naming the strip Writing', stepLimit(3));
  });

  await story.step('They switch to another workspace and back', async () => {
    // The bar's name opens the switcher: the workspace they are in, another
    // one, and a new one.
    await opensSwitcher(robin.tab);
    await pressWhen(robin.tab, 'New workspace', 'the switcher’s New workspace');
    // The page asks what to call it, in a card of its own.
    const asked = await robin.tab.waitFor(`!!document.querySelector('.tilo-entry-card input')`, {timeout: stepLimit(2)})
      .catch(() => false);
    const next = 'Studio';
    if (asked) {
      await typeText(robin.tab, next);
      const create = JSON.parse(await robin.tab.evaluate(`(() => {
        const button = [...document.querySelectorAll('.tilo-entry-card button')]
          .find(candidate => candidate.textContent.trim() === 'Create');
        if (!button) return 'null';
        const rect = button.getBoundingClientRect();
        return JSON.stringify({x: rect.left + rect.width / 2, y: rect.top + rect.height / 2});
      })()`));
      if (!create) fail(`the card offers no Create: ${await names(robin.tab)}`);
      await click(robin.tab, create.x, create.y);
      await until(robin.tab, present(next), `${next} opening`, stepLimit(10));
    } else {
      // A host that keeps no workspaces of its own makes one and names it
      // itself: whatever name it gives is the workspace they are in now.
      await until(robin.tab, `(() => {
        const state = globalThis.tiloMount?.state?.() ?? null;
        return state?.phase === 'presented' && (state.workspaceName ?? '').trim()
          && state.workspaceName !== ${JSON.stringify(first)};
      })()`, 'the new workspace opening', stepLimit(10));
    }
    // And back: the bar's name opens the switcher, where the workspace they
    // arrived in is listed.
    await opensSwitcher(robin.tab);
    await pressWhen(robin.tab, first, `${first} in the switcher`);
    await until(robin.tab, present(first), `${first} opening again`, stepLimit(10));
  });

  await story.step('They find their settings', async () => {
    // Their own face in the bar opens their card, where Settings leads.
    await pressNaming(robin.tab, '\\(you\\): your profile$', 'their own face in the bar');
    const began = Date.now();
    await pressWhen(robin.tab, 'Settings', 'Settings on their own card');
    await until(robin.tab, `(globalThis.tiloMount?.navigation?.getSnapshot?.().entries ?? [])
      .some(entry => entry.label === 'Settings')`, 'Settings opening', stepLimit(6));
    story.timing('Settings, from the press to its window', Date.now() - began);
    // Its own pages, so what opened is Settings and not a window named after it.
    await says(robin.tab, '^Settings categories$', 'Settings’ own pages', stepLimit(4));
  });
} catch (thrown) {
  if (!(thrown instanceof StoryStopped)) {
    error = thrown;
    console.error(`FAIL: ${thrown?.stack ?? thrown}`);
  }
}
// A last look at the screen, so the recording ends on what they saw.
await sleep(1500);
const result = await story.finish({error, extra: {workspace: first}});
console.log(JSON.stringify({completed: result.completed, withinBudget: result.withinBudget, brokeAt: result.brokeAt, timings: result.timings}, null, 2));
process.exit(result.completed && result.withinBudget ? 0 : 1);
