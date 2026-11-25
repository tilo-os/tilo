// Two people of one workspace in a voice call, each in a headless Chromium of
// their own with a synthetic microphone and camera: each arrives at the
// Lounge's web entry as a person, names themselves through the workspace's own
// search, and one makes a voice channel there. Both open Chat, open that
// channel, join its call, unmute and turn their camera on through the call
// view's buttons. What is proved is the call itself, read from the call's own
// counters on each page (`globalThis.tiloCallStats()`, the browser's
// `web/voice.mjs` stats: the frames it heard and sent, and the pictures it
// received), and from the other person's tile drawing picture after picture.
// Nothing here looks at a screenshot.
//
// usage: node page-call.mjs SITE_URL SERVER_ORIGIN WORKSPACE OUT_DIR
//
// Prints a JSON report (also written to OUT_DIR/report.json) and exits non-zero
// when a step a person would notice did not happen, or when a call that both
// people joined carried nothing.
import {mkdirSync, writeFileSync} from 'node:fs';
import {join} from 'node:path';
import {launch, pinnedCertificates, servedCertificate, sleep, stepLimit} from '../../../../tools/web/chrome.mjs';
import {click, find, until} from './shell.mjs';
import {confirmRecoveryWords, machine} from './waits.mjs';

const [site, server, workspace, out] = process.argv.slice(2);
if (!site || !server || !workspace || !out) {
  console.error('usage: node page-call.mjs SITE_URL SERVER_ORIGIN WORKSPACE OUT_DIR');
  process.exit(2);
}
mkdirSync(out, {recursive: true});

const report = {site, server, workspace, steps: [], calls: {}, problems: [], exceptions: []};
const problems = report.problems;
const step = (name, data) => { report.steps.push({name, ...data}); console.log(name, JSON.stringify(data)); };

const siteCertificate = await servedCertificate(site);
const serverCertificate = await servedCertificate(server);
// A browser that captures and plays media: a synthetic microphone and camera,
// the media prompts answered, and audio that starts without a gesture. Each
// person gets one of their own, so what one hears and sees crossed the
// workspace, not a tab's own pipes.
const flags = [pinnedCertificates([siteCertificate, serverCertificate])];
const browsers = [];
const tabs = [];
const openBrowser = async () => {
  const browser = await launch({media: true, args: flags});
  browsers.push(browser);
  return browser;
};

/** Wait for `expression` in `tab`, or fail with the page's own state. */
const waitFor = (tab, expression, what, timeout = stepLimit(8)) => until(tab, expression, what, timeout);

/** Keys as a person types them into the page's field. */
const typeText = async (tab, text) => {
  for (const ch of text) {
    await tab.send('Input.dispatchKeyEvent', {type: 'keyDown', key: ch, text: ch, unmodifiedText: ch});
    await tab.send('Input.dispatchKeyEvent', {type: 'keyUp', key: ch});
    await sleep(10);
  }
};

/** The browser arrives at the site and is handed the workspace's page, as the
 * entry does for a person who has not been here: the card's Join is pressed,
 * the 24 recovery words are kept, and the page shows the workspace. */
const arrive = async (browser, who) => {
  const tab = await browser.tab();
  tabs.push(tab);
  await tab.send('Network.enable', {}).catch(() => {});
  // A desktop's window: the page shows its sidebar and its content beside each
  // other, as it does for a person at a computer.
  await tab.send('Emulation.setDeviceMetricsOverride',
    {width: 1280, height: 800, deviceScaleFactor: 1, mobile: false}).catch(() => {});
  let state = 'waiting';
  await tab.navigate(site);
  for (let i = 0; i < 960; i++) {
    // The person is founded on the way in: their 24 words are shown once and hold
    // the page until kept, so the card is confirmed as it comes.
    await confirmRecoveryWords(tab);
    state = await tab.evaluate(`(() => {
      if (location.pathname.startsWith('/w/')) return 'handed';
      const arrival = document.documentElement.dataset.arrival;
      if (arrival === 'stopped' || arrival === 'done') return arrival;
      const join = [...document.querySelectorAll('button')].find(b => /^Join\\b/i.test(b.textContent.trim()));
      if (join && !join.disabled && !join.dataset.pressed) { join.dataset.pressed = '1'; join.click(); return 'pressed'; }
      return 'waiting';
    })()`).catch(() => 'waiting');
    if (state === 'handed' || state === 'stopped' || state === 'done') break;
    await sleep(250);
  }
  const path = await tab.evaluate('location.pathname').catch(() => '');
  step(`${who}: arrival`, {state, path});
  if (state !== 'handed') throw new Error(`${who}'s browser was not handed to the workspace's page (it is at ${path})`);
  await waitFor(tab, `document.querySelector('#tilo-page [data-p]')`, `${who}'s page showing the workspace`, stepLimit(12));
  const landed = path.split('/')[2] ?? '';
  if (landed !== workspace) throw new Error(`${who} landed in workspace ${landed}, not the Lounge's ${workspace}`);
  return tab;
};

/** The page's own search (`/w/WORKSPACE/search`), typed into; resolves once the
 * row `label` is drawn, without pressing it. */
const search = async (tab, who, query, label) => {
  await tab.evaluate(`location.assign('/w/${workspace}/search')`);
  await waitFor(tab, `!!document.querySelector('#tilo-page .tilo-search-field')`, `${who}'s search page`, stepLimit(12));
  await waitFor(tab, `document.activeElement === document.querySelector('#tilo-page .tilo-search-field')`,
    `${who}'s search field taking the caret`, stepLimit(6));
  // A search box cleared as a person clears it: an earlier query does not
  // prefix this one.
  await tab.evaluate(`(() => {
    const field = document.querySelector('#tilo-page .tilo-search-field');
    field.value = '';
    field.dispatchEvent(new Event('input', {bubbles: true}));
  })()`);
  await typeText(tab, query);
  await waitFor(tab, `[...document.querySelectorAll('#tilo-page .tilo-search-row')]
    .some(row => ((row.getAttribute('aria-label') ?? row.innerText ?? '')).includes(${JSON.stringify(label)}))`,
  `${who}'s search offering "${label}" for "${query}"`);
};

/** Press the search row `label` as a person presses it, by the pointer. */
const pressRow = async (tab, who, label) => {
  const box = JSON.parse(await waitFor(tab, `(() => {
    const row = [...document.querySelectorAll('#tilo-page .tilo-search-row')]
      .find(candidate => ((candidate.getAttribute('aria-label') ?? candidate.innerText ?? '')).includes(${JSON.stringify(label)}));
    if (!row) return '';
    row.scrollIntoView({block: 'nearest'});
    const rect = row.getBoundingClientRect();
    return rect.width && rect.height ? JSON.stringify({x: rect.x + Math.min(rect.width / 2, 40), y: rect.y + rect.height / 2}) : '';
  })()`, `${who}'s row "${label}" drawn`));
  await click(tab, box.x, box.y);
  step(`${who}: pressed ${label}`, {});
};

/** The name a person goes by here, through the workspace's own action: the
 * search offers "Set your name", its form takes the name, and its own Run
 * button runs it. */
const nameSelf = async (tab, who, name) => {
  await search(tab, who, 'set your name', 'Set your name');
  await pressRow(tab, who, 'Set your name');
  await waitFor(tab, `!!document.querySelector('#tilo-page .tilo-search-form input[name=name]')`, `${who}'s name form`);
  // The field comes with the caret in it; a form patched in without one is put
  // there, as a person pressing it does.
  const field = `document.querySelector('#tilo-page .tilo-search-form input[name=name]')`;
  try {
    await waitFor(tab, `document.activeElement === ${field}`, `${who}'s name field taking the caret`, stepLimit(3));
  } catch {
    await tab.evaluate(`${field}.focus()`);
    await waitFor(tab, `document.activeElement === ${field}`, `${who}'s name field taking the caret`);
  }
  await typeText(tab, name);
  await waitFor(tab, `document.querySelector('#tilo-page .tilo-search-form input[name=name]').value === ${JSON.stringify(name)}`,
    `${who}'s name typed`);
  await tab.evaluate(`document.querySelector('#tilo-page .tilo-search-form button[type=submit]').click()`);
  await waitFor(tab, `!document.querySelector('#tilo-page .tilo-search-form')`, `${who}'s name kept`);
  // The action's own answer: a name the workspace refused is said here, not
  // discovered from an unnamed tile later.
  const said = await waitFor(tab, `document.querySelector('#tilo-page .tilo-search-notice')?.textContent?.trim() ?? ''`,
    `${who}'s name answered`);
  if (!said.startsWith('Set your name: Done.')) throw new Error(`${who}'s name was not kept: ${said}`);
  step(`${who}: named`, {name, said});
};

/** The voice channel a person makes here, through the workspace's own action:
 * the search offers "New voice channel", whose name its default fills, so
 * pressing the row runs it. A form is answered if this build's action asks one. */
const makeVoiceChannel = async (tab, who, name) => {
  try {
    await search(tab, who, 'new voice channel', 'New voice channel');
  } catch {
    // The app's actions are offered where its package is known: a person who
    // found nothing here opens the app and looks again.
    await tab.evaluate(`location.assign('/w/${workspace}/chat')`);
    await waitFor(tab, `!!document.querySelector('#tilo-page [data-p]')`, `${who}'s Chat page`, stepLimit(12));
    await search(tab, who, 'new voice channel', 'New voice channel');
  }
  await pressRow(tab, who, 'New voice channel');
  const asked = await waitFor(tab, `(() => {
    if (document.querySelector('#tilo-page .tilo-search-form')) return 'form';
    return document.querySelector('#tilo-page .tilo-search-notice') ? 'ran' : '';
  })()`, `${who}'s channel action answering`, stepLimit(6));
  if (asked === 'form') {
    await tab.evaluate(`(() => {
      const field = document.querySelector('#tilo-page .tilo-search-form input');
      field.value = ${JSON.stringify(name)};
      field.dispatchEvent(new Event('input', {bubbles: true}));
      document.querySelector('#tilo-page .tilo-search-form button[type=submit]').click();
    })()`);
  }
  const notice = await waitFor(tab, `document.querySelector('#tilo-page .tilo-search-notice')?.textContent?.trim() ?? ''`,
    `${who}'s channel made`, stepLimit(8));
  step(`${who}: made the voice channel`, {notice});
  return notice;
};

/** The Chat app's page, with the voice channel's row drawn in its sidebar. */
const openChat = async (tab, who, channel) => {
  await tab.evaluate(`location.assign('/w/${workspace}/chat')`);
  await waitFor(tab, `!!document.querySelector('#tilo-page [data-p]')`, `${who}'s Chat page`, stepLimit(12));
  await waitFor(tab, `(() => {
    for (const node of document.querySelectorAll('#tilo-page [aria-label], button, a, [role=treeitem]')) {
      const box = node.getBoundingClientRect();
      if (!box.width || !box.height) continue;
      if ((node.getAttribute('aria-label') ?? (node.innerText || '').trim().split('\\n')[0]) === ${JSON.stringify(channel)}) return true;
    }
    return false;
  })()`, `${who}'s Chat listing "${channel}"`);
};

/** Press a control the page names, by the pointer, as a person does. */
const press = async (tab, who, name) => {
  const at = await find(tab, name);
  if (!at || at === 'null') throw new Error(`${who}'s page offers no "${name}" to press`);
  const {bounds} = JSON.parse(at);
  await click(tab, (bounds.x0 + bounds.x1) / 2, (bounds.y0 + bounds.y1) / 2);
  step(`${who}: pressed ${name}`, {});
};

/** Press the sidebar row `name` (the nav list's own row, never a heading that
 * happens to say the same) as a person presses it. */
const pressNav = async (tab, who, name) => {
  const box = JSON.parse(await waitFor(tab, `(() => {
    const rows = [...document.querySelectorAll('#tilo-page [role=treeitem]')]
      .filter(row => (row.getAttribute('aria-label') ?? (row.innerText || '').trim().split('\\n')[0]) === ${JSON.stringify(name)});
    const row = rows[0];
    if (!row) return '';
    row.scrollIntoView({block: 'nearest'});
    const rect = row.getBoundingClientRect();
    return rect.width && rect.height ? JSON.stringify({x: rect.x + Math.min(rect.width / 2, 40), y: rect.y + rect.height / 2}) : '';
  })()`, `${who}'s sidebar row "${name}" drawn`));
  await click(tab, box.x, box.y);
  step(`${who}: pressed ${name} in the sidebar`, {});
};

/** The call's own counters, as the page answers them. */
const callStats = async tab => {
  const said = await tab.evaluate(`JSON.stringify(globalThis.tiloCallStats?.() ?? null)`).catch(() => 'null');
  return JSON.parse(said ?? 'null');
};

/** One page as it stands: its call's counters, and every picture it draws, by
 * the person its tile names. A call's picture is the canvas its tile decodes the
 * call's own stream into; another widget's is the still the page paints it as. A
 * picture is read by a digest of what it shows, so a tile that redraws shows as
 * a different one without the bytes leaving the page. */
const look = async tab => JSON.parse(await tab.evaluate(`(() => {
  const digest = text => {
    let hash = 2166136261;
    for (let i = 0; i < text.length; i++) { hash ^= text.charCodeAt(i); hash = Math.imul(hash, 16777619); }
    return (hash >>> 0).toString(16) + ':' + text.length;
  };
  const pixels = canvas => {
    try {
      const data = canvas.getContext('2d').getImageData(0, 0, canvas.width, canvas.height).data;
      let hash = 2166136261;
      for (let i = 0; i < data.length; i += 97) { hash ^= data[i]; hash = Math.imul(hash, 16777619); }
      return (hash >>> 0).toString(16) + ':' + data.length;
    } catch {
      return '';
    }
  };
  const tiles = {};
  for (const img of document.querySelectorAll('#tilo-page img.tilo-paint')) {
    const named = img.closest('[aria-label]')?.getAttribute('aria-label') ?? '';
    (tiles[named] ??= []).push(digest(img.getAttribute('src') ?? ''));
  }
  for (const tile of document.querySelectorAll('#tilo-page [data-live]')) {
    const canvas = tile.querySelector('canvas.tilo-call-picture');
    if (!canvas || !canvas.width || !tile.hasAttribute('data-picture')) continue;
    (tiles[tile.getAttribute('aria-label') ?? ''] ??= []).push(pixels(canvas));
  }
  return JSON.stringify({stats: globalThis.tiloCallStats?.() ?? null, tiles});
})()`));

/** The tiles whose image of the person `name` has drawn more than one picture.
 * A tile's name says what it shows (`"Ada, on camera"`), so it is matched by
 * who it names. */
const redrawn = (samples, name) => {
  const seen = new Map();
  for (const sample of samples) {
    for (const [label, pictures] of Object.entries(sample.tiles)) {
      if (!label.includes(name)) continue;
      const known = seen.get(label) ?? new Set();
      for (const picture of pictures) known.add(picture);
      seen.set(label, known);
    }
  }
  return Math.max(0, ...[...seen.values()].map(pictures => pictures.size));
};

const reportProblems = () => {
  if (!problems.length) return;
  console.error('--- the machine as it failed:', machine().text);
  for (const problem of problems) console.error('PROBLEM:', problem);
};

const ADA = 'Ada';
const BO = 'Bo';
const CHANNEL = 'Voice';

try {
  const adaBrowser = await openBrowser();
  const ada = await arrive(adaBrowser, ADA);
  await nameSelf(ada, ADA, ADA);

  const boBrowser = await openBrowser();
  const bo = await arrive(boBrowser, BO);
  await nameSelf(bo, BO, BO);

  // Both browsers are in the one workspace the Lounge kept, and the page says
  // more than one person is here. What the home page calls them is its
  // presence's word (a device label), not the name a person chose, so it is
  // kept, never judged.
  await ada.evaluate(`location.assign('/w/${workspace}')`);
  await waitFor(ada, `[...document.querySelectorAll('#tilo-page .tilo-person')].length >= 2`,
    'both people in the workspace', stepLimit(12));
  const people = await ada.evaluate(`[...document.querySelectorAll('#tilo-page .tilo-person')].map(row => ({
    name: row.querySelector('.tilo-person-name')?.textContent?.trim() ?? '',
    kind: row.querySelector('.tilo-person-kind')?.textContent?.trim() ?? '',
  }))`);
  step('the workspace says who is here', {people});
  if (people.length < 2) problems.push(`the workspace says ${JSON.stringify(people)} are here`);

  // No voice channel was seeded: one person makes it through the workspace's
  // own search, as any member makes one.
  await makeVoiceChannel(ada, ADA, CHANNEL);

  // Both open Chat and open that channel: its page is the call view.
  await openChat(ada, ADA, CHANNEL);
  await openChat(bo, BO, CHANNEL);
  for (const [tab, who] of [[ada, ADA], [bo, BO]]) {
    await pressNav(tab, who, CHANNEL);
    await waitFor(tab, `(() => [...document.querySelectorAll('#tilo-page button')]
      .some(b => (b.getAttribute('aria-label') ?? b.innerText ?? '').trim() === 'Join'))()`,
    `${who}'s call view offering Join`, stepLimit(8));
  }
  step('both opened the call', {});

  // Each joins, unmutes and turns their camera on, through the call's buttons.
  for (const [tab, who] of [[ada, ADA], [bo, BO]]) {
    await press(tab, who, 'Join');
    await waitFor(tab, `globalThis.tiloCallStats?.()?.phase === 'live'`, `${who} in the call`, stepLimit(12));
    await press(tab, who, 'Unmute');
    await waitFor(tab, `globalThis.tiloCallStats?.()?.muted === false`, `${who} unmuted`, stepLimit(12));
    await waitFor(tab, `globalThis.tiloCallStats?.()?.captureActive === true`, `${who}'s microphone running`, stepLimit(12));
    await press(tab, who, 'Turn camera on');
    await waitFor(tab, `[...document.querySelectorAll('#tilo-page button')]
      .some(b => (b.getAttribute('aria-label') ?? '').trim() === 'Turn camera off')`, `${who}'s camera on`, stepLimit(8));
  }
  step('both joined, unmuted and turned their camera on', {
    ada: await callStats(ada),
    bo: await callStats(bo),
  });

  // Each page draws the other person's tile: a canvas the page decodes the
  // call's own stream into, marked once a frame is in it. The tile is named by
  // the workspace's member list, which the name above is in.
  const tileOf = name => `[...document.querySelectorAll('#tilo-page [data-live]')]
    .some(tile => tile.hasAttribute('data-picture') && (tile.getAttribute('aria-label') ?? '').includes(${JSON.stringify(name)}))`;
  const tileLabels = async tab => tab.evaluate(`[...document.querySelectorAll('#tilo-page [data-live]')]
    .map(tile => tile.getAttribute('aria-label') ?? '(unnamed)')`);
  try {
    await waitFor(ada, tileOf(BO), `${ADA}'s page drawing ${BO} on camera`, stepLimit(12));
    await waitFor(bo, tileOf(ADA), `${BO}'s page drawing ${ADA} on camera`, stepLimit(12));
  } catch (error) {
    step('the tiles as they were drawn', {ada: await tileLabels(ada), bo: await tileLabels(bo)});
    throw error;
  }
  step('each page draws the other person', {ada: await tileLabels(ada), bo: await tileLabels(bo)});

  // The call over a few seconds: each page plays the other's voice, sends
  // pictures of its own camera and receives the other's, and the other's tile
  // is drawn again and again.
  const began = {ada: await look(ada), bo: await look(bo)};
  const samples = {ada: [], bo: []};
  for (let i = 0; i < 24; i++) {
    samples.ada.push(await look(ada));
    samples.bo.push(await look(bo));
    await sleep(250);
  }
  const ended = {ada: await look(ada), bo: await look(bo)};
  const rises = {};
  for (const key of ['ada', 'bo']) {
    const who = key === 'ada' ? ADA : BO;
    const other = key === 'ada' ? BO : ADA;
    const before = began[key].stats?.counters ?? {};
    const after = ended[key].stats?.counters ?? {};
    const rise = {
      heard: (after.heard ?? 0) - (before.heard ?? 0),
      sent: (after.sent ?? 0) - (before.sent ?? 0),
      pictures: (after.pictures ?? 0) - (before.pictures ?? 0),
      frames: (after.frames ?? 0) - (before.frames ?? 0),
      drawn: redrawn(samples[key], other),
    };
    rises[who] = rise;
    step(`${who}: the call over ${(samples[key].length * 250) / 1000} s`, {
      ...rise,
      phase: ended[key].stats?.phase,
      captureActive: ended[key].stats?.captureActive,
      micIssue: ended[key].stats?.micIssue,
      counters: after,
    });
    // What a call that is carried shows: the other person's voice arriving, this
    // person's pictures leaving, the other's arriving, and their tile redrawn.
    if (ended[key].stats?.phase !== 'live') problems.push(`${who}'s call is ${ended[key].stats?.phase}, not live, at the end`);
    if (ended[key].stats?.captureActive !== true) problems.push(`${who}'s microphone is not running by the end (${ended[key].stats?.micIssue ?? 'no reason said'})`);
    if ((after.heard ?? 0) - (before.heard ?? 0) < 4) problems.push(`${who} heard ${rise.heard} frames of ${other}'s voice in the call`);
    if ((after.sent ?? 0) - (before.sent ?? 0) < 2) problems.push(`${who} sent ${rise.sent} pictures of their camera in the call`);
    if ((after.pictures ?? 0) - (before.pictures ?? 0) < 2) problems.push(`${who} received ${rise.pictures} pictures of ${other} in the call`);
    if (rise.drawn < 2) problems.push(`${who}'s page drew ${other}'s tile with ${rise.drawn} distinct picture(s)`);
  }
  report.calls = rises;
  report.window = {samples: samples.ada.length, everyMs: 250};
} catch (error) {
  report.error = String(error?.stack ?? error).slice(0, 6000);
  problems.push(String(error?.message ?? error).slice(0, 2000));
} finally {
  // Every exception the pages threw, and what they reported as failures.
  report.exceptions = tabs.flatMap(tab => tab.log.filter(line => /^EXC|error/i.test(line)).slice(-10)).slice(-40);
  report.problems = problems;
  writeFileSync(join(out, 'report.json'), `${JSON.stringify(report, null, 2)}\n`);
  console.log('report', JSON.stringify({calls: report.calls, problems, error: report.error ?? null}));
  reportProblems();
  // Every browser, its supervisor and its profile: closed however the walk ended.
  await Promise.race([
    Promise.all(browsers.map(browser => browser.close().catch(() => {}))),
    sleep(10_000),
  ]);
  process.exit(problems.length ? 1 : 0);
}
