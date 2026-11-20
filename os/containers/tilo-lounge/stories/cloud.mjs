// The steps a story about someone's own cloud account walks before it parts
// from the others: a person who has never seen the site arrives, names
// themselves, makes a workspace, and reaches Settings' External accounts page
// to add an account on a provider's own page. The stories import these so the
// same flow is walked, and fails, in one place.
//
// Nothing here reads the product's state to make a step happen: it presses
// what the page publishes for a screen reader, types into the fields the page
// draws, and reads what the page says.
import {sleep} from '../../../../tools/web/chrome.mjs';
import {stepLimit} from '../integration/waits.mjs';
import {
  found, giveName, key, makeWorkspace, mirroredFields, openWorkspace, pressFieldNamed, pressNamed, pressWhen, shows,
  TREE_TEXT, type, waitUntil,
} from '../integration/shell.mjs';

/** The page's own word that a person has arrived at their desktop. */
export const ARRIVED = `['done', 'stopped'].includes(document.documentElement.dataset.arrival)`;

/** The page asked for what it publishes, and a moment for the answer to
 * arrive: what a read of the tree is preceded by. */
export const settle = async tab => {
  await tab.evaluate('globalThis.tiloMount?.requestSemantics?.()').catch(() => {});
  await sleep(400);
};

/** Every node the page publishes now, as `{name, value, role, bounds}`, so a
 * failure says what the page offered and not only what was looked for. */
export const nodesNow = async tab => {
  await settle(tab);
  return JSON.parse((await tab.evaluate(`(() => {
    const out = [];
    const node = n => {
      out.push({name: n.name ?? '', value: n.value ?? null,
        role: typeof n.role === 'string' ? n.role : '', bounds: n.bounds ?? null});
      (n.children || []).forEach(node);
    };
    const walk = n => { if (n.semantics) node(n.semantics); (n.children || []).forEach(walk); };
    const root = globalThis.tiloMount?.state()?.semantics?.root;
    if (root) walk(root);
    return JSON.stringify(out);
  })()`).catch(() => '[]')) || '[]');
};

/** What the page offers, each name with the role it is published under. */
export const offered = async tab => (await nodesNow(tab))
  .filter(node => node.name).map(node => node.role ? `${node.name} [${node.role}]` : node.name).slice(0, 120);

/** Press the field the page publishes as `name`, where the page lays its own
 * field for it over the canvas: a text field's box is not in the tree the
 * desktop publishes (only the canvas holds the pointer), so a person's press
 * is taken from the field's own place. Waits for the form as a person would,
 * and says what the page offers when none comes. */
export const pressField = async (tab, name, what, timeout = 45_000) => {
  const deadline = Date.now() + timeout;
  for (;;) {
    await tab.evaluate('globalThis.tiloMount?.requestSemantics?.()').catch(() => {});
    if (await pressFieldNamed(tab, name)) return true;
    if (Date.now() >= deadline) {
      const fields = await tab.evaluate(mirroredFields()).catch(() => '[]');
      throw new Error(`${what} is not on the page; the fields it lays: ${fields}; it offers: ${JSON.stringify(await nodesNow(tab))}`);
    }
    await sleep(750);
  }
};

/** Wait until one of `cases` (`[what, expression]`) holds, and answer its
 * `what`: a step where two things could happen, each the page's own word. */
export const waitAny = async (tab, cases, what, timeout = stepLimit(6)) => {
  const deadline = Date.now() + timeout;
  for (;;) {
    for (const [label, expression] of cases) {
      if (await tab.evaluate(expression).catch(() => false)) return label;
    }
    if (Date.now() >= deadline) {
      throw new Error(`${what} did not happen within ${(timeout / 1000).toFixed(1)} s; the page offers: ${JSON.stringify(await offered(tab))}`);
    }
    await sleep(250);
  }
};

/** `person` opening `site` for the first time, naming themselves `name`, and
 * making a workspace called `workspace`: what every story's person does
 * before what it is about. `search` is the driver's own, so a search that goes
 * wrong is said in the driver's words. */
export const arrive = async ({story, person, site, name, workspace, command, search}) => {
  const began = Date.now();
  await story.web.coldLoad(person, site);
  await waitUntil(person.tab, ARRIVED, `${name}’s page arriving`, stepLimit(6));
  const arrival = await person.tab.evaluate('document.documentElement.dataset.arrival');
  if (arrival !== 'done') {
    const shown = await person.tab.evaluate('(document.body.innerText || "").slice(0, 300)').catch(() => 'unreadable');
    throw new Error(`${name}’s page stopped: ${shown}`);
  }
  story.timing(`${name}’s page load, to the desktop`, Date.now() - began);
  story.web.scriptSize(person);
  await search(person.tab, command, 'set your name', 'Set your name');
  await giveName(person.tab, name);
  await search(person.tab, command, 'new workspace', 'New workspace');
  await makeWorkspace(person.tab, workspace);
  await waitUntil(person.tab, openWorkspace(workspace), `${workspace} opening`, stepLimit(10));
  return workspace;
};

/** Press the node whose published name contains `text`, as a person reaches
 * a row whose name carries its description too. Answers the name pressed. */
export const pressContaining = async (tab, text, what) => {
  await settle(tab);
  const name = await tab.evaluate(`(() => {
    const hits = [];
    const node = n => { if (n.name && n.bounds) hits.push(n.name); (n.children || []).forEach(node); };
    const walk = n => { if (n.semantics) node(n.semantics); (n.children || []).forEach(walk); };
    const root = globalThis.tiloMount?.state()?.semantics?.root;
    if (root) walk(root);
    return hits.find(name => name.includes(${JSON.stringify(text)})) ?? '';
  })()`).catch(() => '');
  if (!name) throw new Error(`${what} is not on the page; it offers: ${JSON.stringify(await offered(tab))}`);
  await pressWhen(tab, name, what);
  return name;
};

/** Settings, opened the way search offers every app, then its Accounts page.
 * The shell's own card opens Settings too, but a press there lands where the
 * bar last laid the avatar out, which a workspace's first frames move. */
export const openExternalAccounts = async (tab, command, search) => {
  await search(tab, command, 'settings', 'Settings');
  await pressWhen(tab, 'Accounts', 'Accounts in Settings', stepLimit(6));
  await shows(tab, 'Add an account', 'the accounts page offering a provider', stepLimit(6));
};

/** The Nextcloud account step: Nextcloud, the server's address, and the sign
 * in that opens the server's own page. Answers the page targets open before
 * the press, so the caller can pick out the one it opens. */
export const beginNextcloudSignIn = async (tab, server) => {
  // Choosing the provider opens its form; a press the window took only as its
  // own focus shows no form, and is given again, as a person presses again
  // what did not take.
  const form = `(${TREE_TEXT}).includes('Server address')`;
  let shown = false;
  for (let attempt = 0; attempt < 3 && !shown; attempt++) {
    await pressWhen(tab, 'Nextcloud', 'Nextcloud among the providers')
      .catch(() => pressContaining(tab, 'Nextcloud', 'Nextcloud among the providers'));
    shown = await tab.waitFor(form, {timeout: 10_000}).catch(() => false);
  }
  if (!shown) {
    throw new Error(`choosing Nextcloud showed no server field; the page offers: ${JSON.stringify(await nodesNow(tab))}`);
  }
  await pressField(tab, 'Server address', 'the Nextcloud server field');
  await type(tab, server);
  // A press the busy page did not take leaves the address nowhere: pressed
  // again, with what was typed selected and typed over, as a person corrects
  // a field the typing did not reach.
  const addressed = async () => tab.evaluate('globalThis.tiloMount?.requestSemantics?.()').catch(() => {})
    .then(() => tab.waitFor(`(${TREE_TEXT}).includes(${JSON.stringify(server)})`, {timeout: 8000}))
    .catch(() => false);
  if (!await addressed()) {
    await pressField(tab, 'Server address', 'the Nextcloud server field');
    await key(tab, 'a', 2);
    await type(tab, server);
    if (!await addressed()) {
      throw new Error(`the server address did not reach the field; the page shows: ${JSON.stringify(await offered(tab))}`);
    }
  }
  const step = await tab.evaluate(`(() => {
    const nodes = [];
    const node = n => { if (n.name) nodes.push(n.name); (n.children || []).forEach(node); };
    const walk = n => { if (n.semantics) node(n.semantics); (n.children || []).forEach(walk); };
    const root = globalThis.tiloMount?.state()?.semantics?.root;
    if (root) walk(root);
    return nodes.includes('Sign in with Nextcloud');
  })()`).catch(() => false);
  if (!step) throw new Error(`the Nextcloud step offers no "Sign in with Nextcloud"; it offers: ${JSON.stringify(await offered(tab))}`);
};

/** The page targets open in `launched`'s browser now, by id: what a press is
 * watched from, so the page it opens is told apart from the pages already
 * there. */
export const pageTargets = async launched => new Set(
  (await launched.send('Target.getTargets')).result.targetInfos
    .filter(info => info.type === 'page').map(info => info.targetId));

/** The page target that appeared since `before`, or null once `timeout` ms
 * passed: a press that opens a page in the browser opens a target. */
export const newPageTarget = async (launched, before, timeout = stepLimit(6)) => {
  const deadline = Date.now() + timeout;
  for (;;) {
    const infos = (await launched.send('Target.getTargets')).result.targetInfos
      .filter(info => info.type === 'page' && !before.has(info.targetId));
    if (infos.length) return infos[0];
    if (Date.now() >= deadline) return null;
    await sleep(200);
  }
};

/** The text a plain web page shows: what a person reads on a page that is not
 * Tilo's canvas, such as the Nextcloud sign-in the browser opens. A button
 * written as an input shows its value, which the page's text leaves out. */
export const plainText = tab => tab.evaluate(`(() => {
  const buttons = [...document.querySelectorAll('input[type=submit], input[type=button]')]
    .filter(input => input.offsetParent !== null).map(input => input.value);
  return [document.body?.innerText || '', ...buttons].join(' ').replace(/\\s+/g, ' ').trim();
})()`).catch(() => '');

/** Wait until `tab`'s plain page shows `text`. */
export const plainShows = async (tab, text, what, timeout = stepLimit(8)) => {
  const deadline = Date.now() + timeout;
  while (!(await plainText(tab)).includes(text)) {
    if (Date.now() >= deadline) throw new Error(`${what} did not happen within ${(timeout / 1000).toFixed(1)} s`);
    await sleep(250);
  }
};

/** Type `text` into the plain page's `selector`, as a person does: the field
 * takes the keyboard, and every key goes to it. */
export const plainType = async (tab, selector, text) => {
  const focused = await tab.evaluate(`(() => {
    const field = document.querySelector(${JSON.stringify(selector)});
    if (!field) return false;
    field.focus();
    return document.activeElement === field;
  })()`).catch(() => false);
  if (!focused) throw new Error(`the page offers no ${selector} to type into`);
  await type(tab, text);
};

/** Press the first button of `tab`'s plain page whose words are `label`. */
export const plainPress = async (tab, label) => {
  const pressed = await tab.evaluate(`(() => {
    const wanted = ${JSON.stringify(label)};
    const button = [...document.querySelectorAll('button, input[type=submit], a.button, a[role=button]')]
      .find(candidate => (candidate.innerText || candidate.value || '').trim() === wanted);
    if (!button) return false;
    button.click();
    return true;
  })()`).catch(() => false);
  if (!pressed) throw new Error(`the page offers no "${label}"; it shows: ${JSON.stringify((await plainText(tab)).slice(0, 400))}`);
};

/** Sign in to the Nextcloud page `tab` the account step opened and grant Tilo
 * access, as a person does: the server first asks them to connect their
 * account, then shows its login form, then asks to grant access. */
export const nextcloudSignIn = async (tab, {user, password, beforeGrant = async () => {}}) => {
  await plainShows(tab, 'Log in', 'the server’s connect page', stepLimit(4));
  await plainPress(tab, 'Log in');
  await waitUntil(tab, 'Boolean(document.querySelector("input[name=user]"))', 'the server’s login form', stepLimit(4));
  await plainType(tab, 'input[name=user]', user);
  await plainType(tab, 'input[name=password]', password);
  await plainPress(tab, 'Log in');
  await plainShows(tab, 'Grant access', 'the server asking to grant access', stepLimit(4));
  await beforeGrant();
  await plainPress(tab, 'Grant access');
};

/** Everything a calendar of `who` holds on the server, as its raw iCalendar
 * texts: what an invitation delivered to that person would be among. */
export const calendarEntries = async ({base, who, password}) => {
  const auth = 'Basic ' + Buffer.from(`${who}:${password}`).toString('base64');
  const home = `${base}/calendars/${who}/`;
  const listing = await fetch(home, {method: 'PROPFIND', headers: {authorization: auth, depth: '1',
    'content-type': 'application/xml'}, body: `<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:"><d:prop><d:resourcetype/></d:prop></d:propfind>`}).catch(() => null);
  if (!listing?.ok) return [];
  const xml = await listing.text();
  // The calendars of the home, never the home itself: a depth-1 listing has
  // both, and only a calendar answers a calendar-query.
  const homeParts = new URL(home).pathname.replace(/\/$/, '').split('/').length;
  const hrefs = [...xml.matchAll(/<d:href>([^<]+)<\/d:href>/g)].map(match => match[1])
    .filter(href => href.endsWith('/') && href.replace(/\/$/, '').split('/').length > homeParts);
  const entries = [];
  for (const href of hrefs) {
    const report = await fetch(new URL(href, base), {method: 'REPORT', headers: {authorization: auth,
      depth: '1', 'content-type': 'application/xml'}, body: `<?xml version="1.0" encoding="utf-8"?>
<c:calendar-query xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
  <d:prop><d:getetag/><c:calendar-data/></d:prop>
  <c:filter><c:comp-filter name="VCALENDAR"><c:comp-filter name="VEVENT"/></c:comp-filter></c:filter>
</c:calendar-query>`}).catch(() => null);
    if (!report?.ok) continue;
    const body = await report.text();
    for (const match of body.matchAll(/<c:calendar-data[^>]*>([\s\S]*?)<\/c:calendar-data>/g)) {
      entries.push(match[1]);
    }
  }
  return entries;
};
