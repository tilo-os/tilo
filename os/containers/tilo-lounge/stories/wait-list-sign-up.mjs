// The story of a stranger at an invitation-only Tilo: the entry shows its wait
// list instead of a way in, they leave an address there, confirm it through the
// mail the entry sends, the person who runs the host invites them from the
// workspace's own wait list, and the invitation's link brings them into a
// workspace of their own.
//
// The host is a person, not a script: they open the owner link the stack's own
// owner made (`stack.sh owner`, as production's deployment adds its owners),
// arrive in the Lounge, and invite the address from the waiting card the
// workspace offers whoever may invite. The mails are the entry's own, kept by
// the stack's mail sink; each step's links are read from the mail a person
// would have opened.
//
// usage: node wait-list-sign-up.mjs SITE_URL OUT_DIR [SERVICE_LOG…]
// Writes OUT_DIR/story.json (stories/recorder.mjs) and exits non-zero when a
// step failed or ran over its budget.
import {execFileSync} from 'node:child_process';
import {sleep} from '../../../../tools/web/chrome.mjs';
import {stepLimit, keepRecoveryWords} from '../integration/waits.mjs';
import {leaveWaitListAddress, mailLink, poll, pressText, searcher, waitForMail, waitingCard}
  from '../integration/shell.mjs';
import {recorder, StoryStopped} from './recorder.mjs';

const [site, out, ...logs] = process.argv.slice(2);
if (!site || !out) {
  console.error('usage: node wait-list-sign-up.mjs SITE_URL OUT_DIR [SERVICE_LOG…]');
  process.exit(2);
}
const stack = process.env.STORY_STACK ?? '';
const mailDir = `${process.env.TILO_SH_STATE ?? ''}/mail`;
const hostOrigin = new URL(site).origin;
const STRANGER = 'ada@example.org';
const HOST = 'Lin';

// What a wait that runs out says: what it waited for, and where the page was.
const until = async (tab, expression, what, timeout = stepLimit(3)) => {
  const value = await tab.waitFor(expression, {timeout}).catch(() => false);
  if (value) return value;
  const shown = await tab.evaluate(`JSON.stringify({
    arrival: document.documentElement.dataset.arrival ?? null,
    refusal: document.documentElement.dataset.refusal ?? null,
    waitList: document.documentElement.dataset.waitList ?? null,
    entry: document.documentElement.dataset.entry ?? null,
    status: document.getElementById('status')?.textContent?.trim().slice(0, 200) ?? null,
    text: (document.body.innerText || '').replace(/\\s+/g, ' ').trim().slice(0, 300),
  })`).catch(() => 'unreadable');
  throw new Error(`${what} did not happen within ${(timeout / 1000).toFixed(1)} s (the page: ${shown})`);
};
const fail = message => { throw new Error(message); };
const {search} = searcher({until, failed: fail, dump: async () => {}});
const arrived = `['done', 'stopped'].includes(document.documentElement.dataset.arrival)`;
const refusedByWaitList = `document.documentElement.dataset.refusal === 'wait-list'`;
// The workspace the person is in, drawn and named: the Lounge is not theirs.
const ownWorkspace = `(() => {
  const name = document.documentElement.dataset.workspace ?? '';
  const drawn = document.documentElement.dataset.arrival === 'done'
    && !!document.querySelector('#tilo-page [data-p]');
  return drawn && name && !/lounge/i.test(name) ? name : '';
})()`;
const enteredLounge = `(() => {
  try { return JSON.parse(globalThis.localStorage.getItem('tilo-host-memberships') ?? '{}')[${JSON.stringify(hostOrigin)}]?.workspace ?? ''; }
  catch { return ''; }
})()`;
// The workspace the page has open, drawn: its name, once its page is up.
const drawn = `(() => {
  const name = document.documentElement.dataset.workspace ?? '';
  const up = document.documentElement.dataset.arrival === 'done'
    && !!document.querySelector('#tilo-page [data-p]');
  return up ? name : '';
})()`;
const chordOf = async tab => (await tab.evaluate('/Mac/.test(navigator.platform)')) ? 4 : 2;

const story = recorder({id: 'wait-list-sign-up', out, logs});
let error = null;
try {
  const ada = await story.person('Ada');
  const lin = await story.person(HOST);

  await story.step('A stranger opens Tilo and leaves their address on the wait list', async () => {
    const began = Date.now();
    await story.web.coldLoad(ada, site);
    await until(ada.tab, arrived, 'Ada’s page arriving', stepLimit(4));
    story.timing('Ada’s page load, to the entry', Date.now() - began);
    story.web.scriptSize(ada);
    // The entry refuses a stranger: the page stops entering and shows its wait
    // list, which is where a person leaves an address.
    await until(ada.tab, refusedByWaitList, 'the entry showing its wait list', stepLimit(4));
    await until(ada.tab, `document.documentElement.dataset.waitList === 'form'`, 'the wait list offering its field', stepLimit(4));
    const entry = await ada.tab.evaluate(`JSON.stringify({
      heading: document.getElementById('arrival-refusal-heading')?.textContent?.trim() ?? null,
      body: document.getElementById('arrival-refusal-body')?.textContent?.trim() ?? null,
      button: document.querySelector('#arrival-wait button')?.textContent?.trim() ?? null,
    })`).then(JSON.parse);
    console.log(`the entry: ${JSON.stringify(entry)}`);
    const pressed = Date.now();
    if (!await leaveWaitListAddress(ada.tab, STRANGER)) fail('the wait list offers no field to leave an address in');
    await until(ada.tab, `document.documentElement.dataset.waitList === 'inbox'`, 'the address being left', stepLimit(4));
    await until(ada.tab, 'document.documentElement.hasAttribute("data-wait-listed")', 'the page saying where the address stands', stepLimit(4));
    story.web.action(ada, Date.now() - pressed, 'leaving an address on the wait list');
    console.log(`the wait list says: ${await ada.tab.evaluate('document.getElementById("arrival-wait-done")?.textContent?.trim() ?? ""')}`);
  });

  await story.step('They confirm it from the mail', async () => {
    const confirmation = await waitForMail(mailDir, 'the confirmation mail', 1);
    const confirm = mailLink(confirmation, 'wait-list-confirm');
    if (!confirm) fail(`the confirmation mail carries no confirmation link: ${(confirmation.text ?? '').slice(0, 400)}`);
    console.log(`the confirmation mail reached ${confirmation.to} (${confirmation.subject})`);
    await ada.tab.navigate(confirm);
    await until(ada.tab, `document.documentElement.dataset.waitList === 'listed'`, 'the confirmation link listing the address', stepLimit(4));
    // The person comes back to the entry and the address is still theirs.
    await ada.tab.navigate(site);
    await until(ada.tab, arrived, 'Ada’s page arriving again', stepLimit(4));
    await until(ada.tab, refusedByWaitList, 'the entry after a reload', stepLimit(4));
    await until(ada.tab, `document.documentElement.dataset.waitList === 'listed'`, 'the confirmed address on a reload', stepLimit(4));
    const began = Date.now();
    await story.web.warmLoad(ada);
    await until(ada.tab, `document.documentElement.dataset.waitList === 'listed'`, 'the confirmed address again', stepLimit(4));
    story.timing('Ada’s page again, to the confirmed wait list', Date.now() - began);
  });

  await story.step(`The host opens the Lounge and becomes its owner`, async () => {
    await lin.tab.navigate(site);
    await until(lin.tab, arrived, `${HOST}’s page arriving`, stepLimit(6));
    await until(lin.tab, refusedByWaitList, 'the closed entry for the host', stepLimit(4));
    // The stack's own owner invites the host as an owner, as a deployment's
    // owners add one: `tilo do workspace/invitations#invite role=owner`, whose link
    // anyone opens to become an owner.
    if (!stack) fail('STORY_STACK names no stack, so the host cannot be made an owner');
    const said = execFileSync('bash', [stack, 'owner'], {encoding: 'utf8', timeout: 120_000});
    const link = said.trim().split('\n').filter(Boolean).at(-1) ?? '';
    if (!/[?&]invite=|\/i\/[A-Za-z0-9_-]+#/.test(link)) fail(`the stack's owner link is not an invitation: ${said.slice(0, 300)}`);
    await lin.tab.navigate(link);
    // An owner's link is an invitation like any other: the page offers it, and
    // a person takes it.
    const offered = await poll(lin.tab, `(() => {
      if (${drawn}) return 'in';
      const text = (document.body.innerText || '').toLowerCase();
      return text.includes('invited') || [...document.querySelectorAll('button')].some(button => button.textContent.trim() === 'Join')
        ? 'card' : '';
    })()`, stepLimit(4));
    if (offered === 'card') {
      for (const title of ['Join', 'Open my workspace', 'Accept']) {
        if (await pressText(lin.tab, title)) break;
      }
    }
    // The join founds the host's person: the entry shows their 24 recovery
    // words once, and the workspace waits for them to be kept.
    await keepRecoveryWords(lin.tab, 'the host: founded');
    await until(lin.tab, drawn, 'the owner link letting the host into the Lounge', stepLimit(4));
    await until(lin.tab, arrived, 'the Lounge arriving for the host', stepLimit(4));
    // A person arriving for the first time gives their name, so the people
    // they ask in know who did.
    await search(lin.tab, await chordOf(lin.tab), 'set your name', 'Set your name');
    await until(lin.tab, `!!document.querySelector('input[name=display-name]')`, 'the name form', stepLimit(4));
    await lin.tab.evaluate(`(() => {
      const input = document.querySelector('input[name=display-name]');
      input.value = ${JSON.stringify(HOST)};
      input.dispatchEvent(new Event('input', {bubbles: true}));
      input.closest('form').querySelector('button[type=submit]').click();
    })()`);
    await until(lin.tab, '!document.querySelector("input[name=display-name]")', 'the name kept', stepLimit(6));
    console.log(`the host is in the Lounge and goes by ${HOST}`);
  });

  await story.step('The host invites them from the wait list', async () => {
    // The waiting card is the workspace's own action, offered to whoever it
    // lets invite; the search reaches it.
    let card = null;
    for (const deadline = Date.now() + stepLimit(4); Date.now() < deadline && !card;) {
      await search(lin.tab, await chordOf(lin.tab), 'waiting', 'Waiting to join');
      card = await waitingCard(lin.tab);
      if (!card) await sleep(500);
    }
    if (!card) fail(`the host’s search offered no wait list: ${await lin.tab.evaluate('(document.body.innerText || "").slice(0, 400)')}`);
    console.log(`the host’s wait list: ${JSON.stringify(card)}`);
    if (!card.contacts.includes(STRANGER)) fail(`the wait list shows ${JSON.stringify(card.contacts)} (${card.said ?? 'nothing said'}), not ${STRANGER}`);
    if (!await pressText(lin.tab, 'Invite', '.tilo-waiting')) fail(`the wait list offers no Invite: ${JSON.stringify(card.buttons)}`);
    const mailed = await poll(lin.tab, `(() => {
      const sent = document.querySelector('.tilo-waiting-sent');
      const problem = document.querySelector('.tilo-waiting .tilo-entry-error');
      const share = document.querySelector('[data-card=share]');
      if (sent && !sent.hidden && sent.textContent.includes('emailed')) return 'mailed';
      if (problem && !problem.hidden && problem.textContent.trim()) return 'refused: ' + problem.textContent.trim();
      if (share) return 'shared instead: ' + share.textContent.trim().slice(0, 300);
      return '';
    })()`, stepLimit(4));
    if (mailed !== 'mailed') fail(`the wait list did not mail the invitation (${mailed})`);
  });

  await story.step(`They open the invitation mail’s link`, async () => {
    const invitation = await waitForMail(mailDir, 'the invitation mail', 2);
    console.log(`the invitation mail reached ${invitation.to} (${invitation.subject})`);
    const invite = mailLink(invitation, 'invite')
      ?? ((`${invitation.text ?? ''}\n${invitation.html ?? ''}`).match(/https?:\/\/\S+\/i\/[A-Za-z0-9_-]{16}#[A-Za-z0-9_-]{43}/) ?? [])[0];
    if (!invite) fail(`the invitation mail carries no link: ${(invitation.text ?? '').slice(0, 400)}`);
    // They asked to get in: the link joins them at once, with no card to press.
    await ada.tab.navigate(invite);
  });

  await story.step('They arrive in a workspace of their own', async () => {
    const began = Date.now();
    // The invitation founds Ada's person: the entry shows her 24 recovery
    // words once, and the workspace waits for them to be kept.
    await keepRecoveryWords(ada.tab, 'Ada: founded');
    await until(ada.tab, drawn, 'the invitation letting Ada in', stepLimit(4));
    await until(ada.tab, arrived, 'Ada’s workspace arriving', stepLimit(4));
    const own = await until(ada.tab, ownWorkspace, 'the workspace of Ada’s own, drawn', stepLimit(4));
    story.web.signIn(ada, Date.now() - began);
    const firstScreen = await ada.tab.evaluate('document.documentElement.dataset.firstScreen ?? null');
    if (firstScreen !== null) fail(`the setup screen came over the workspace Ada joined (${firstScreen})`);
    console.log(`Ada arrived in ${own}`);
    // The workspace is Ada's, not the Lounge's: the Lounge is elsewhere.
    const lounge = await lin.tab.evaluate(`document.documentElement.dataset.workspace ?? null`);
    if (!lounge) fail('the host lost the Lounge when Ada joined');
    if (String(own).includes(lounge)) fail(`Ada landed in the Lounge (${own}), not a workspace of her own`);
  });
} catch (thrown) {
  if (!(thrown instanceof StoryStopped)) error = thrown;
}
// A last look at both screens, so the recording ends on what they saw.
await sleep(1500);
const result = await story.finish({error});
console.log(JSON.stringify({completed: result.completed, withinBudget: result.withinBudget, brokeAt: result.brokeAt, timings: result.timings}, null, 2));
process.exit(result.completed && result.withinBudget ? 0 : 1);
