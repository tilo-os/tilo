// The person's hands and eyes on the web desktop, shared by the page drivers:
// keys and clicks as a person gives them, the nodes the desktop publishes for a
// screen reader (what a driver reads and presses by name, since what is drawn
// is a picture), the shell's search, invitation links, what a page said while
// it loaded, and the mails a stack's entry sends.
import {readdirSync, readFileSync} from 'node:fs';
import {join} from 'node:path';
import {sleep} from '../../../../tools/web/chrome.mjs';
import {SHELL, stepLimit} from './waits.mjs';
// Keys as the desktop's search reads them (its own scripts drive it this
// way): the chord, the query, Enter.
//
// The events of one gesture go out together, not one acknowledged frame at a
// time. Chromium answers a dispatched key or press when the page has taken it,
// and this page's main thread is inside a frame for seconds under the check: a
// driver that waited for each key in turn typed a five-letter query in eight
// frames, where a person types it in a tenth of a second and the shell reads
// the whole query on the frame it draws next.
export const event = (tab, params) => tab.send('Input.dispatchKeyEvent', params);
export const key = async (tab, name, modifiers = 0) => {
  const named = {Enter: 13, Escape: 27, ArrowDown: 40, ArrowUp: 38};
  const code = name.length === 1 ? `Key${name.toUpperCase()}` : name;
  const pressed = {key: name, code, windowsVirtualKeyCode: named[name] ?? name.toUpperCase().charCodeAt(0), modifiers};
  await Promise.all([
    event(tab, {type: 'rawKeyDown', ...pressed, ...(name === 'Enter' ? {text: '\r'} : {})}),
    event(tab, {type: 'keyUp', ...pressed}),
  ]);
};
export const type = async (tab, text) => {
  await Promise.all([...text].flatMap(ch => [
    event(tab, {type: 'keyDown', key: ch, text: ch}),
    event(tab, {type: 'keyUp', key: ch}),
  ]));
  // The shell reads the query on its next frame; the wait that follows is on
  // its own answer to it.
  await sleep(600);
};
export const took = from => ((Date.now() - from) / 1000).toFixed(1);
export const click = async (tab, x, y) => {
  // One gesture's three events, sent together: a press acknowledged one frame
  // at a time is three of this page's frames for a click a person makes in
  // milliseconds.
  await Promise.all([
    tab.send('Input.dispatchMouseEvent', {type: 'mouseMoved', x, y}),
    ...['mousePressed', 'mouseReleased'].map(type =>
      tab.send('Input.dispatchMouseEvent', {type, x, y, button: 'left', clickCount: 1})),
  ]);
  // The press is the page's to take on its next frame; what it opened is read
  // after this, and the waits themselves end on the page's changes.
  await sleep(600);
};
// The centre of the first semantic node named `name`, in page pixels.
// The `nth` control drawn under `name` (a negative index counts from the
// last): a shell's floating card and the app's own panel can offer the same
// control, and a person's press is the one they can reach.
// [`find`] as the page-side expression a wait reads: the node's JSON, or ''.
export const found = (name, nth = 0) => `(() => {
  const edges = b => Array.isArray(b)
    ? {x0: b[0], y0: b[1], x1: b[2], y1: b[3]}
    : {x0: b.x0 ?? b.x, y0: b.y0 ?? b.y, x1: b.x1 ?? b.x + b.width, y1: b.y1 ?? b.y + b.height};
  const sized = r => Array.isArray(r)
    ? {x0: r[0], y0: r[1], x1: r[0] + r[2], y1: r[1] + r[3]}
    : {x0: r.x, y0: r.y, x1: r.x + r.width, y1: r.y + r.height};
  const found = [];
  const node = n => {
    const box = n.bounds ? edges(n.bounds) : n.rect ? sized(n.rect) : null;
    found.push({name: n.name ?? '', bounds: box});
    (n.children || []).forEach(node);
  };
  const walk = n => { if (n.semantics) node(n.semantics); (n.children || []).forEach(walk); };
  const root = globalThis.tiloMount?.state()?.semantics?.root;
  if (root) walk(root);
  // A page the server renders: a control is the element that carries the name
  // a person reaches it by, and its box is what the browser laid out.
  if (!root) {
    const page = document.getElementById('tilo-page');
    for (const el of page?.querySelectorAll('[aria-label], button, a, input, textarea, [contenteditable]') ?? []) {
      const box = el.getBoundingClientRect();
      if (!box.width || !box.height) continue;
      found.push({name: el.getAttribute('aria-label') ?? (el.innerText || '').trim().split('\n')[0],
        bounds: {x0: box.x, y0: box.y, x1: box.right, y1: box.bottom}});
    }
  }
  const matches = found.filter(n => n.name === ${JSON.stringify(name)} && n.bounds);
  const nth = ${nth};
  const match = matches[nth < 0 ? matches.length + nth : nth];
  // '' while it is not there, so a wait on this reads it as not yet.
  return match ? JSON.stringify(match) : '';
})()`;
export const find = async (tab, name, nth = 0) => (await tab.evaluate(found(name, nth))) || 'null';
export const until = async (tab, expression, what, timeout = stepLimit(3)) => {
  const value = await tab.waitFor(expression, {timeout}).catch(() => false);
  if (value) return value;
  const shown = await tab.evaluate(`JSON.stringify({
    arrival: document.documentElement.dataset.arrival ?? null,
    refusal: document.documentElement.dataset.refusal ?? null,
    entry: document.documentElement.dataset.entry ?? null,
    status: document.getElementById('status')?.textContent?.trim().slice(0, 200) ?? null,
    windows: (globalThis.tiloMount?.navigation?.getSnapshot?.().entries ?? []).map(entry => entry.label),
  })`).catch(() => 'unreadable');
  throw new Error(`${what} did not happen within ${(timeout / 1000).toFixed(1)} s (the page: ${shown})`);
};
export const returning = tab => tab.send('Page.addScriptToEvaluateOnNewDocument',
  {source: "try { localStorage.setItem('tilo-setup', JSON.stringify({step: 'done'})); } catch {}"});
// What a page's loading said beside its console's errors: requests that
// failed and the browser's own log (a resource that did not load is said
// there, not in the console), and the page's console in full, so a page that
// stops while it loads leaves why.
export const recordLoading = async (launched, tab) => {
  const asked = new Map();
  launched.on(({sessionId, method, params}) => {
    if (sessionId !== tab.sessionId) return;
    if (method === 'Network.requestWillBeSent') asked.set(params.requestId, params.request.url);
    else if (method === 'Network.loadingFailed') {
      tab.log.push(`NET ${asked.get(params.requestId) ?? params.requestId}: ${params.errorText}${params.canceled ? ' (canceled)' : ''}`);
    } else if (method === 'Network.responseReceived' && params.response.status >= 400) {
      tab.log.push(`HTTP ${params.response.status} ${params.response.url}`);
    } else if (method === 'Log.entryAdded') {
      tab.log.push(`LOG ${params.entry.level} ${params.entry.text}${params.entry.url ? ` (${params.entry.url})` : ''}`.slice(0, 600));
    } else if (method === 'Runtime.consoleAPICalled' && !['error', 'warning'].includes(params.type)) {
      tab.log.push(`${params.type.toUpperCase()} ${params.args.map(a => a.value ?? a.description ?? '').join(' ')}`.slice(0, 600));
    }
  });
  await tab.send('Network.enable', {}).catch(() => {});
  await tab.send('Log.enable', {}).catch(() => {});
};
export const pageText = tab => tab.evaluate(`(document.body.innerText || '').replace(/\\s+/g, ' ').trim()`).catch(() => '');
export const searcher = ({until, failed, dump}) => {
  /// What the open search offers with nothing typed, as the groups it lists: the
  /// page's own sources list apps and windows, the session's list settings, people
  /// and files, so which stands says how much of search is mounted here.
  const emptyCard = async (tab, command) => {
    for (let tries = 0; tries < 3; tries++) {
      if (!await tab.evaluate('!!globalThis.tiloMount?.state()?.searchOpen').catch(() => false)) break;
      await key(tab, 'Escape');
      await until(tab, '!globalThis.tiloMount?.state()?.searchOpen', 'the search closing', stepLimit(2)).catch(() => {});
    }
    if (!await tab.evaluate('!!globalThis.tiloMount?.state()?.searchOpen').catch(() => false)) {
      await key(tab, 'k', command);
    }
    await tab.waitFor('globalThis.tiloMount?.state()?.searchSearching === false', {timeout: 20_000}).catch(() => false);
    const card = JSON.parse(await tab.evaluate('JSON.stringify(globalThis.tiloMount?.state()?.searchCard ?? null)').catch(() => 'null'));
    return card?.rows?.map(entry => `${entry.title} [${entry.kind}]`) ?? null;
  };
  // A person's search: the chord, the query, and Enter on the row the palette
  // offers for it.
  //
  // Every wait here is on what the shell says, never on how many frames it drew.
  // A frame of this page costs seconds under the check — a megapixel through a
  // software rasteriser, with the app opened last still compiling its module on
  // the same thread — so a wait that counted frames paid seconds for an answer
  // the shell had already given.
  //
  // The palette's card is that answer (`searchCard`): the query the shell's field
  // holds and the rows it will act on, in the order Enter walks them. The shell
  // says it still owes rows (`searchSearching`) while a source has yet to answer
  // or the rows are not cut to the card's room, and the rows it shows before that
  // answer are the previous query's. So Enter waits for the shell to have answered
  // *this* query, and for the row asked for to be the first one under the
  // palette's heading, which is where the highlight sits.
  const search = async (tab, command, query, row, opens = null, {focus = true, mayStayUp = false} = {}) => {
    const began = Date.now();
    const framesFrom = await presented(tab);
    // What the shell said of itself on every frame from here to the answer: a
    // search that opened and closed between two frames and one that never
    // opened read the same from a single reading afterwards.
    await tab.evaluate(`(() => {
      globalThis.__pressTrail = [];
      const began = performance.now();
      const tick = () => {
        const s = globalThis.tiloMount?.state?.() ?? {};
        if (globalThis.__pressTrail.length < 200) globalThis.__pressTrail.push([Math.round(performance.now() - began), s.phase ?? null,
          !!s.searchOpen, s.frames ?? null, s.desktopRevision ?? null,
          s.searchCard?.scope ?? null,
          (document.activeElement?.nodeName ?? '') + (document.hasFocus() ? '+' : '-'),
          s.searchPending ?? null]);
        if (performance.now() - began < 75000) requestAnimationFrame(tick);
      };
      tick();
      return true;
    })()`).catch(() => false);
    // A person's search is the shell's, never one window's: the chord opens the
    // shell's over a window's own (a window's field a press inside the window
    // left up) and closes only the shell's. What the open search reaches is the
    // page's own answer (`searchCard.scope`), so the flow reads that instead of
    // closing searches to make the one it wants the only one.
    const openScope = () => tab.evaluate("globalThis.tiloMount?.state()?.searchOpen ? (globalThis.tiloMount.state().searchCard?.scope ?? 'unknown') : null");
    const shellSearch = scope => scope === 'workspace' || scope === 'everywhere';
    const opened = `(() => { const s = globalThis.tiloMount?.state?.() ?? null;
      return !!s?.searchOpen && ['workspace', 'everywhere'].includes(s.searchCard?.scope); })()`;
    // A shell still joining a workspace it just opened takes no key: a person
    // pressed there would see nothing, and press again.
    await tab.waitFor(`globalThis.tiloMount?.state?.()?.phase === 'presented'`, {timeout: stepLimit(3)}).catch(() => false);
    // The shell keeps the search up until what it opened has arrived, and the
    // chord toggles the shell's own: it is pressed only when the shell's is down.
    // A chord the page took while it was mounting a window (the workspace it
    // just opened) is given again, as a person presses again what did not take.
    const openShellSearch = `(() => { const s = globalThis.tiloMount?.state?.() ?? null;
      return !!s?.searchOpen && ['workspace', 'everywhere'].includes(s.searchCard?.scope); })()`;
    let chorded = 0;
    for (let tries = 0; tries < 3; tries++) {
      const scope = await openScope();
      if (shellSearch(scope)) break;
      // Something is open but not the shell's search (a window's own): its
      // chord was given, and pressing again would close what just opened.
      if (scope !== null && tries > 0) break;
      if (focus) await click(tab, SHELL.empty.x, SHELL.empty.y);
      chorded = Date.now();
      await key(tab, 'k', command);
      if (await tab.waitFor(openShellSearch, {timeout: 3000}).catch(() => false)) break;
    }
    chorded ||= Date.now();
    // What the chord did, said before the long wait: a key the page took and the
    // shell did not answer is the shell's, not the key's, and a failure that
    // waited a minute to say "closed" never said which it was.
    if (!await tab.waitFor(openShellSearch, {timeout: 3000}).catch(() => false)) {
      const card = await tab.evaluate('JSON.stringify(globalThis.tiloMount?.state()?.searchCard ?? null)').catch(() => 'null');
      console.error(`the search chord opened nothing after ${Date.now() - chorded} ms (scope=${await openScope()}, card=${card})`);
    }
    // A wait that runs out exits the flow from inside `until`: the chord and the
    // frames after it are read into its failure line there.
    //
    // The first search on a page fetches the palette's engine, a module of its
    // own, and the page of a busy node spends the fetch and its compile on the
    // very frames this waits for: six steps are what a page's join gets, and the
    // opening measured here was 48 s of them (load 335).
    await until(tab, opened,
      `the shell's search for "${query}"`, stepLimit(6), async () => {
      const trail = await tab.evaluate('JSON.stringify((globalThis.__pressTrail ?? []).slice(0, 40))').catch(() => '[]');
      const chords = await tab.evaluate('JSON.stringify((globalThis.__chordStates ?? []).slice(-6))').catch(() => '[]');
      return `the chord opened nothing after ${Date.now() - chorded} ms: the chord as it arrived and after: ${chords}; frames after it [ms, phase, open, frames, revision, scope, focus, pending]: ${trail}`;
    });
    const scope = await openScope();
    if (scope !== 'workspace') failed(`the search for "${query}" is not the shell's (${scope})`);
    await type(tab, query);
    const typed = Date.now();
    const asked = JSON.stringify(query);
    // The card as the shell answers it for this query, once it owes no row:
    // nothing before that is its answer to the text in its field.
    const answered = `(() => {
      const s = globalThis.tiloMount?.state?.() ?? null;
      const card = s?.searchCard ?? null;
      return card && s.searchSearching === false && card.query === ${asked} ? JSON.stringify(card) : '';
    })()`;
    const card = JSON.parse(await until(tab, answered, `the shell answering "${query}"`, stepLimit(6)));
    if (card.rows[0]?.title !== row) {
      // The settled card is the shell's answer, not a frame of one still coming:
      // a palette that offers something else first is the failure.
      const rows = await paletteRows(tab);
      await dump(tab, `timeout-rows-for-${query}`);
      const offered = card.rows?.map(entry => `${entry.title} [${entry.kind}]`) ?? rows;
      // The groups an empty card lists say which sources the shell has at all:
      // the page's own list apps and windows, the session's list settings, people
      // and files. Which of them stands tells a search over no apps from one
      // whose app list is empty.
      const empty = await emptyCard(tab, command);
      failed(`the palette offers no ${row} first for "${query}" in the ${card.scope ?? '?'} search: ${JSON.stringify(offered)}; with nothing typed it offers ${JSON.stringify(empty)}`);
    }
    const chosen = card.rows;
    // Enter is what opens the app: the watch is in the page before the press, so
    // the frame it closes search and the frame the app's window arrives on are
    // told apart on the page's own clock.
    if (opens) await armOpen(tab, opens);
    await key(tab, 'Enter');
    const pressed = Date.now();
    // A shell that keeps its search up after what it opened has arrived
    // leaves the person to close it: a person presses Escape, and goes on.
    // The caller that asked for that says so in what it checks next.
    const closed = await tab.waitFor('!globalThis.tiloMount?.state()?.searchOpen', {timeout: mayStayUp ? 5000 : stepLimit(4)})
      .catch(() => false);
    if (!closed && mayStayUp) {
      await key(tab, 'Escape');
      await tab.waitFor('!globalThis.tiloMount?.state()?.searchOpen', {timeout: 5000}).catch(() => false);
    } else if (!closed) {
      await until(tab, '!globalThis.tiloMount?.state()?.searchOpen', `the search closing after ${row}`, stepLimit(4));
    }
    // Where a search's time went, in the parts a person's search has: the chord,
    // the text typed, the shell answering the query, the rows it offers, Enter,
    // and what the search waited for after it (the app's window, arriving), with
    // the frames the shell presented meanwhile.
    console.log(`TIMING search=${asked} chord=${took(began)} type=${took(chorded)}` +
      ` answer=${took(typed)} rows=${card.rows.length} open=${took(pressed)}` +
      ` frames=${await presented(tab) - framesFrom}`);
    return chosen;
  };
  return {search, emptyCard};
};

export const shows = (tab, text, what, timeout) => tab.evaluate('globalThis.tiloMount?.requestSemantics?.()').catch(() => {})
  .then(() => waitUntil(tab, `(${TREE_TEXT}).includes(${JSON.stringify(text)})`, what, timeout));

export const TREE_TEXT = `(() => {
  const out = [];
  const node = n => { for (const part of [n.name, n.value]) if (part) out.push(part); (n.children || []).forEach(node); };
  const walk = n => { if (n.semantics) node(n.semantics); (n.children || []).forEach(walk); };
  const root = globalThis.tiloMount?.state()?.semantics?.root;
  if (root) walk(root);
  // A page the server renders has no semantic tree: what it shows are its own
  // elements, their words and the names they answer to.
  if (!root) {
    const page = document.getElementById('tilo-page');
    if (page) {
      out.push(...(page.innerText || '').split(/\n+/).filter(Boolean));
      for (const field of page.querySelectorAll('input,textarea')) if (field.value) out.push(field.value);
      for (const named of page.querySelectorAll('[aria-label]')) out.push(named.getAttribute('aria-label'));
    }
  }
  return out.join('\\n');
})()`;

export const waitForMail = async (dir, what, count, timeout = stepLimit(6)) => {
  const deadline = Date.now() + timeout;
  for (;;) {
    const kept = mailsOf(dir);
    if (kept.length >= count) return kept[count - 1];
    if (Date.now() > deadline) {
      throw new Error(`${what} never reached the mail sink (${kept.length} mails: ${JSON.stringify(kept.map(mail => mail.subject))})`);
    }
    await sleep(250);
  }
};
/** The link a mail carries whose path ends in `marker`'s variable, e.g. the
 * confirmation's `wait-list-confirm=<secret>` or an invitation's `invite=<key>`. */
export const mailLink = (mail, marker) => {
  const text = `${mail.text ?? ''}\n${mail.html ?? ''}`;
  return text.match(new RegExp(`https?://[^\\s"'<>]*${marker}[=:][0-9a-zA-Z-]+`))?.[0] ?? null;
};
/** A person leaves `address` on the entry's wait list, as the page's own form
 * takes it. The driver waits on what the page says next. */
export const leaveWaitListAddress = (tab, address) => tab.evaluate(`(() => {
  const field = document.getElementById('arrival-wait-email');
  if (!field) return false;
  field.value = ${JSON.stringify(address)};
  field.dispatchEvent(new Event('input', {bubbles: true}));
  const button = document.querySelector('#arrival-wait button');
  if (button) button.click();
  return !!button;
})()`);
/** A person presses the button of the page's own form whose text is `text`, in
 * the card `within` (a page's card, not the shell's canvas). */
export const pressText = (tab, text, within = 'body') => tab.evaluate(`(() => {
  const button = [...document.querySelectorAll(${JSON.stringify(within)} + ' button')]
    .find(candidate => candidate.textContent.trim() === ${JSON.stringify(text)} && !candidate.disabled);
  if (!button) return false;
  button.click();
  return true;
})()`);
/** Wait until the page answers `expression` something other than an empty
 * string, or say '' when it never did: for reading what a card says next, or
 * what went wrong, without the wait failing the run. */
export const poll = (tab, expression, timeout = stepLimit(4)) =>
  tab.waitFor(expression, {timeout}).catch(() => '');
/** The Lounge's waiting card, as a person reads it: the addresses waiting to be
 * let in and what the card offers, once it has answered with rows, with
 * nobody on it, or with what went wrong; null while it has said none of them. */
export const waitingCard = async (tab, timeout = stepLimit(6)) => {
  const shown = await poll(tab, `(() => {
    const card = document.querySelector('.tilo-waiting');
    if (!card) return '';
    const error = card.querySelector('.tilo-entry-error');
    const rows = [...card.querySelectorAll('.tilo-waiting-contact')].map(node => node.textContent.trim());
    const empty = card.querySelector('.tilo-waiting-none');
    if (!rows.length && !empty && !(error && !error.hidden)) return '';
    return JSON.stringify({contacts: rows,
      said: error && !error.hidden ? error.textContent.trim() : null,
      buttons: [...card.querySelectorAll('li button')].map(node => node.textContent.trim())});
  })()`, timeout);
  return shown ? JSON.parse(shown) : null;
};
