// The agent-mcp story: Ada connects her agent to a workspace over MCP, and it
// does real work there — writes a file, adds a Tasks issue, writes a note and
// posts in Chat — every change shown under its name. When it asks her a
// question, the sheet comes up on her page and she answers it.
//
// Her agent is played by a scripted MCP client over stdio, against the real
// `tilo mcp` server the `tilo` command starts, as an agent's own client starts
// it (`integration/agent-setup.mjs` connects one the same way). The agent is
// "Claude Code", the name the tools it registered go by.
//
// usage: node agent-mcp.mjs SITE_URL OUT_DIR [SERVICE_LOG…]
// Writes OUT_DIR/story.json (stories/recorder.mjs) and exits non-zero when a
// step failed or ran over its budget.
import {sleep} from '../../../../tools/web/chrome.mjs';
import {SHELL, stepLimit} from '../integration/waits.mjs';
import {click, found, key, makeWorkspace, openWorkspace, pressNamed, searcher, showsText as shows, TREE_TEXT, waitUntil}
  from '../integration/shell.mjs';
import {computerAt} from './computer.mjs';
import {recorder, StoryStopped} from './recorder.mjs';

const [site, out, ...logs] = process.argv.slice(2);
if (!site || !out) {
  console.error('usage: node agent-mcp.mjs SITE_URL OUT_DIR [SERVICE_LOG…]');
  process.exit(2);
}
const fail = message => { throw new Error(message); };
const until = (tab, expression, what, timeout = stepLimit(3)) => waitUntil(tab, expression, what, timeout);
const {search} = searcher({until, failed: fail, dump: async () => {}});
const press = (tab, name) => pressNamed(tab, name);
const pressWhen = async (tab, name, what, timeout = stepLimit(3)) => {
  await tab.evaluate('globalThis.tiloMount?.requestSemantics?.()').catch(() => {});
  await until(tab, found(name), what, timeout);
  if (!await press(tab, name)) fail(`${what} is not where a person can press it`);
};
const chord = async tab => (await tab.evaluate('/Mac/.test(navigator.platform)')) ? 4 : 2;
const arrived = `['done', 'stopped'].includes(document.documentElement.dataset.arrival)`;

const story = recorder({id: 'agent-mcp', out, logs});
const WORKSPACE = 'Workshop';
const AGENT = 'Claude Code';
const FILE = 'agent-notes.md';
const TASK = 'Check the spring budget';
const NOTE = 'Spring plans';
const CHAT = 'I filed the budget under Spring.';
const QUESTION = 'Shall I file the budget under Spring?';
const ANSWER = 'Yes, file it';
const computer = computerAt(out);
let agent = null;
let error = null;

/** One `tilo_do` of the agent's: the first of `paths` the workspace takes. */
const act = async (paths, args = {}) => {
  let refused = null;
  for (const path of [].concat(paths)) {
    try {
      return await agent.call('tilo_do', {path, args});
    } catch (problem) {
      if (!/unknown action|no such|UnknownAction|unavailable/i.test(String(problem))) throw problem;
      refused = problem;
    }
  }
  throw refused;
};
/** An app of the person's opened on her page, and the changes it shows. */
const openApp = async (tab, keys, name, app, text, what, timeout) => {
  const already = `(globalThis.tiloMount?.navigation?.getSnapshot?.().entries ?? []).some(entry => entry.app === ${JSON.stringify(app)})`;
  if (!await tab.evaluate(already).catch(() => false)) await search(tab, keys, name.toLowerCase(), name, app);
  await until(tab, already, `${name} on Ada’s screen`, stepLimit(4));
  await shows(tab, text, what, timeout);
};

try {
  const ada = await story.person('Ada');
  const adaKeys = await chord(ada.tab);

  await story.step('Ada opens Tilo', async () => {
    const began = Date.now();
    await story.web.coldLoad(ada, site);
    await until(ada.tab, arrived, 'Ada’s page arriving', stepLimit(6));
    if (await ada.tab.evaluate('document.documentElement.dataset.arrival') !== 'done') {
      fail(`Ada’s page stopped: ${(await ada.tab.evaluate('(document.body.innerText || "").slice(0, 300)'))}`);
    }
    story.timing('Ada’s page load, to the desktop', Date.now() - began);
    story.web.scriptSize(ada);
  });

  await story.step(`Ada makes a workspace, “${WORKSPACE}”`, async () => {
    await click(ada.tab, SHELL.empty.x, SHELL.empty.y);
    await key(ada.tab, 'k', adaKeys);
    await until(ada.tab, 'globalThis.tiloMount?.state()?.searchOpen === true', 'the search opening');
    await search(ada.tab, adaKeys, 'new workspace', 'New workspace');
    await makeWorkspace(ada.tab, WORKSPACE);
    await until(ada.tab, openWorkspace(WORKSPACE), `${WORKSPACE} opening`, stepLimit(10));
  });

  await story.step('Ada connects their agent to the workspace', async () => {
    await search(ada.tab, adaKeys, 'connect an agent', 'Connect an agent or computer');
    const prompt = await until(ada.tab, `(() => {
      const offer = document.getElementById('first-screen-offer')?.textContent ?? '';
      return /login [2-9A-HJKMNP-TV-Z]{3}-[2-9A-HJKMNP-TV-Z]{4}/.test(offer) ? offer : '';
    })()`, 'the setup prompt with a login code', stepLimit(6));
    const code = prompt.match(/login ([2-9A-HJKMNP-TV-Z]{3}-[2-9A-HJKMNP-TV-Z]{4})/)[1];
    const loggedIn = await computer.run(['login', code, '--site', site, '--agent', AGENT], {timeout: stepLimit(12)});
    if (loggedIn.code !== 0) fail(`tilo login failed (${loggedIn.code}): ${loggedIn.stderr}${loggedIn.stdout}`);
    if (!/Logged in/.test(loggedIn.stdout)) fail(`tilo login did not say it logged in: ${loggedIn.stdout.slice(-600)}`);
    await until(ada.tab, `document.documentElement.dataset.firstScreen === undefined`, 'the setup screen leaving', stepLimit(6));
    // Her agent's client starts the server over stdio, as a restarted agent
    // does; the server enters the workspace as her computer's agent once it
    // can, and the workspace lists it from then on.
    agent = computer.mcp();
    await agent.initialize();
    const deadline = Date.now() + stepLimit(6);
    let listed = null;
    while (!listed) {
      try {
        await agent.call('tilo_ls');
        listed = true;
      } catch (problem) {
        if (Date.now() > deadline) throw new Error(`the agent's server never reached the workspace: ${problem}`);
        await sleep(1000);
      }
    }
    const tools = await agent.listTools();
    if (!tools.includes('tilo_do')) fail(`the MCP server offers no tilo_do: ${JSON.stringify(tools)}`);
    const present = `(() => {
      const person = (globalThis.tiloMount?.people?.() ?? []).find(row => row.agent && /${AGENT}/.test(row.label ?? ''));
      return person ? JSON.stringify(person) : '';
    })()`;
    await until(ada.tab, present, `${AGENT} in Ada’s people list`, stepLimit(6));
  });

  await story.step('The agent writes a file', async () => {
    const bytes = Buffer.from('# Notes from your agent\n\nThe spring budget is filed.\n', 'utf8');
    const stored = await agent.call('tilo_put_many', {files: [bytes.toString('base64')]});
    const content = stored.match(/tilo\.content:[0-9a-f]{64}/)?.[0];
    if (!content) fail(`the upload answered no content reference: ${stored.slice(0, 300)}`);
    await act(['files#open'], {content, name: FILE});
    await openApp(ada.tab, adaKeys, 'Files', 'so.tilo.file-explorer', FILE, `${FILE} in Files`, stepLimit(8));
  });

  await story.step('The agent adds a Tasks issue', async () => {
    await act(['tasks.issues#create', 'tasks#create'], {title: TASK});
    await openApp(ada.tab, adaKeys, 'Tasks', 'so.tilo.tasks', TASK, `the issue “${TASK}” in Tasks`, stepLimit(8));
  });

  await story.step('The agent writes a note', async () => {
    await act(['notes#new-note'], {title: NOTE});
    await act(['notes#append-to-note'], {title: NOTE, markdown: `- file the budget\n- book the trip\n`});
    await openApp(ada.tab, adaKeys, 'Notes', 'so.tilo.notes', NOTE, `the note “${NOTE}” in Notes`, stepLimit(8));
  });

  await story.step('The agent posts in Chat', async () => {
    await act(['chat.workspace#begin-chat', 'chat#begin-chat'], {});
    await act(['chat.messages#post', 'chat#post'], {channel: 'c_chat', text: CHAT});
    await openApp(ada.tab, adaKeys, 'Chat', 'so.tilo.chat', CHAT, 'the message in Chat', stepLimit(10));
  });

  await story.step('Ada sees each change under the agent’s name', async () => {
    // Activity opens on what is new since she was last here; History lists
    // every change of the workspace, each under the name of who made it.
    const open = `(globalThis.tiloMount?.navigation?.getSnapshot?.().entries ?? []).some(entry => entry.app === 'so.tilo.activity')`;
    if (!await ada.tab.evaluate(open).catch(() => false)) await search(ada.tab, adaKeys, 'activity', 'Activity', 'so.tilo.activity');
    await until(ada.tab, open, 'Activity on Ada’s screen', stepLimit(4));
    await ada.tab.evaluate('globalThis.tiloMount?.requestSemantics?.()').catch(() => {});
    // Its places are beside its title: History lists every change.
    const places = await ada.tab.waitFor(found('History'), {timeout: stepLimit(3)}).catch(() => false);
    if (places) {
      if (!await press(ada.tab, 'History')) fail('History among Activity’s places is not where a person can press it');
    } else {
      await pressWhen(ada.tab, 'See history', 'Activity’s See history');
    }
    await shows(ada.tab, FILE, 'History listing the agent’s file', stepLimit(8));
    await shows(ada.tab, TASK, `History listing ${TASK}`, stepLimit(6));
    await shows(ada.tab, NOTE, `History listing ${NOTE}`, stepLimit(6));
    // A message is the person's own words, which History names by what was
    // done rather than quoting: its row is the one about a message.
    const message = await ada.tab.waitFor(`/message/i.test(${TREE_TEXT})`, {timeout: stepLimit(6)}).catch(() => false);
    if (!message) fail('History lists no message the agent posted');
    // Each row names who wrote it: the agent, in its own name, beside the change.
    const parts = JSON.parse(await ada.tab.evaluate(`(() => {
      const out = [];
      const node = n => { for (const part of [n.name, n.value]) if (part) out.push(part); (n.children || []).forEach(node); };
      const walk = n => { if (n.semantics) node(n.semantics); (n.children || []).forEach(walk); };
      const root = globalThis.tiloMount?.state()?.semantics?.root;
      if (root) walk(root);
      return JSON.stringify(out);
    })()`).catch(() => '[]'));
    const changes = [
      {what: FILE, match: part => part.includes(FILE)},
      {what: TASK, match: part => part.includes(TASK)},
      {what: NOTE, match: part => part.includes(NOTE)},
      {what: 'the message', match: part => /message/i.test(part)},
    ];
    const unnamed = changes.filter(change => {
      const at = parts.findIndex(change.match);
      return at < 0 || !parts.slice(Math.max(0, at - 4), at + 4).some(part => part.includes(AGENT));
    });
    if (unnamed.length) {
      fail(`History does not name ${AGENT} beside ${unnamed.map(change => change.what).join(', ')}; it says: ${JSON.stringify(parts.slice(0, 120))}`);
    }
  });

  await story.step('The agent asks Ada a question, and Ada answers it', async () => {
    // The agent's call waits for her answer, as a person's agent waits. The
    // sheet's answers are buttons: the one she presses is her answer.
    const asked = agent.call('tilo_ask', {question: QUESTION, choices: [ANSWER, 'Leave it for now']});
    asked.catch(() => {});
    await shows(ada.tab, QUESTION, 'the agent’s question on Ada’s page', stepLimit(6));
    await pressWhen(ada.tab, ANSWER, 'the answer the person picks', stepLimit(4));
    const answered = await Promise.race([asked, sleep(stepLimit(6)).then(() => { throw new Error('the agent’s question was never answered'); })]);
    if (!answered.includes(ANSWER)) fail(`the agent was not told her answer: ${answered}`);
  });
} catch (thrown) {
  if (!(thrown instanceof StoryStopped)) error = thrown;
} finally {
  computer.close();
}
await sleep(1500);
const result = await story.finish({error});
console.log(JSON.stringify({completed: result.completed, withinBudget: result.withinBudget, brokeAt: result.brokeAt}, null, 2));
process.exit(result.completed && result.withinBudget ? 0 : 1);
