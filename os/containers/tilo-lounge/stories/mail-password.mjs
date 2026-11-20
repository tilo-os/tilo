// The story of a mail account a person connects with only its address and password: Ada opens
// Mail and tells it the address alone. Mail works out where the account's mail lives from the
// address (the settings document the provider publishes for mail programs); where that
// document is not reachable the page asks for the servers, and she names them. The account
// signs in, and she reads the newest message, searches for an older one, and answers it.
//
// The account is the stack's stand-in mail provider (TILO_SH_MAIL_STAND_IN=1), which holds a
// few made-up messages, keeps the mail it is handed, and publishes the settings document where
// Mail looks: `stack.sh mail-account` prints what a person types. The document is at
// https://autoconfig.<domain>/, which takes a port a check may not bind, so there the page
// asks for the servers and the step says so.
//
// usage: node mail-password.mjs SITE_URL OUT_DIR [SERVICE_LOG…]
// Writes OUT_DIR/story.json (stories/recorder.mjs) and exits non-zero when a step failed or
// ran over its budget.
import {execFileSync} from 'node:child_process';
import {sleep} from '../../../../tools/web/chrome.mjs';
import {stepLimit} from '../integration/waits.mjs';
import {boxOf, canvasTopLeft, focusShell, found, key, nodesOf, poll, pressBox, pressNamed, pressWidget,
  searcher, TREE_TEXT} from '../integration/shell.mjs';
import {recorder, StoryStopped} from './recorder.mjs';

const [site, out, ...logs] = process.argv.slice(2);
if (!site || !out) {
  console.error('usage: node mail-password.mjs SITE_URL OUT_DIR [SERVICE_LOG…]');
  process.exit(2);
}
const stack = process.env.STORY_STACK ?? '';

// The account the stack's stand-in mail provider takes, as `stack.sh mail-account` prints it.
const account = () => {
  if (!stack) throw new Error('STORY_STACK names no stack, so the stand-in account is unknown');
  const said = execFileSync('bash', [stack, 'mail-account'], {encoding: 'utf8'});
  const field = name => said.match(new RegExp(`^\\s*${name}:\\s*(\\S+)`, 'm'))?.[1] ?? '';
  return {
    address: field('Email address'),
    password: field('Password'),
    imap: said.match(/Incoming \(IMAP\):\s*(localhost:\d+)/)?.[1] ?? '',
    smtp: said.match(/Outgoing \(SMTP\):\s*(localhost:\d+)/)?.[1] ?? '',
  };
};
// A server as the accounts page writes it: "localhost, port 25424".
const at = server => {
  const [host, port] = server.split(':');
  return `${host}, port ${port}`;
};

// What a wait that runs out says: what it waited for, and what the page showed.
const visible = async tab => {
  const nodes = await nodesOf(tab);
  return JSON.stringify(nodes.filter(node => node.name || node.id)
    .map(node => `${node.role}:${node.name}${node.id ? `#${node.id}` : ''}`).slice(0, 90));
};
const until = async (tab, expression, what, timeout = stepLimit(3)) => {
  const value = await tab.waitFor(expression, {timeout}).catch(() => false);
  if (value) return value;
  const shown = await tab.evaluate(`JSON.stringify({
    arrival: document.documentElement.dataset.arrival ?? null,
    refusal: document.documentElement.dataset.refusal ?? null,
    entry: document.documentElement.dataset.entry ?? null,
    text: (document.body.innerText || '').replace(/\\s+/g, ' ').trim().slice(0, 200),
  })`).catch(() => 'unreadable');
  throw new Error(`${what} did not happen within ${(timeout / 1000).toFixed(1)} s (the page: ${shown}; what it shows: ${await visible(tab)})`);
};
const fail = message => { throw new Error(message); };
// The wheel a mouse sends over the page, until the control `id` stands where a
// person can press it: the accounts page is taller than the window, and its
// last field and button are below what it shows.
const scrollTo = async (tab, id) => {
  const corner = JSON.parse(await canvasTopLeft(tab));
  for (let moved = 0; moved < 12; moved += 1) {
    const node = (await nodesOf(tab)).find(node => node.id === id && node.bounds);
    if (!node) fail(`the page offers no ${id}`);
    const [x0, y0, x1, y1] = boxOf(node.bounds);
    const [left, top] = [corner.x + (x0 + x1) / 2, corner.y + (y0 + y1) / 2];
    const inside = left >= corner.x && left <= corner.x + corner.width
      && top >= corner.y + 30 && top <= corner.y + corner.height - 30;
    if (inside) return;
    // The wheel goes over the control's own column: a page laid out beside
    // another window scrolls under its own width, not the middle of the screen.
    const at = {x: Math.min(Math.max(left, corner.x + 8), corner.x + corner.width - 8), y: corner.y + corner.height / 2};
    await tab.send('Input.dispatchMouseEvent', {type: 'mouseMoved', ...at});
    await tab.send('Input.dispatchMouseEvent', {type: 'mouseWheel', ...at, deltaX: 0, deltaY: 140});
    await sleep(300);
  }
  fail(`the ${id} never came on screen as the page scrolled`);
};
const {search} = searcher({until, failed: fail, dump: async () => {}});
// A search a person makes again where the page was still drawing: the chord
// sent while a window mounts opens nothing, and pressing it again is what a
// person does.
const searchAgain = async (tab, query, row, opens = null) => {
  // The shell hears the chord only while its canvas holds the keyboard, which
  // a card the page showed takes: it is given back and the chord tried again.
  for (let tried = 0; tried < 4; tried += 1) {
    if (await tab.evaluate('!!globalThis.tiloMount?.state?.()?.searchOpen').catch(() => false)) break;
    await focusShell(tab);
    await key(tab, 'k', chord);
    if (await tab.waitFor('!!globalThis.tiloMount?.state?.()?.searchOpen', {timeout: 5000}).catch(() => false)) break;
    await sleep(1000);
  }
  await search(tab, chord, query, row, opens, {focus: false});
};
// A press on a control whose answer is `expression`, among the names the page
// may call it: a press that only brings the window in front changes nothing,
// and a person presses again.
const pressAny = async (tab, names, expression, what) => {
  for (const name of names) {
    for (let pressed = 0; pressed < 2; pressed += 1) {
      if (!await pressNamed(tab, name)) break;
      if (await poll(tab, expression, stepLimit(2))) return;
    }
  }
  fail(`${what}: none of ${names.join(', ')} opened it (${await visible(tab)})`);
};
const arrived = `['done', 'stopped'].includes(document.documentElement.dataset.arrival)`;
// The desktop's own text, as a person reads it.
const shows = (tab, text, what, timeout) => tab.evaluate('globalThis.tiloMount?.requestSemantics?.()').catch(() => {})
  .then(() => until(tab, `(${TREE_TEXT}).includes(${JSON.stringify(text)})`, what, timeout));
const platformChord = async tab => (await tab.evaluate('/Mac/.test(navigator.platform)')) ? 4 : 2;
// The search chord this page's platform uses (Control, or Command on a Mac).
let chord = 2;

const story = recorder({id: 'mail-password', out, logs});
const mail = account();
if (!mail.address || !mail.password || !mail.imap || !mail.smtp) {
  console.error(`the stack printed no account to sign in: ${JSON.stringify(mail)}`);
  process.exit(2);
}
let error = null;
try {
  const ada = await story.person('Ada');
  chord = await platformChord(ada.tab);

  await story.step('Ada opens Mail and enters their address and password', async () => {
    const began = Date.now();
    await story.web.coldLoad(ada, site);
    await until(ada.tab, arrived, 'Ada’s page arriving', stepLimit(6));
    story.timing('Ada’s page load, to the desktop', Date.now() - began);
    story.web.scriptSize(ada);
    // A hosted person's mail account lives on the server that keeps their own
    // workspace, so Ada first makes one of her own and works there.
    await searchAgain(ada.tab, 'new workspace', 'New workspace');
    await until(ada.tab, `document.querySelector('.tilo-entry-card input') !== null`,
      'the card asking what to name it', stepLimit(4));
    await ada.tab.evaluate(`(() => {
      const input = document.querySelector('.tilo-entry-card input');
      input.value = 'Mailbox';
      input.dispatchEvent(new Event('input', {bubbles: true}));
      const button = [...document.querySelectorAll('.tilo-entry-card button')]
        .find(candidate => candidate.textContent.trim() === 'Create' && !candidate.disabled);
      button?.click();
      return !!button;
    })()`);
    await until(ada.tab, `(() => {
      const chosen = JSON.parse(globalThis.localStorage.getItem('tilo-chosen-workspace') ?? 'null');
      const entered = JSON.parse(globalThis.localStorage.getItem('tilo-entered-workspaces') ?? '[]');
      const kept = (Array.isArray(entered) ? entered : []).find(entry => entry.workspace === chosen?.workspace);
      const open = document.documentElement.dataset.entry === 'open' && ${arrived};
      return open && kept?.name === 'Mailbox' ? chosen.workspace : '';
    })()`, 'Ada’s own workspace opening', stepLimit(6));
    await until(ada.tab, `globalThis.tiloMount?.state?.()?.phase === 'presented'`,
      'the new workspace drawn', stepLimit(4));
    // Mail, opened the way a person opens an app.
    await searchAgain(ada.tab, 'mail', 'Mail', 'so.tilo.mail');
    // Mail's window holds no account yet, and offers to add one.
    await shows(ada.tab, 'No email account yet', 'Mail’s first window', stepLimit(6));
    // The account is added where the person's accounts are kept: Mail's own
    // offer opens Settings there.
    await pressAny(ada.tab, ['Add an email account', 'Add account'],
      `(${TREE_TEXT}).includes('Other mail account')`, 'adding an account');
    await pressAny(ada.tab, ['Other mail account'],
      `(${TREE_TEXT}).includes('Email address')`, 'choosing another provider');
    // A person presses the field and types the address, and nothing else.
    if (!await pressWidget(ada.tab, 'settings.web.accounts.address')) fail('the address field is not where a person can press it');
    await story.web.type(ada, mail.address);
    if (!await pressWidget(ada.tab, 'settings.web.accounts.find')) fail('the accounts page offers no Set up');
  });

  await story.step('Ada names the server when Mail cannot find it, and connects', async () => {
    // Mail works the servers out from the address alone where the provider's
    // settings document is reachable; where it is not, the page asks for them
    // and a person names them. The measure says which happened.
    const asked = await poll(ada.tab, `(${TREE_TEXT}).includes(${JSON.stringify(at(mail.imap))})
      ? ${JSON.stringify(at(mail.imap))} : ''`, stepLimit(1));
    let byMail = 0;
    if (asked) {
      byMail = 1;
      console.log(`Mail found the servers by itself: ${mail.imap} and ${mail.smtp}`);
      await scrollTo(ada.tab, 'settings.web.accounts.password');
      if (!await pressWidget(ada.tab, 'settings.web.accounts.password')) fail('the found step offers no password field');
      await story.web.type(ada, mail.password);
    } else {
      await shows(ada.tab, 'Incoming server (IMAP)', 'the page asking for the servers', stepLimit(3));
      const [incoming, incomingPort] = mail.imap.split(':');
      const [outgoing, outgoingPort] = mail.smtp.split(':');
      for (const [id, value] of [
        ['settings.web.accounts.incoming', incoming],
        ['settings.web.accounts.incoming-port', incomingPort],
        ['settings.web.accounts.outgoing', outgoing],
        ['settings.web.accounts.outgoing-port', outgoingPort],
        ['settings.web.accounts.password', mail.password],
      ]) {
        await scrollTo(ada.tab, id);
        if (!await pressWidget(ada.tab, id)) fail(`the page offers no ${id} to type in`);
        await story.web.type(ada, value);
      }
      console.log(`Mail asked for the servers; they were named (${mail.imap}, ${mail.smtp})`);
    }
    story.measure('servers-found-by-mail', byMail, byMail ? 'Mail found them from the address alone' : 'Mail asked, and they were typed');
    await scrollTo(ada.tab, 'settings.web.accounts.add');
    // What the person waits for: the press that signs the account in, to the
    // page saying it is added.
    const signing = Date.now();
    if (!await pressWidget(ada.tab, 'settings.web.accounts.add')) fail('the page offers no Add account');
    await shows(ada.tab, 'is added', 'the account being added', stepLimit(5));
    story.web.action(ada, Date.now() - signing, 'signing the account in');
    console.log(`the account was added`);
    // Mail's own window then holds the account: it is opened again so it is the
    // window in front.
    await searchAgain(ada.tab, 'mail', 'Mail', 'so.tilo.mail');
    await shows(ada.tab, mail.address, 'the account in Mail', stepLimit(5));
  });

  await story.step('Ada reads the newest mail', async () => {
    const began = Date.now();
    const newest = 'Your badge is at the reception from Monday.';
    // The account is in Mail's own list, and its inbox's newest message stands
    // first: a person at the keyboard walks to it and opens it. The mail may
    // still be arriving from the server, so the walk is tried again as a person
    // would until the message shows.
    await shows(ada.tab, mail.address, 'the account in Mail', stepLimit(4));
    const pressed = Date.now();
    for (let walked = 0; walked < 8; walked += 1) {
      await key(ada.tab, 'k');
      if (await poll(ada.tab, `(${TREE_TEXT}).includes(${JSON.stringify(newest)})`, 1500)) break;
    }
    await shows(ada.tab, newest, 'the newest message’s words', stepLimit(2));
    await key(ada.tab, 'Enter');
    await shows(ada.tab, newest, 'the opened message’s words', stepLimit(3));
    story.web.action(ada, Date.now() - pressed, 'opening the newest message');
    story.timing('The newest message, from the key to its words', Date.now() - began);
  });

  await story.step('Ada searches for an older one', async () => {
    // A person searches the mail from the keyboard's search, over Mail: it
    // lists the messages it holds by subject, sender and words.
    await searchAgain(ada.tab, 'quarterly', 'Quarterly plan');
    await shows(ada.tab, 'The draft is in the shared folder.', 'the older message’s words', stepLimit(3));
  });

  await story.step('Ada sends a reply, and it arrives', async () => {
    const pressed = Date.now();
    if (!await pressNamed(ada.tab, 'Reply')) fail('the message offers no Reply');
    await shows(ada.tab, 'Send', 'the composer', stepLimit(4));
    const words = `Thanks, see you Monday ${Date.now().toString(36)}`;
    // The composer's own field, as a person writes in it: the field that says
    // what it is for, or the only field the composer offers.
    const opened = await until(ada.tab, found('Write your message'), 'the composer’s field', stepLimit(4)).catch(() => false);
    if (opened) {
      if (!await pressNamed(ada.tab, 'Write your message')) fail('the composer’s field is not where a person can press it');
    } else {
      const fields = (await nodesOf(ada.tab)).filter(node => node.role === 'MultilineTextInput' && node.bounds);
      if (!fields.length) fail(`the composer offers no field to write in: ${await visible(ada.tab)}`);
      if (!await pressBox(ada.tab, fields[0].bounds)) fail('the composer’s field is not where a person can press it');
    }
    await story.web.type(ada, words);
    if (!await pressNamed(ada.tab, 'Send')) fail('the composer offers no Send');
    story.web.action(ada, Date.now() - pressed, 'sending a reply');
    // It arrives: the account's server holds it, and Mail shows it once it asks
    // the server again.
    await searchAgain(ada.tab, 'refresh folder', 'Refresh folder');
    await searchAgain(ada.tab, 'thanks, see you', 'Re: Quarterly plan');
    await shows(ada.tab, words, 'the reply’s words', stepLimit(4));
    console.log(`the reply arrived: ${words}`);
  });
} catch (thrown) {
  if (!(thrown instanceof StoryStopped)) error = thrown;
}
// A last look at the screen, so the recording ends on what Ada saw.
await sleep(1500);
const result = await story.finish({error});
console.log(JSON.stringify({completed: result.completed, withinBudget: result.withinBudget, brokeAt: result.brokeAt, timings: result.timings}, null, 2));
process.exit(result.completed && result.withinBudget ? 0 : 1);
