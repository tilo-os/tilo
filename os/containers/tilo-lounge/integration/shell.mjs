// The person's hands and eyes on the web desktop, shared by the page drivers:
// keys and clicks as a person gives them, the nodes the desktop publishes for a
// screen reader (what a driver reads and presses by name, since what is drawn
// is a picture), the shell's search, invitation links, and what a page said
// while it loaded.
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
// The names the command palette shows, in order, as the desktop publishes
// them: its query field, its result rows and its actions.
export const paletteRows = async tab => {
  await tab.evaluate('globalThis.tiloMount?.requestSemantics?.()');
  // As `snapshot`: the tree arrives in a status of its own after the frame.
  await sleep(500);
  return JSON.parse(await tab.evaluate(`(() => {
    let rows = null;
    const collect = node => {
      rows = [];
      const walk = semantics => { rows.push(semantics.name ?? ''); (semantics.children || []).forEach(walk); };
      if (node.semantics) walk(node.semantics);
    };
    const find = node => { if (node.widget === 'ActionPalette') collect(node); (node.children || []).forEach(find); };
    const root = globalThis.tiloMount?.state()?.semantics?.root;
    if (root) find(root);
    return JSON.stringify(rows);
  })()`));
};
// Seconds from `from` to now, and the frames the shell has presented: what the
// timing lines below print.
export const took = from => ((Date.now() - from) / 1000).toFixed(1);
export const presented = tab => tab.evaluate('globalThis.tiloMount?.state()?.frames ?? 0').catch(() => 0);
// A canvas-drawn desktop takes the pointer; the search chord is Ctrl+K away
// from the keyboard (Meta on a Mac, as the desktop reads it).
export const canvasTopLeft = tab => tab.evaluate(`(() => {
  const rect = globalThis.tiloMount?.canvas?.getBoundingClientRect();
  return JSON.stringify(rect ? {x: rect.left, y: rect.top, width: rect.width, height: rect.height} : null);
})()`);
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
  const found = [];
  const node = n => { found.push({name: n.name ?? '', bounds: n.bounds ?? n.rect ?? null}); (n.children || []).forEach(node); };
  const walk = n => { if (n.semantics) node(n.semantics); (n.children || []).forEach(walk); };
  const root = globalThis.tiloMount?.state()?.semantics?.root;
  if (root) walk(root);
  const matches = found.filter(n => n.name === ${JSON.stringify(name)} && n.bounds);
  const nth = ${nth};
  const match = matches[nth < 0 ? matches.length + nth : nth];
  // '' while it is not there, so a wait on this reads it as not yet.
  return match ? JSON.stringify(match) : '';
})()`;
export const find = async (tab, name, nth = 0) => (await tab.evaluate(found(name, nth))) || 'null';
// A published control's `bounds` as its four edges, whichever shape they came in.
export const boxOf = bounds => Array.isArray(bounds) ? bounds
  : [bounds.x0 ?? bounds.x, bounds.y0 ?? bounds.y,
     bounds.x1 ?? bounds.x + bounds.width, bounds.y1 ?? bounds.y + bounds.height];
export const pressNamed = async (tab, name, nth = 0, presses = null) => {
  const node = JSON.parse(await find(tab, name, nth));
  if (!node) return false;
  const [x0, y0, x1, y1] = Array.isArray(node.bounds)
    ? node.bounds
    : [node.bounds.x0 ?? node.bounds.x, node.bounds.y0 ?? node.bounds.y,
       node.bounds.x1 ?? node.bounds.x + node.bounds.width, node.bounds.y1 ?? node.bounds.y + node.bounds.height];
  const corner = JSON.parse(await canvasTopLeft(tab));
  const x = corner.x + (x0 + x1) / 2;
  const y = corner.y + (y0 + y1) / 2;
  // Only a control on screen can be pressed: one laid out where a card has
  // scrolled it away is no place a person can reach, and a press beside the
  // page lands nowhere. The caller scrolls it in first, as a person does.
  const inside = x >= corner.x && x <= corner.x + corner.width
    && y >= corner.y && y <= corner.y + corner.height;
  // What stands at the point the press is for: a control drawn by the shell
  // is under page chrome only when something of the page covers it, and then
  // the press is that element's.
  const under = await tab.evaluate(`(() => {
    const element = document.elementFromPoint(${x}, ${y});
    if (!element) return null;
    return JSON.stringify({tag: element.tagName, className: String(element.className),
      semantic: element.dataset?.tiloSemantic ?? null});
  })()`);
  presses?.push({name, bounds: node.bounds, at: {x, y}, inside, under});
  if (!inside) return false;
  await click(tab, x, y);
  return true;
};
// A page-side clock around the person's Enter. What a person waits for is the
// shell showing the app's window, and this shell paints into one canvas: the
// window is on screen when the frame that carries it is presented. So both
// moments are frames the page presented — the press, stamped where the key
// arrives, and the first presented frame after it that lists the window —
// kept on the page's own clock, where a round trip from here never lands in
// the number.
//
// The press is stamped by a `keydown` listener, not by reading the shell's
// state: a MutationObserver runs only after the frame the key caused has
// already published, so a watch that took its press from the first status
// showing search closed dated the press at or after the frame it was meant to
// precede, and every open read as 0 ms.
//
// The frame that counts is the first presented one after the press whose
// navigation lists the app's window: this shell draws a window in the pass that
// publishes it, so that frame carries the window's pixels, where the entry
// alone can be published a frame before its pixels are shown.
export const armOpen = (tab, app) => tab.evaluate(`(() => {
  const mount = globalThis.tiloMount;
  const host = mount.canvas.parentElement;
  const watch = globalThis.__tiloOpen = {app: ${JSON.stringify(app)}, pressedAt: null,
    framesAtPress: null, shownAt: null, framesShown: null, framesAtKey: null};
  if (!globalThis.__tiloOpenKeys) {
    globalThis.__tiloOpenKeys = true;
    addEventListener('keydown', event => {
      const w = globalThis.__tiloOpen;
      if (!w || w.pressedAt !== null) return;
      if (event.key !== 'Enter' && event.key !== '\\r') return;
      w.pressedAt = performance.now();
      w.framesAtPress = Number(mount.canvas.parentElement.dataset.frames ?? 0);
    }, true);
  }
  const read = () => {
    const w = globalThis.__tiloOpen;
    if (!w || w.pressedAt === null || w.shownAt !== null) return;
    if (host.dataset.phase !== 'presented') return;
    if (Number(host.dataset.frames ?? 0) <= w.framesAtPress) return;
    if ((mount.navigation?.getSnapshot?.().entries ?? []).some(entry => entry.app === w.app)) {
      w.shownAt = performance.now();
      w.framesShown = Number(host.dataset.frames ?? 0);
      // The frames the shell had presented when it took the key, as it says
      // itself: the frame after that one is the first that can show the
      // window, whatever the page's counter read when the browser fired it.
      w.framesAtKey = mount.state()?.framesAtKey ?? null;
    }
  };
  new MutationObserver(read).observe(host, {attributes: true, attributeFilter: ['data-frames', 'data-phase']});
  // The window's entry can be published after the shell's last frame, with no
  // frame after it: read again as the navigation changes too, and once more
  // as the marks are taken. Shown is then never earlier than both a frame
  // after the key and the entry, so an open is never measured faster than it was.
  mount.navigation?.subscribe?.(read);
  watch.read = read;
  return true;
})()`);
// What the open cost, in the shell's own unit: the window is drawn on the frame
// right after the one that took the key, so the open is worth no more frames
// than that. The milliseconds ride along as what this machine's rasteriser
// took to put that frame on screen — a megapixel through software costs about
// half a second here and near nothing on a display, and neither says anything
// about whether the shell heard the person.
//
// What was not measured is `ok: false`, never a number: an open the watch
// never saw begun or never saw end is no reading at all, and the caller fails
// on it rather than counting `0` as fast.
export const openMarks = tab => tab.evaluate(`(() => {
  const watch = globalThis.__tiloOpen;
  watch?.read?.();
  globalThis.__tiloOpen = undefined;
  if (!watch || watch.pressedAt === null || watch.shownAt === null || watch.framesAtKey === null) {
    // What the watch saw, so an unmeasured open says which condition held it back.
    const host = globalThis.tiloMount?.canvas?.parentElement;
    return JSON.stringify({ok: false, pressed: watch?.pressedAt != null, shown: watch?.shownAt != null,
      phase: host?.dataset.phase ?? null, frames: Number(host?.dataset.frames ?? NaN), framesAtPress: watch?.framesAtPress ?? null,
      framesAtKey: watch?.framesAtKey ?? null,
      entry: (globalThis.tiloMount?.navigation?.getSnapshot?.().entries ?? []).some(entry => entry.app === watch?.app),
      subscribes: typeof globalThis.tiloMount?.navigation?.subscribe === 'function'});
  }
  return JSON.stringify({ok: true,
    presentMs: Math.round(watch.shownAt - watch.pressedAt),
    frames: watch.framesShown - watch.framesAtKey});
})()`).then(JSON.parse);
// People who come back to the site: a browser new to it opens on the first
// screen (set up with an agent, or skip to one's own desktop), which these
// people have been past already, so their page opens the Lounge itself.
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
// The invitations a site hands over: `<scheme>://<site>/i/<handle>#<key>`, the
// handle and the key alone, the key never in the part a site's logs see.
export const sealedLink = /^https?:\/\/[^/]+\/i\/[A-Za-z0-9_-]{16}#[A-Za-z0-9_-]{43}$/;
// The page's words over the page, as a person at a link reads them.
export const pageText = tab => tab.evaluate(`(document.body.innerText || '').replace(/\\s+/g, ' ').trim()`).catch(() => '');
// Every value the desktop's semantic tree carries, as the sheet's link is one.
export const NODE_VALUES = `(() => {
  const out = [];
  const node = n => { if (n.value) out.push(n.value); (n.children || []).forEach(node); };
  const walk = n => { if (n.semantics) node(n.semantics); (n.children || []).forEach(walk); };
  const root = globalThis.tiloMount?.state()?.semantics?.root;
  if (root) walk(root);
  return JSON.stringify(out);
})()`;
export const nodeValues = tab => tab.evaluate(NODE_VALUES).then(JSON.parse);

/** A person's search, waiting the way the calling driver waits: `until` is its
 * wait that fails the run with what the page showed, `failed` its failure and
 * `dump` what it keeps of a page, so a search that goes wrong is said in the
 * driver's own words. */
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
  const search = async (tab, command, query, row, opens = null) => {
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
    // The shell keeps the search up until what it opened has arrived, and the
    // chord toggles the shell's own: it is pressed only when the shell's is down.
    if (!shellSearch(await openScope())) {
      await click(tab, SHELL.empty.x, SHELL.empty.y);
      await key(tab, 'k', command);
    }
    const chorded = Date.now();
    // What the chord did, said before the long wait: a key the page took and the
    // shell did not answer is the shell's, not the key's, and a failure that
    // waited a minute to say "closed" never said which it was.
    const opened = await tab.waitFor(`(() => { const s = globalThis.tiloMount?.state?.() ?? null;
      return !!s?.searchOpen && ['workspace', 'everywhere'].includes(s.searchCard?.scope); })()`, {timeout: 3000})
      .catch(() => false);
    if (!opened) {
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
    await until(tab, `(() => { const s = globalThis.tiloMount?.state?.() ?? null;
      return !!s?.searchOpen && ['workspace', 'everywhere'].includes(s.searchCard?.scope); })()`,
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
    await until(tab, '!globalThis.tiloMount?.state()?.searchOpen', `the search closing after ${row}`, stepLimit(4));
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

/** Every name and value in the page's semantic tree, as one text: what a
 * person reads on the desktop, whichever app drew it. */
export const TREE_TEXT = `(() => {
  const out = [];
  const node = n => { for (const part of [n.name, n.value]) if (part) out.push(part); (n.children || []).forEach(node); };
  const walk = n => { if (n.semantics) node(n.semantics); (n.children || []).forEach(walk); };
  const root = globalThis.tiloMount?.state()?.semantics?.root;
  if (root) walk(root);
  return out.join('\\n');
})()`;
