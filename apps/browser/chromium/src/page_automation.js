// Page reading and operation for browser actions. The browser process
// evaluates this in an isolated world of one frame, the main frame unless the
// caller named a frame from the outline: it shares that document's DOM, but
// the page's own scripts can neither see nor call it. Refs are
// `d<document>e<element>`; the document number is the world's, so a ref never
// resolves in a later document, and a ref only ever resolves in the frame it
// was read from.
globalThis.__tiloPage ||= (() => {
  const execNativeHistory = Document.prototype.execCommand;
  const queryNativeHistory = Document.prototype.queryCommandEnabled;
  const nativeHistory = () => ({
    can_undo: Reflect.apply(queryNativeHistory, document, ['undo']),
    can_redo: Reflect.apply(queryNativeHistory, document, ['redo'])
  });
  const MAX_LINES = 1200, MAX_CHARS = 100000, MAX_VISITED = 40000;
  // What bounds a read is the elements it looks at, each a style to ask
  // for. A text node costs its characters, which the read's size bounds, and
  // so little besides that a page may hold ten times as many: the spaces a
  // page's mark-up leaves between its elements are text nodes too, and
  // counted like elements they ended the read of a long article early.
  const MAX_VISITED_TEXT = 400000;
  const MAX_TEXT = 200, MAX_NAME = 120, MAX_VALUE = 200, MAX_OPTIONS = 30;
  // A paragraph is read whole: a page's own words are what a reader came
  // for, and a paragraph cut after a sentence or two cannot be read at all.
  // The whole page a read may walk, of which each read carries a part.
  const MAX_PROSE = 2000, MAX_ALL_LINES = 20000, MAX_ALL_CHARS = 2000000;
  // How a line break stands in text being gathered, and what two or more
  // of them with nothing but space between are.
  const LINE_BREAK = '\u0000', PARAGRAPH_BREAK = /\u0000(?:\s*\u0000)+/;
  // In code being gathered the same mark stands where a box of its own
  // starts or ends: one line break there, unless the code has its own.
  const BOX_BREAK = /\n?\u0000+\n?/g;
  const INPUT_ROLES = {button: 'button', submit: 'button', reset: 'button', image: 'button',
    file: 'file', color: 'color', checkbox: 'checkbox', radio: 'radio', range: 'slider',
    number: 'spinbutton', search: 'searchbox'};
  const INTERACTIVE = new Set(['link', 'button', 'checkbox', 'radio', 'switch', 'textbox',
    'searchbox', 'combobox', 'listbox', 'option', 'slider', 'spinbutton', 'tab', 'menuitem',
    'menuitemcheckbox', 'menuitemradio', 'treeitem', 'clickable', 'file', 'color', 'hint',
    'draggable', 'canvas', 'hoverable']);
  const CONTAINERS = new Set(['navigation', 'main', 'banner', 'contentinfo', 'complementary',
    'form', 'dialog', 'alertdialog', 'region', 'search', 'list', 'listitem', 'table', 'row',
    'cell', 'columnheader', 'rowheader', 'menu', 'menubar', 'tablist', 'tabpanel', 'tree',
    'grid', 'group', 'radiogroup', 'toolbar', 'alert', 'status', 'article', 'figure']);
  const KEEP_EMPTY = new Set(['dialog', 'alertdialog', 'alert', 'status']);
  const COLLAPSE = new Set(['list', 'listitem', 'row', 'cell', 'columnheader', 'rowheader', 'group']);
  const NAMED_FROM_CONTENT = new Set(['link', 'button', 'heading', 'option', 'tab', 'menuitem',
    'menuitemcheckbox', 'menuitemradio', 'treeitem', 'checkbox', 'radio', 'switch', 'cell',
    'columnheader', 'rowheader', 'clickable', 'draggable', 'canvas', 'hoverable']);
  const SKIP = new Set(['script', 'style', 'noscript', 'template', 'head', 'meta', 'link',
    'title', 'base', 'svg', 'object', 'embed']);
  const NOT_TEXT = new Set(['button', 'submit', 'reset', 'image', 'file', 'checkbox', 'radio',
    'hidden']);
  const SET_VALUE = new Set(['date', 'time', 'datetime-local', 'month', 'week', 'range']);
  // A colour a person picks is `#rrggbb`; the shorthand `#rgb` names the same
  // colour, and a colour input holds whichever it was given as `#rrggbb`.
  const HEX_COLOR = /^#(?:[0-9a-f]{3}|[0-9a-f]{6})$/i;
  // Colours named in words are read too, as the colour they name: an agent
  // that says "navy" means the input's own `#000080`.
  const NAMED_COLORS = {black: '#000000', silver: '#c0c0c0', gray: '#808080', grey: '#808080',
    white: '#ffffff', maroon: '#800000', red: '#ff0000', purple: '#800080', fuchsia: '#ff00ff',
    magenta: '#ff00ff', green: '#008000', lime: '#00ff00', olive: '#808000', yellow: '#ffff00',
    navy: '#000080', blue: '#0000ff', teal: '#008080', aqua: '#00ffff', cyan: '#00ffff',
    orange: '#ffa500', pink: '#ffc0cb', brown: '#a52a2a', gold: '#ffd700'};

  const ids = new WeakMap(), refs = new Map();
  let next = 0, doc = 0;
  // The outlines each actor in this tab last saw, per frame id ('' for the
  // main document), for `changes`. Several agents may read one tab, and each
  // keeps its own: one's read never takes what changed away from another. A
  // caller that names no actor is the one unnamed actor.
  const MAX_BASELINES = 64;
  const lastLinesByFrame = new Map();
  let actorId = '';
  const baselineKey = () => `${actorId}\n${frameId()}`;
  const getLastLines = () => lastLinesByFrame.get(baselineKey()) ?? null;
  const setLastLines = lines => {
    const key = baselineKey();
    lastLinesByFrame.delete(key);
    lastLinesByFrame.set(key, lines);
    if (lastLinesByFrame.size > MAX_BASELINES) lastLinesByFrame.delete(lastLinesByFrame.keys().next().value);
  };
  const MAX_DIFF_CELLS = 4000000, MAX_SETTLE_POLL_MS = 50;
  // Revealed lines one hover reports, and moves between a drag's two ends.
  const MAX_REVEAL_LINES = 40, MAX_DRAG_STEPS = 64;
  // The outline a gesture started from, so its answer says what it changed.
  let gestureBefore = null;
  // Where the engine's pointer came to rest, so styles that depend on a
  // real hover (not just a class) are applied where it now is.
  let pointerAt = null;
  // The markers applied for the last rest, so the page's own class names are
  // put back exactly as they were once the pointer leaves them.
  let marked = [];
  // The kind of gesture the engine is running, so an input it drives itself is
  // known to be one whose own event may never come.
  let gestureKind = null;
  // Whether the current gesture's page opens something on `:hover`, so the
  // pointer's rest has to be mirrored through the page's own style sheets.
  let gestureMirror = false;
  const MAX_POINTER_STYLES = 4000;
  // What a turn of the wheel turned: the box it turns over, and where in it
  // the wheel turns. The closest scroll container of the element the caller
  // named — a list, a table's own body — moves before the document does, as it
  // does under a hand resting there.
  let scrolling = null;
  const MAX_SCROLL_TURN = 2000;
  // How many turns in a row the wheel may give nothing before the page is
  // taken to have stopped moving for it.
  // The furthest one turn of the wheel carries a box, as the engine turns
  // it; how many looks a turn the box has not shown is waited for before it
  // is taken as never made; and how many turns in a row may go unshown
  // before the box is taken as stopped.
  const MAX_WHEEL_TURN = 2000, SCROLL_OWED_LOOKS = 3, MAX_SCROLL_UNSHOWN = 1;
  // The least a turn has to carry a box for that to be taken as how far one
  // turn of this wheel carries it.
  const MIN_WHEEL_REACH = 16;
  // How far the wheel is sent toward an end of the page: past any page.
  const WHEEL_TO_THE_END = 1e6;
  // The box an element's own overflow scrolls, found up its tree; the page's
  // own scrolling box when it is the page.
  function scrollBox(element) {
    const root = document.scrollingElement || document.documentElement;
    for (let node = element; node && node.nodeType === 1; node = node.parentElement || node.getRootNode().host) {
      const style = getComputedStyle(node);
      if (/(auto|scroll|overlay)/.test(style.overflowY) && node.scrollHeight > node.clientHeight + 1) return node;
    }
    return root;
  }
  const scrollable = element => Math.max(element.scrollHeight - element.clientHeight, 0);
  const scrollRoot = () => document.scrollingElement || document.documentElement;
  // How much further a box can go in the direction the wheel turns it.
  // How far down a box is scrolled. The page's own box needs no special case:
  // the window holds the document's scrolling element and the document, and
  // asking either of them for what it holds never moves the page.
  const scrollYOffset = box => box.scrollTop;
  // How far the last scroll moved the box the wheel turned over, taken by the
  // read that answers it: null when no scroll is being answered.
  let scrollMoved = null;
  const takeScrollMoved = () => { const moved = scrollMoved; scrollMoved = null; return moved; };
  // Where that scroll left the box, and how far the box goes: a caller that
  // scrolls by distances knows where it is without a picture of the page.
  let scrollAt = null;
  const takeScrollAt = () => { const at = scrollAt; scrollAt = null; return at; };
  // Whether the page moved the box out of view while the wheel turned it.
  let scrollLost = false;
  const placeOf = box => {
    const at = Math.round(scrollYOffset(box));
    return {at, of: Math.max(Math.round(scrollable(box)), at)};
  };
  const FRAME_SETTLE_MS = 250, FRAME_SETTLE_STILL = 2, FRAME_MS = 16;
  // The wheel turns over a page that has laid itself out, as a person's does:
  // a sheet that arrives late, and the reflow it causes, are waited for. The
  // wait ends as soon as the page's own size holds still. It is measured on a
  // timer, not an animation frame: a window behind another draws no frames,
  // and the wait must still end for a page nobody is looking at.
  const scrollFrames = () => new Promise(done => {
    const start = performance.now();
    let size = '', still = 0;
    const tick = () => {
      const now = performance.now(), box = document.scrollingElement || document.documentElement;
      const held = `${box.scrollHeight}x${innerWidth}x${innerHeight}`;
      still = held !== size ? 0 : still + 1; size = held;
      if (still >= FRAME_SETTLE_STILL || now - start >= FRAME_SETTLE_MS) { done(); return; }
      setTimeout(tick, FRAME_MS);
    };
    setTimeout(tick, FRAME_MS);
  });
  // Where the wheel turns, and how far the box under it still has to go:
  // the whole distance asked for, or everything the box still holds.
  async function scrollTarget(command) {
    scrollMoved = null;
    await scrollFrames();
    // Bringing an element into view turns the wheel over its nearest
    // scrollable box, at a point inside it that is on screen now.
    if (typeof command.into_view === 'string') {
      const element = resolve(command.into_view);
      const box = scrollBox(element);
      const visible = element.getBoundingClientRect(), area = box.getBoundingClientRect();
      const top = area.top + Math.max(0, Math.min(visible.top - area.top, area.height - 1));
      const point = {x: area.left + Math.max(0, Math.min(area.width / 2, area.width - 1)), y: top};
      if (!(point.x >= 0 && point.y >= 0 && point.x < innerWidth && point.y < innerHeight))
        throw failure('not_visible', `${command.into_view} is off the page; scroll the page first`);
      // The row is centred in its box when there is room for that, and the
      // wheel turns toward it from where the box is scrolled to now.
      const delta = Math.round(visible.top - area.top - (area.height - visible.height) / 2);
      return beginScroll(box, point, scrollYOffset(box) + delta, () => clip(box.getBoundingClientRect()));
    }
    // The box the caller named, or the page itself; a point inside it that
    // the wheel can turn over is its visible centre.
    const element = typeof command.element === 'string' ? resolve(command.element) : null;
    const box = element ? scrollBox(element) : (document.scrollingElement || document.documentElement);
    // The box is remembered by what it is, not by what the last turn did to
    // it: scrolling it makes it the document more records for good.
    const root = !box || box === document.scrollingElement || box === document.documentElement;
    const area = root ? {left: 0, top: 0, width: innerWidth, height: innerHeight} : box.getBoundingClientRect();
    const point = element ? (clip(element.getBoundingClientRect()) || clip(area)) : {x: innerWidth / 2, y: innerHeight / 2};
    if (!point) throw failure('not_visible', `${command.element} has no box on the page`);
    const scroller = root ? scrollRoot() : box;
    // Where the box is asked to go: an end of it, or a distance from where
    // it stands now.
    const goal = command.to === 'bottom' ? Infinity : command.to === 'top' ? 0
      : typeof command.by === 'number' ? scrollYOffset(scroller) + command.by : scrollYOffset(scroller);
    return beginScroll(scroller, point, goal,
      root ? null : () => (element && clip(element.getBoundingClientRect())) || clip(box.getBoundingClientRect()));
  }
  // A scroll is the wheel turned until the box stands where it was asked to
  // go, or as near as it can go: the end of what it holds now, which grows
  // for a list that renders more as it scrolls. What is left is always
  // measured from where the box stands, so a turn the page shows late, or
  // one the wheel made twice, is put right by the next turn and never
  // carries the box past where it was asked to go for good.
  // A turn goes out as wheel events of at most one page of the box each
  // (see `ScrollTurn` in page_automation.h): `page` is how tall the box is.
  const pageOf = box => Math.max(40, Math.floor(box === scrollRoot() ? innerHeight : box.clientHeight) || 40);
  const scrollAnswer = (left, wait) => wait
    ? {ok: true, x: scrolling.x, y: scrolling.y, left, page: pageOf(scrolling.box), wait: true}
    : {ok: true, x: scrolling.x, y: scrolling.y, left, page: pageOf(scrolling.box)};
  // The next turn toward where the box was asked to go; none when it stands
  // there. The turn is owed by the box until the box has shown it.
  function scrollTurn() {
    // The wheel turns over the box where the page has it now: a page that
    // grew above it while the wheel turned (a section that loaded late) has
    // moved it, and a wheel left where the box was would turn the page
    // instead. A box the page has moved out of view is turned no further.
    if (scrolling.where) {
      const at = scrolling.where();
      if (!at) { scrollLost = true; scrolling.owed = 0; return scrollAnswer(0, false); }
      scrolling.x = at.x; scrolling.y = at.y;
    }
    const box = scrolling.box, held = scrollable(box), now = scrollYOffset(box);
    const goal = Math.max(0, Math.min(scrolling.goal, held));
    let left = Math.round(goal - now);
    // A box asked to go further than it holds is turned at its end all the
    // same: a page that loads more once its end is reached is given the
    // turn that makes it, and the looks after it for what it loads to
    // arrive. A turn that brings nothing is one never shown.
    if (!left && scrolling.goal > held && now >= held - 1) left = Math.round(Math.min(scrolling.goal, WHEEL_TO_THE_END) - now);
    // A wheel that carries this box only so far a turn is turned that far
    // at a time, so each turn is shown in full (see `scrollLeft`).
    const most = scrolling.reach || MAX_WHEEL_TURN;
    scrolling.owed = Math.max(-most, Math.min(most, left));
    scrolling.turned = 0;
    return scrollAnswer(scrolling.owed, false);
  }
  function beginScroll(box, point, goal, where) {
    scrolling = {box, x: point.x, y: point.y, goal, start: scrollYOffset(box), still: 0, owed: 0, unshown: 0,
      turned: 0, reach: 0, where};
    scrollMoved = 0;
    scrollLost = false;
    scrollAt = placeOf(box);
    return scrollTurn();
  }
  // What the box has left to go after a turn. A turn the box has not shown
  // in full is waited for while the box moves, and a few looks more when it
  // does not: the page shows a turn a frame or a few after the wheel made
  // it, and turning again meanwhile would make it twice. A turn never shown
  // was given nothing by the page (a frame still settling, a box whose
  // content arrives later) and is made once more; a second one never shown
  // says the box has stopped. A turn that carried the box part of the way
  // and no further, with room left to go, is neither: it is as far as one
  // turn of this wheel carries this box. The box has not stopped, and the
  // turns after it are that long.
  function scrollLeft() {
    if (!scrolling) return {ok: true, x: innerWidth / 2, y: innerHeight / 2, left: 0};
    const box = scrolling.box, now = scrollYOffset(box), gone = now - scrolling.start;
    scrollMoved = (scrollMoved || 0) + Math.round(gone);
    scrollAt = placeOf(box);
    scrolling.start = now;
    if (scrolling.owed) {
      const owed = scrolling.owed - gone;
      scrolling.turned += gone;
      scrolling.owed = Math.abs(owed) < 1 || owed * scrolling.owed < 0 ? 0 : owed;
      if (!scrolling.owed) { scrolling.still = 0; scrolling.unshown = 0; }
      else {
        scrolling.still = gone ? 0 : scrolling.still + 1;
        if (scrolling.still <= SCROLL_OWED_LOOKS)
          return scrollAnswer(Math.round(scrolling.owed) || Math.sign(scrolling.owed), true);
        const carried = Math.abs(scrolling.turned);
        const room = scrolling.owed > 0 ? scrollable(box) - now : now;
        scrolling.owed = 0;
        scrolling.still = 0;
        if (carried >= MIN_WHEEL_REACH && room > 1) { scrolling.reach = carried; scrolling.unshown = 0; }
        else if (++scrolling.unshown > MAX_SCROLL_UNSHOWN) return scrollAnswer(0, false);
      }
    }
    return scrollTurn();
  }
  // The class the page's own sheets never use: a resting pointer puts it on
  // the elements it is over, and a hover-only rule reads `:hover` as it.
  const MARKER = '__tilo-hover__';
  const COMPOUND = new RegExp('[\\s>+~]');

  // `:hover` depends on the browser's own pointer position, which the
  // isolated world cannot move. While a gesture rests or drags the pointer,
  // each rule that applies only on hover is applied at the element the
  // pointer is over and along its ancestors, and removed again once the
  // pointer moves elsewhere, so a menu opens and closes as it would for a
  // person. Nothing is applied for a page without hover rules or for a
  // pointer that is not resting anywhere. The marker class the outline looks
  // for follows the same chain.
  function restPointer(x, y, tour) {
    if (x === null) { hoverStyles([]); mark(null); pointerAt = null; return null; }
    let hit = topmost(x, y);
    const chain = [];
    for (; hit && chain.length < 10000; hit = hit.parentElement || hit.getRootNode()?.host) chain.push(hit);
    pointerAt = {x, y, chain};
    if (tour) hoverStyles(chain);
    mark(chain);
    return pointerAt;
  }
  // The marker class the <style> sheets already contain, on the elements the
  // pointer now rests over: a hover-only rule lists it as its trigger.
  function mark(chain) {
    for (const [element, added] of marked)
      if (added) element.classList.remove(MARKER);
      else element.classList.add(MARKER);
    marked = [];
    for (const element of chain || []) {
      if (!element.classList) continue;
      // A page that already uses the marker keeps its own class.
      marked.push([element, !element.classList.contains(MARKER)]);
      element.classList.add(MARKER);
    }
  }
  function hoverStyles(chain) {
    const wanted = new Set(chain);
    let seen = 0;
    for (const sheet of document.styleSheets) {
      let rules = null;
      try { rules = sheet.cssRules; } catch (error) { rules = null; }
      if (!rules) continue;
      for (const rule of rules) {
        if (++seen > MAX_POINTER_STYLES) return;
        const selector = rule.selectorText;
        if (!selector || !selector.includes(':hover')) continue;
        const trigger = hoverTrigger(selector);
        if (!trigger) continue;
        const plain = selector.replace(/:hover(?![\w-])/g, '');
        for (const element of wanted) {
          let matched = false;
          try { matched = element.matches(trigger); } catch (error) { matched = false; }
          if (!matched) continue;
          try { applyHoverRule(element, plain); } catch (error) { /* a hostile sheet */ }
        }
      }
    }
  }
  // The rule's declarations, taken from a rule the sheet already parsed for
  // the element without its `:hover`, inlined into a per-element <style>.
  function applyHoverRule(element, plain) {
    let style = element.__tiloHoverStyle;
    if (!style || !style.isConnected) {
      style = document.createElement('style');
      style.setAttribute('data-tilo-pointer', '');
      element.__tiloHoverStyle = style;
      element.parentNode?.insertBefore(style, element);
    }
    const declarations = [];
    for (const sheet of document.styleSheets) {
      let rules = null;
      try { rules = sheet.cssRules; } catch (error) { rules = null; }
      if (!rules) continue;
      for (const rule of rules) {
        if (rule.selectorText && rule.selectorText.includes(':hover') &&
            rule.selectorText.replace(/:hover(?![\w-])/g, '') === plain)
          declarations.push(`${plain}{${rule.style.cssText}}`);
      }
    }
    style.textContent = declarations.join('');
  }

  const clean = (text, limit) => {
    const value = String(text ?? '').replace(/\s+/g, ' ').trim();
    return value.length > limit ? value.slice(0, limit - 1) + '…' : value;
  };
  const quote = value => JSON.stringify(value);
  const failure = (code, message) => ({ok: false, code, message});

  function refOf(element) {
    let id = ids.get(element);
    if (id === undefined) {
      id = ++next;
      ids.set(element, id);
      refs.set(id, new WeakRef(element));
    }
    return `d${doc}e${id}`;
  }
  function resolve(ref) {
    const match = /^d(\d+)e(\d+)$/.exec(ref || '');
    if (!match) throw failure('invalid_request', `${ref} is not an element ref`);
    if (Number(match[1]) !== doc)
      throw failure('stale_element', `${ref} belongs to a page this tab has left; read the page again`);
    const element = refs.get(Number(match[2]))?.deref();
    if (!element || !element.isConnected)
      throw failure('stale_element', `${ref} is no longer on the page; read the page again`);
    return element;
  }
  function prune() {
    if (refs.size < 20000) return;
    for (const [id, ref] of refs) if (!ref.deref()?.isConnected) refs.delete(id);
  }

  // A frame is named by the element that holds it. The page prints that
  // element's ref between two marks, and the browser process, which keeps
  // the tab's frame numbers, writes the frame's number in its place. The ref
  // is this document's own, so a frame keeps its name while the frames
  // around it come and go and while its own document moves on, and a name
  // never passes to another frame.
  const FRAME_MARK = '\u0001';
  const frameMark = element => `${FRAME_MARK}${refOf(element)}${FRAME_MARK}`;
  // Whether a frame shows enough of itself to be read with the page around
  // it: a frame nobody sees (a tracker's, an ad slot's before it is filled)
  // is listed and not read.
  function frameShown(element) {
    if (element.checkVisibility && !element.checkVisibility({checkOpacity: true, checkVisibilityCSS: true})) return false;
    const box = element.getBoundingClientRect();
    return box.width >= 40 && box.height >= 20;
  }

  const type = element => (element.getAttribute('type') || 'text').toLowerCase();
  function role(element) {
    const explicit = (element.getAttribute('role') || '').trim().split(/\s+/)[0].toLowerCase();
    if (explicit && explicit !== 'presentation' && explicit !== 'none' && explicit !== 'generic')
      return explicit;
    const tag = element.localName;
    switch (tag) {
      case 'a': case 'area': return element.hasAttribute('href') ? 'link' : '';
      case 'button': case 'summary': return 'button';
      case 'input': return type(element) === 'hidden' ? '' : INPUT_ROLES[type(element)] || 'textbox';
      case 'textarea': return 'textbox';
      case 'select': return element.multiple || element.size > 1 ? 'listbox' : 'combobox';
      case 'h1': case 'h2': case 'h3': case 'h4': case 'h5': case 'h6': return 'heading';
      case 'img': return element.getAttribute('alt') ? 'img' : '';
      case 'nav': return 'navigation';
      case 'main': case 'form': case 'dialog': case 'search': case 'article': case 'figure': return tag;
      case 'aside': return 'complementary';
      case 'header': return element.closest('article,aside,main,nav,section') ? '' : 'banner';
      case 'footer': return element.closest('article,aside,main,nav,section') ? '' : 'contentinfo';
      case 'section':
        return element.hasAttribute('aria-label') || element.hasAttribute('aria-labelledby') ? 'region' : '';
      case 'ul': case 'ol': case 'menu': return 'list';
      case 'li': return 'listitem';
      case 'table': return 'table';
      case 'tr': return 'row';
      case 'th': return 'columnheader';
      case 'td': return 'cell';
      case 'fieldset': return 'group';
      case 'iframe': case 'frame': return 'iframe';
      case 'option': return 'option';
      case 'canvas': return 'canvas';
    }
    if (element.isContentEditable && !element.parentElement?.isContentEditable) return 'textbox';
    return '';
  }
  function labelledBy(element) {
    const idrefs = element.getAttribute('aria-labelledby');
    if (!idrefs) return '';
    const root = element.getRootNode();
    return idrefs.split(/\s+/)
      .map(id => (root.getElementById ? root.getElementById(id) : null) || document.getElementById(id))
      .filter(Boolean).map(label => label.innerText || label.textContent).join(' ');
  }
  // A label's own words, without the text of the control it wraps.
  function labelText(label) {
    let text = '';
    for (const node of label.childNodes) {
      if (node.nodeType === 3) text += node.data;
      else if (node.nodeType === 1 && !['input', 'select', 'textarea', 'button'].includes(node.localName))
        text += ' ' + (node.innerText ?? node.textContent);
    }
    return text;
  }
  function name(element, kind, options) {
    let text = clean(labelledBy(element), MAX_NAME) || clean(element.getAttribute('aria-label'), MAX_NAME);
    if (text) return text;
    const tag = element.localName;
    if (tag === 'input' || tag === 'textarea' || tag === 'select') {
      const input = type(element);
      if (tag === 'input' && (input === 'button' || input === 'submit' || input === 'reset'))
        return clean(element.value || (input === 'submit' ? 'Submit' : input === 'reset' ? 'Reset' : ''), MAX_NAME);
      if (tag === 'input' && input === 'image') return clean(element.alt || element.value || 'Submit', MAX_NAME);
      text = clean([...(element.labels || [])].map(labelText).join(' '), MAX_NAME);
      return text || clean(element.getAttribute('placeholder') || element.title, MAX_NAME);
    }
    if (tag === 'img' || tag === 'area') return clean(element.getAttribute('alt') || element.title, MAX_NAME);
    if (tag === 'fieldset') {
      const legend = element.querySelector(':scope > legend');
      if (legend) return clean(legend.innerText, MAX_NAME);
    }
    if (tag === 'table' && element.caption) return clean(element.caption.innerText, MAX_NAME);
    // An element named only to be found again by its own words.
    if (options && options.naming) text = clean(element.innerText ?? element.textContent, MAX_NAME) || clean(element.title, MAX_NAME);
    if (text) return text;
    // An element with no name of its own takes the words it shows: the
    // trigger a hover opens a menu with, a draggable row, a drawing surface.
    if (NAMED_FROM_CONTENT.has(kind) || kind === 'hint' || kind === 'draggable' || kind === 'canvas') {
      text = clean(element.innerText, MAX_NAME) || clean([...element.querySelectorAll('img[alt],[aria-label],svg title')]
        .map(part => part.getAttribute('alt') || part.getAttribute('aria-label') || part.textContent).join(' '), MAX_NAME) ||
        clean(shownText(element), MAX_NAME);
      if (text) return text;
    }
    return clean(element.title, MAX_NAME);
  }
  // A field that is secret by its nature: a password field, one the page
  // says holds a card's number or security code, a password or a one-time
  // code, and one the page draws as dots. A password field stays one: a
  // page that shows a password on request turns its field into plain text,
  // and what it holds is no less a secret for that. What the page says of
  // a field by its autocomplete or its style holds for as long as the page
  // says it. What a secret field holds is never read out, recorded or told,
  // whatever wrote it.
  const SECRET_TOKENS = /(^|\s)(cc-[a-z-]+|one-time-code|current-password|new-password)(\s|$)/i;
  const wasPassword = new WeakSet();
  function secret(element) {
    if (!element || element.nodeType !== 1) return false;
    if (wasPassword.has(element)) return true;
    const tag = element.localName;
    if (tag === 'input' && type(element) === 'password') { wasPassword.add(element); return true; }
    if (SECRET_TOKENS.test(element.getAttribute('autocomplete') || '')) return true;
    if (tag !== 'input' && tag !== 'textarea') return false;
    const drawn = getComputedStyle(element).webkitTextSecurity;
    return !!drawn && drawn !== 'none';
  }
  function checked(element) {
    const aria = element.getAttribute('aria-checked');
    if (aria === 'mixed' || element.indeterminate) return 'mixed';
    return (element.localName === 'input' ? element.checked : aria === 'true') ? 'checked' : 'unchecked';
  }
  function details(element, kind) {
    const out = [], tag = element.localName;
    if (kind === 'heading') out.push(`level=${/^h[1-6]$/.test(tag) ? tag[1] : element.getAttribute('aria-level') || 2}`);
    // A colour input's value is the colour it shows, not text it holds, so it
    // is stated outright and filled by name like the other value controls.
    if (kind === 'color' && tag === 'input') {
      out.push('type=color');
      out.push(`value=${quote(element.value || '#000000')}`);
    }
    if (kind === 'textbox' || kind === 'searchbox' || kind === 'spinbutton' || kind === 'slider') {
      if (tag === 'input' || tag === 'textarea') {
        const input = tag === 'textarea' ? 'textarea' : type(element);
        if (input !== 'text' && input !== 'search' && input !== 'textarea') out.push(`type=${input}`);
        if (element.value) out.push(secret(element) ? `value=<${element.value.length} characters hidden>`
          : `value=${quote(clean(element.value, MAX_VALUE))}`);
      } else if (element.isContentEditable) {
        const value = clean(element.innerText, MAX_VALUE);
        if (value) out.push(`value=${quote(value)}`);
      }
    }
    if (tag === 'select' && kind === 'combobox' && element.selectedOptions[0])
      out.push(`value=${quote(clean(element.selectedOptions[0].label, MAX_VALUE))}`);
    if (kind === 'file') {
      // A file input's own rule is what an upload has to satisfy, so the
      // outline states it and what it holds now.
      out.push('type=file');
      if (element.multiple) out.push('multiple');
      const accept = clean(element.getAttribute('accept'), MAX_VALUE);
      if (accept) out.push(`accept=${quote(accept)}`);
      const chosen = [...(element.files || [])].map(file => clean(file.name, MAX_NAME));
      out.push(chosen.length ? `files=${quote(chosen.join(', '))}` : 'files=none');
    }
    if (kind === 'checkbox' || kind === 'radio' || kind === 'switch' || kind === 'menuitemcheckbox') out.push(checked(element));
    if (element.getAttribute('aria-pressed') === 'true') out.push('pressed');
    const expanded = element.getAttribute('aria-expanded');
    if (expanded === 'true') out.push('expanded'); else if (expanded === 'false') out.push('collapsed');
    if (element.getAttribute('aria-selected') === 'true' || (tag === 'option' && element.selected)) out.push('selected');
    if (element.disabled || element.getAttribute('aria-disabled') === 'true') out.push('disabled');
    if (element.required || element.getAttribute('aria-required') === 'true') out.push('required');
    if (element.readOnly && (tag === 'input' || tag === 'textarea')) out.push('readonly');
    if (element.getAttribute('aria-invalid') === 'true') out.push('invalid');
    if (kind === 'link') {
      const href = typeof element.href === 'string' ? element.href : element.getAttribute('href') || '';
      if (/^https?:/i.test(href)) out.push(`-> ${clean(href, 300)}`);
    }
    return out;
  }
  function summary(element, options) {
    // An element's own role names it; a bare element is named by what it
    // does with the words inside it, which is all an agent needs to act.
    const own = role(element);
    const kind = own || (element.getAttribute('draggable') === 'true' ? 'draggable'
      : element.localName === 'canvas' ? 'canvas' : element.localName);
    const label = name(element, kind, options);
    return label ? `${kind} ${quote(label)}` : kind;
  }

  // Elements a `:hover` rule in the page's own sheets targets are the
  // triggers of hover-only menus; the outline lists them with a ref so a
  // hover can name what its author cannot see. Bounded against a hostile
  // sheet, and rebuilt per outline so a changed page is read as it is.
  const MAX_HOVER_RULES = 4000, MAX_HOVER_TARGETS = 2000;
  let hoverTargets = null;
  // The compound a person hovers to open what a rule reveals: the part of the
  // selector that carries `:hover`, with it read as the marker class the
  // pointer adds. A rule names what it opens after that compound
  // (`.menu:hover ul`), so the trigger is not the rule's last compound.
  function hoverTrigger(selector) {
    if (typeof selector !== 'string') return '';
    for (const part of selector.replace(/:hover(?![\w-])/g, '.' + MARKER).split(COMPOUND))
      if (part.includes(MARKER)) return part;
    return '';
  }
  const HIDDEN = {display: /^(?:none)$/i, visibility: /^(?:hidden|collapse)$/i,
    'content-visibility': /^(?:hidden)$/i, opacity: /^0(?:\.0+)?%?$/};
  // Where a rule's inner part names what sits under the hovered compound
  // (`.menu:hover ul`, `.menu:hover > .panel`), the part the selector goes on
  // with, or '' when the rule styles the trigger itself.
  function afterHover(selector) {
    if (typeof selector !== 'string') return '';
    const at = selector.search(/:hover(?![\w-])/);
    return at < 0 ? '' : selector.slice(at + 6).replace(/^[^\s>+~]*/, '').trim();
  }
  function ruleElements(rule) {
    // A sheet a stylesheet import brings in is one more place a hover's rules
    // sit; a layer's rules are nested like a media query's.
    if (rule.styleSheet) {
      let imported = null;
      try { imported = rule.styleSheet.cssRules; } catch (error) { imported = null; }
      return imported ? [...imported].flatMap(ruleElements) : [];
    }
    return rule.cssRules?.length ? [...rule.cssRules].flatMap(ruleElements) : [rule];
  }
  // Whether an element has a place on the page to be scrolled to: a box the
  // page lays out and shows. A closed menu's rows, or a box of no size, have
  // nowhere to be reached, and scrolling cannot open them.
  function onPage(element) {
    return element.getClientRects().length > 0 && !hiddenByStyle(element);
  }
  // Whether the page keeps this box out of sight with one of the hiding
  // declarations it rests under. A box that has no size yet (an empty
  // underline that grows on a hover) is not out of sight: it is a spacer, and
  // a hover that gives it a size reveals nothing a gesture could reach.
  function hiddenByStyle(element) {
    for (let node = element, depth = 0; node && node.nodeType === 1 && depth++ < 32;
      node = node.parentElement || node.getRootNode().host) {
      const style = getComputedStyle(node);
      if (Object.entries(HIDDEN).some(([name, held]) => held.test(String(style.getPropertyValue(name)).trim())))
        return true;
    }
    return false;
  }
  // Whether an element the rule names after the hover is hidden now: what a
  // hover would take out of hiding. The rest of the selector only means
  // something under the box being hovered, so it is read from there: a rule
  // that opens another box's panel says nothing about this one.
  function hiddenNow(selector, under) {
    if (!selector) return false;
    let nodes = null;
    try { nodes = under ? under.querySelectorAll(selector) : document.querySelectorAll(selector); }
    catch (error) { return false; }
    for (const node of nodes) if (hiddenByStyle(node)) return true;
    return false;
  }
  // Whether one rule takes a box out of hiding on the hover: the part it names
  // under the hovered compound starts hidden, or its own declarations bring a
  // box the hidden declarations held down back into sight. A tint alone opens
  // nothing.
  function revealsOnHover(rule, element) {
    for (const inner of ruleElements(rule)) {
      const under = afterHover(String(inner && inner.selectorText || ''));
      // A rule that only tints the trigger itself lays no box out to be
      // reached: `display:flex` on a box the page already shows is a tint.
      if (!under) continue;
      if (hiddenNow(under, element)) return true;
      // A rule the hover gives a size or a place to (`height:2px` on an empty
      // underline) reveals nothing: it is the same box, drawn larger.
    }
    return false;
  }
  function markHoverTargets() {
    hoverTargets = new Set();
    let seen = 0;
    for (const sheet of document.styleSheets) {
      let rules = null;
      try { rules = sheet.cssRules; } catch (error) { rules = null; }
      if (!rules) continue;
      for (const rule of rules) {
        if (seen++ > MAX_HOVER_RULES) return hoverTargets.size;
        try {
          const trigger = hoverTrigger(rule.selectorText);
          if (!trigger) continue;
          // The marker is what a resting pointer puts on the element; it is
          // borrowed here to read which elements a hover would land on.
          const bare = trigger.split('.' + MARKER).join('');
          let beside = null;
          try { bare && (beside = document.querySelectorAll(bare)); } catch (error) { beside = null; }
          if (!beside) continue;
          for (const element of beside) {
            const had = element.classList.contains(MARKER);
            if (!had) element.classList.add(MARKER);
            let wanted = false;
            try { wanted = element.matches(trigger); } catch (error) { wanted = false; }
            if (!had) element.classList.remove(MARKER);
            if (!wanted || !revealsOnHover(rule, element)) continue;
            if (hoverTargets.size >= MAX_HOVER_TARGETS) return hoverTargets.size;
            hoverTargets.add(element);
          }
        } catch (error) {
          // A rule this reader cannot follow is not a trigger it can name.
        }
      }
    }
    return hoverTargets.size;
  }
  // Whether a hover on this box is what takes a box out of hiding: the page's
  // own rule says so, and the box the CSS hovers is the one a hand has to
  // reach. It is read from the sheets, not from the class index: the index
  // holds only the rules that style a box at rest.
  function hoverRevealsUnder(element) {
    let seen = 0;
    for (const sheet of document.styleSheets) {
      let rules = null;
      try { rules = sheet.cssRules; } catch (error) { rules = null; }
      if (!rules) continue;
      for (const rule of rules) {
        if (seen++ > MAX_HOVER_RULES) return false;
        const trigger = hoverTrigger(rule.selectorText || '');
        if (!trigger || !revealsOnHover(rule, element)) continue;
        let beside = false;
        try { beside = element.matches(trigger.split('.' + MARKER).join('')); } catch (error) { beside = false; }
        if (beside) return true;
      }
    }
    return false;
  }
  // The element on which a hover opens something: itself when a rule names
  // it, an ancestor when the rule hovers one, else the parent whose hovered
  // trigger opened a child that starts hidden. Words inside a trigger are the
  // trigger's own, so a hover over them opens what the trigger opens.
  // The boxes that hold a hover's targets, each with the targets it holds:
  // an element's ancestors are asked what they hold, where every child of
  // each of them used to be looked at. On a page of three thousand
  // paragraphs that was nine million looks for one read, and half a second.
  let hoverHolders = null, hoverHeldFor = null;
  function hoverReveals(element) {
    if (!hoverTargets) markHoverTargets();
    if (!hoverTargets.size) return false;
    if (hoverHeldFor !== hoverTargets) {
      hoverHolders = new Map();
      for (const target of hoverTargets) {
        const holder = target.parentElement, held = hoverHolders.get(holder);
        if (held) held.push(target); else if (holder) hoverHolders.set(holder, [target]);
      }
      hoverHeldFor = hoverTargets;
    }
    let depth = 0;
    for (let node = element; node && depth++ < 16; node = node.parentElement) {
      if (hoverTargets.has(node)) return true;
      for (const child of hoverHolders.get(node) || [])
        if (!child.checkVisibility?.()) return true;
    }
    return false;
  }

  // The number the tab calls this document's frame by, '' for the page's own
  // document. The browser process says it with each command: a frame of
  // another site cannot see the element that holds it.
  let frameAddress = '';
  const frameId = () => frameAddress;

  // The frame element a ref of this document names, which the browser
  // process asks the engine about (which frame it holds); nothing once the
  // element has left the page.
  function frameNode(ref) {
    try {
      const element = resolve(ref);
      return element.localName === 'iframe' || element.localName === 'frame' ? element : null;
    } catch (_) { return null; }
  }

  // Where a frame this document holds sits in it now, in this document's
  // viewport: a point read in the frame's own document moves by this to
  // become a point in this one.
  function frameOffset(ref, anchored) {
    const element = frameNode(ref);
    if (!element) throw failure('frame_gone', 'The frame has left the page; read the page again');
    // The click is a pointer event in the top document, at a point the caller
    // never saw: the frame is brought into view so that point is reachable.
    // A gesture stands where its step is, though: the box the step resolves to
    // is read in the frame's own document against coordinates the frame's
    // place then has to match, so it is measured where the page put it.
    if (anchored !== true) element.scrollIntoView({block: 'nearest', inline: 'nearest'});
    // A frame is a replaced element: its content view starts inside its
    // border and its padding, and a point placed in it is that view's own. A
    // point read in the frame's document is a point in the frame's viewport,
    // so the frame's own viewport position is the whole of the offset: no
    // scroll is added.
    const origin = frameOrigin(element);
    return {ok: true, message: 'Frame', x: origin.x, y: origin.y};
  }
  // Where a frame's own viewport starts in this document's: inside the
  // frame element's border and padding.
  function frameOrigin(element) {
    const box = element.getBoundingClientRect(), style = getComputedStyle(element);
    const inset = side => (parseFloat(style[`border${side}Width`]) || 0) + (parseFloat(style[`padding${side}`]) || 0);
    return {x: box.x + inset('Left'), y: box.y + inset('Top')};
  }
  // The frames this document shows, as many as a read of the page reads
  // with it: where a find or a search looks on when asked to, or when this
  // document holds no match.
  const MAX_SHOWN_FRAMES = 6;
  function shownFrames() {
    return [...document.querySelectorAll('iframe,frame')].filter(frameShown).slice(0, MAX_SHOWN_FRAMES)
      .map(element => ({ref: refOf(element), label: clean(element.title || element.name, MAX_NAME)}));
  }

  // A point in a frame's own document means nothing in the page: the caller
  // reads points in the main document, where clicking happens.
  function atInFrame() {
    return failure('invalid_request', `Read the element at a point in the page itself, not in frame ${frameId()}`);
  }

  function outline(from, most) {
    const {lines, frames: listed, cut, first, last} = outlineLines(from);
    // What `changes` compares with is a read from the top: reading on leaves
    // it as it was.
    const onward = first > 1;
    if (!onward) setLastLines(lines);
    const frame = frameId();
    const where = frame ? ` in frame ${frame}` : '';
    // A read that stopped short says so in its answer, not only in its last
    // line: an agent that reads the message and acts on it would otherwise
    // believe it has seen the whole page, when what it was asked to check may
    // be in the part it never saw.
    const read = onward ? `Read lines ${first} to ${last}${where}` : `Read ${lines.length} lines${where}`;
    // The page around a frame reads the frame's first lines with its own:
    // the answer says when the frame goes on, and the rest is read with the
    // frame's number.
    if (typeof most === 'number' && most > 0 && lines.length > most)
      return {ok: true, message: read, outline: lines.slice(0, most).join('\n'),
        data: {frames: listed.filter(frame => frame.at < most), more: true}};
    return {ok: true, message: cut ? `${read}, and stopped there: ${cut}` : read,
      outline: lines.join('\n'), data: {frames: listed, more: false}};
  }

  // Lines added and removed since the last read, by longest common
  // subsequence; refs are stable within a document, so an unchanged element
  // keeps its line.
  function changes() {
    // What a scroll answers says how far it took the page, so a turn that
    // changed no line still tells the caller the page moved, and by how much.
    const moved = takeScrollMoved(), place = takeScrollAt();
    const before = getLastLines(), {lines} = outlineLines();
    setLastLines(lines);
    const stands = place ? ` ${place.at} of ${place.of}` : '';
    const lost = scrollLost ? '; the page moved the box out of view while the wheel turned, so it stopped there' : '';
    scrollLost = false;
    const travelled = moved === null ? '' : (moved ? `Scrolled ${Math.abs(moved)}px ${moved > 0 ? 'down' : 'up'}${stands ? ',' : ''}${stands ? ' to' + stands : ''}`
      : `Did not move${stands ? ', at' + stands : ''}`) + lost;
    // A scroll answers with how far it went. A caller that had not read the
    // page is told so, not handed the whole page for turning a wheel.
    if (!before && moved !== null)
      return {ok: true, message: `${travelled}; the page had not been read before, so read it to see what it shows`, outline: ''};
    if (!before)
      return {ok: true, message: `First read: ${lines.length} lines`, outline: lines.join('\n')};
    if (before.length * lines.length > MAX_DIFF_CELLS)
      return {ok: true, message: `Too much changed to compare; the whole page, ${lines.length} lines`, outline: lines.join('\n')};
    const n = before.length, m = lines.length;
    const table = Array.from({length: n + 1}, () => new Uint32Array(m + 1));
    for (let i = n - 1; i >= 0; i--)
      for (let j = m - 1; j >= 0; j--)
        table[i][j] = before[i] === lines[j] ? table[i + 1][j + 1] + 1 : Math.max(table[i + 1][j], table[i][j + 1]);
    const out = [];
    let i = 0, j = 0, added = 0, removed = 0;
    while (i < n || j < m) {
      if (i < n && j < m && before[i] === lines[j]) { i++; j++; }
      else if (j < m && (i >= n || table[i][j + 1] >= table[i + 1][j])) { out.push('+ ' + lines[j++]); added++; }
      else { out.push('- ' + before[i++]); removed++; }
    }
    const diff = added || removed ? `${added} lines added, ${removed} removed` : 'Nothing changed';
    const told = moved === null ? diff : `${travelled}; ${diff[0].toLowerCase()}${diff.slice(1)}`;
    return {ok: true, message: told, outline: out.join('\n')};
  }

  // What a click or a hover at viewport point (x, y) would act on: the
  // element itself when it is one an agent can act on, else the nearest
  // ancestor that is.
  function targetAt(hit) {
    for (let node = hit; node && node.nodeType === 1; node = node.assignedSlot || node.parentNode || node.host) {
      if (INTERACTIVE.has(role(node)) || node.hasAttribute('onclick') ||
          node.localName === 'canvas' || node.getAttribute('draggable') === 'true' || hoverReveals(node))
        return node;
    }
    return null;
  }
  // The disclosure state of an element, which a hover-only menu changes.
  function expandedState(element) {
    const aria = element.getAttribute('aria-expanded');
    if (aria === 'true') return 'expanded';
    if (aria === 'false') return 'collapsed';
    for (const child of element.children) {
      if (child.checkVisibility && child.checkVisibility() && getComputedStyle(child).visibility === 'visible')
        return 'expanded';
    }
    return 'collapsed';
  }
  function revealState(element) {
    const ref = refOf(element), state = expandedState(element), label = summary(element, {naming: true});
    return {ref, state, line: `${label || element.localName} [ref=${ref}] ${state}`};
  }

  // What a click at viewport point (x, y) would hit. A point over a frame
  // hits the frame's own document: the frame is named with the point as its
  // document sees it, and the browser process asks that document in turn,
  // which answers with the point as the page gave it (`page`).
  function at(x, y, page) {
    const hit = topmost(x, y), said = page || {x, y};
    const where = `${said.x}, ${said.y}${frameId() ? ` in frame ${frameId()}` : ''}`;
    if (!hit) return {ok: true, message: `Nothing at ${where}`, data: {x: said.x, y: said.y, element: null}};
    const target = targetAt(hit);
    const data = {x: said.x, y: said.y, element: describe(hit), interactive: !!target};
    if (frameId()) data.frame = frameId();
    if (target) Object.assign(data, {ref: refOf(target), target: summary(target)});
    if (hit.localName === 'iframe' || hit.localName === 'frame') {
      const origin = frameOrigin(hit);
      data.frame_element = refOf(hit);
      data.inside = {x: x - origin.x, y: y - origin.y};
    }
    return {ok: true, message: target ? `${summary(target)} [ref=${data.ref}] is at ${where}` : `${describe(hit)} is at ${where}`, data};
  }

  // What the action in progress set off. It is armed before the action's
  // input reaches the page; once that input's dispatch and default action have
  // run, it says whether the page began leaving the document meanwhile: by a
  // navigation it started, or a form submission it planned, which navigates in
  // a task of its own. A navigation the page starts later is not the action's
  // to report: the reply says one is still possible.
  let watch = null;
  function arm(input) {
    // Keys typed into a child frame dispatch where this world cannot see them.
    const framed = input === 'key' && ['iframe', 'frame'].includes(document.activeElement?.localName);
    watch = framed ? null : {input, dispatched: input === 'script', navigate: null, submit: null, done: null};
    // A drag this world is driving may never come back as an event: its press
    // ends in the engine's own drop, where this document hears no pointerup.
    // The dispatch is then over all the same, so it is said after a turn rather
    // than waited for, and a fence behind it always answers. Any other pointer
    // input is an ordinary click, whose own event does arrive.
    if (watch && input === 'pointer' && gestureKind === 'drag') {
      const armed = watch;
      setTimeout(() => { armed.dispatched = true; report(); });
    }
    return {ok: true, message: 'Armed'};
  }
  const arrived = input => () => {
    if (!watch || watch.input !== input || watch.arrived) return;
    watch.arrived = true;
    // The rest of this input's dispatch, and its default action, run in the
    // current task.
    const current = watch;
    setTimeout(() => { current.dispatched = true; report(); });
  };
  for (const type of ['pointerup', 'mouseup', 'click', 'drop']) addEventListener(type, arrived('pointer'), true);
  // The moves of the pointer this document has heard. A drawn path sends
  // its next point once the page has heard the one before: two moves the
  // page has not handled yet reach it as one, and a line drawn through
  // seven points would have six.
  let heardMove = {x: -1, y: -1, n: 0}, moveWaiter = null;
  addEventListener('pointermove', event => {
    if (!event.isTrusted) return;
    heardMove = {x: event.clientX, y: event.clientY, n: heardMove.n + 1};
    if (moveWaiter) moveWaiter();
  }, true);
  function pointerHeard(x, y, after, wait) {
    const there = () => heardMove.n > after && Math.abs(heardMove.x - x) <= 1.5 && Math.abs(heardMove.y - y) <= 1.5;
    if (there()) return {ok: true, heard: true, n: heardMove.n};
    return new Promise(done => {
      const timer = setTimeout(() => { moveWaiter = null; done({ok: true, heard: false, n: heardMove.n}); }, wait);
      moveWaiter = () => {
        if (!there()) return;
        clearTimeout(timer);
        moveWaiter = null;
        done({ok: true, heard: true, n: heardMove.n});
      };
    });
  }
  // A press that started a drag ends with its drop in the engine, where these
  // listeners may never hear a pointerup; the drag's own end says so instead.
  addEventListener('dragend', arrived('pointer'), true);
  addEventListener('keyup', arrived('key'), true);
  addEventListener('submit', event => { if (watch) watch.submit = event; }, true);
  navigation.addEventListener('navigate', event => {
    if (watch && !event.destination.sameDocument && event.downloadRequest === null) watch.navigate = event;
  });
  function leaving(current) {
    const event = current.navigate;
    // A navigation the page intercepts stays in this document, at its URL.
    if (event && !event.defaultPrevented &&
        (!event.canIntercept || event.navigationType === 'reload' || location.href !== event.destination.url)) return true;
    const submit = current.submit;
    if (!submit || submit.defaultPrevented) return false;
    const form = submit.target, submitter = submit.submitter;
    const method = submitter?.hasAttribute('formmethod') ? submitter.formMethod : form.method;
    const target = submitter?.hasAttribute('formtarget') ? submitter.formTarget : form.target;
    return method !== 'dialog' && ['', '_self', '_top', '_parent'].includes(target.toLowerCase());
  }
  function report() {
    const current = watch;
    if (!current?.done || !current.dispatched) return;
    watch = null;
    current.done({ok: true, navigation: leaving(current) ? 'started' : 'possible'});
  }
  function fence() {
    if (!watch) return {ok: true, navigation: 'possible'};
    return new Promise(done => { watch.done = done; report(); });
  }

  // How much of what reveals something changed while the pointer moved: a
  // menu the hover opened, and the lines it added.
  function revealDelta(before) {
    const lines = outlineLines().lines;
    const kept = new Set(before.lines);
    const added = lines.filter(line => !kept.has(line));
    const gone = before.lines.filter(line => !lines.includes(line)).length;
    const revealing = added.length !== 0 || gone !== 0;
    const note = added.length ? `${added.length} lines added` : gone ? `${gone} lines removed` : 'nothing changed';
    return {revealing, added: added.slice(0, MAX_REVEAL_LINES), note};
  }

  // Viewport CSS pixels for a named element's visible centre, or for an
  // explicit point when no element is named.
  function landing(ref, x, y, verb) {
    if (!ref) return {x, y, element: null};
    const element = resolve(ref);
    enabled(element, ref);
    const center = point(element);
    if (!center) throw failure('not_visible', `${ref} has no visible area to ${verb}; scroll or open its container first`);
    if (verb !== 'hover') {
      const hit = topmost(center.x, center.y);
      if (!within(hit, element))
        throw failure('obscured', `${ref} is covered by ${describe(hit)}; close or scroll past it first`);
    }
    return {x: center.x, y: center.y, element};
  }

  // A contact path, and the same path for the synthetic fallback: `points`
  // are viewport CSS pixels, `local` are relative to the drawing element.
  function contactPath(ref, raw) {
    const origin = ref ? resolve(ref).getBoundingClientRect() : {left: 0, top: 0};
    // Pairs, because the engine reads them as plain points in view pixels.
    const points = raw.map(([x, y]) => [ref ? origin.left + x : x, ref ? origin.top + y : y]);
    return {points};
  }

  // Resolves once the document stops changing for `quiet` ms after it has
  // loaded, or after `timeout` ms.
  function settle(quiet, timeout) {
    return new Promise(done => {
      const start = performance.now();
      let last = start, mutations = 0;
      let resources = performance.getEntriesByType('resource').length;
      const observer = new MutationObserver(records => { mutations += records.length; last = performance.now(); });
      observer.observe(document, {subtree: true, childList: true, attributes: true, characterData: true});
      const tick = () => {
        const now = performance.now(), count = performance.getEntriesByType('resource').length;
        if (count !== resources) { resources = count; last = now; }
        const settled = document.readyState === 'complete' && now - last >= quiet;
        if (settled || now - start >= timeout) {
          observer.disconnect();
          const waited = Math.round(now - start);
          done({ok: true, message: settled ? `Settled after ${waited} ms` : `Still changing after ${waited} ms`,
            data: {settled, waited_ms: waited, mutations, resources}});
          return;
        }
        setTimeout(tick, Math.min(MAX_SETTLE_POLL_MS, quiet));
      };
      tick();
    });
  }

  // The site a form sends its data to, when another than the page's own.
  function foreignTarget(form, submitter) {
    try {
      // A submitter's `formAction` reads as the page's own URL unless it sets
      // `formaction`; only then does it override the form's target.
      const own = submitter && submitter.hasAttribute('formaction') ? submitter.formAction : '';
      const action = new URL(own || form.action || location.href, location.href);
      return action.origin !== location.origin && /^https?:$/.test(action.protocol) ? action.origin : null;
    } catch { return null; }
  }
  function guardSubmission(form, submitter, allowed) {
    const target = form && foreignTarget(form, submitter);
    if (target && !allowed)
      throw failure('cross_origin_submit', `This form sends its data to ${target}, another site than ${location.origin}; submit it with cross_origin to allow that`);
  }

  function walkLines(maxLines, maxChars) {
    const lines = [], frames = [];
    let chars = 0, visited = 0, texts = 0, truncated = false, pending = '', pendingDepth = 0;
    const emit = (depth, text) => {
      if (lines.length >= maxLines || chars >= maxChars) { truncated = true; return false; }
      const line = '  '.repeat(Math.min(depth, 24)) + '- ' + text;
      lines.push(line);
      chars += line.length + 1;
      return true;
    };
    // Running text is printed whole. Where the page breaks it with two line
    // breaks, as pages without paragraph elements do, each part is a line of
    // its own; and a run longer than a line carries goes on in the next
    // line, broken at the end of a sentence, so no word of it is left out.
    const flush = () => {
      const run = pending;
      pending = '';
      for (const paragraph of run.split(PARAGRAPH_BREAK)) {
        let text = clean(paragraph.replaceAll(LINE_BREAK, ' '), Infinity);
        while (text) {
          let piece = text;
          if (piece.length > MAX_PROSE) {
            const held = piece.slice(0, MAX_PROSE);
            const sentence = Math.max(held.lastIndexOf('. '), held.lastIndexOf('? '), held.lastIndexOf('! ')) + 1;
            const word = held.lastIndexOf(' ');
            piece = piece.slice(0, sentence > MAX_PROSE / 2 ? sentence : word > MAX_PROSE / 2 ? word : MAX_PROSE);
          }
          text = text.slice(piece.length).trim();
          if (!emit(pendingDepth, `text ${quote(piece.trim())}`)) return;
        }
      }
    };
    markHoverTargets();
    const hovering = hoverTargets || new Set();
    // A block of code is read as it is written: its line breaks and its
    // indentation are part of what it says. It takes the line the walk kept
    // for it, and a long one goes on in the next line, broken between two
    // of its own lines.
    const placeCode = (at, depth, parts, ref) => {
      let rest = parts.join('').replace(BOX_BREAK, '\n').replace(/[ \t]+$/gm, '').replace(/^\n+|\n+$/g, '');
      const placed = [], indent = '  '.repeat(Math.min(depth, 24));
      while (rest) {
        const end = rest.length > MAX_PROSE ? rest.lastIndexOf('\n', MAX_PROSE) : rest.length;
        const piece = rest.slice(0, end > MAX_PROSE / 2 ? end : MAX_PROSE);
        rest = rest.slice(piece.length).replace(/^\n/, '');
        placed.push(`${indent}- code ${quote(piece)}${placed.length ? '' : ref}`);
      }
      // A block with no words of its own is listed only for what it holds.
      if (!placed.length && lines.length > at + 1) placed.push(`${indent}- code${ref}`);
      chars += placed.reduce((sum, line) => sum + line.length + 1, 0) - lines[at].length - 1;
      lines.splice(at, 1, ...placed);
      for (const frame of frames) if (frame.line > at) frame.line += placed.length - 1;
      if (lines.length > maxLines) { lines.length = maxLines; truncated = true; }
    };
    const walk = (parent, depth, context) => {
      for (const child of flatChildren(parent)) {
        if (truncated) return;
        visit(child, depth, context);
      }
    };
    const visit = (node, depth, context) => {
      if (node.nodeType === 1 ? ++visited > MAX_VISITED : ++texts > MAX_VISITED_TEXT) { truncated = true; return; }
      if (node.nodeType === 3) {
        if (!context.visible || context.folded) return;
        if (context.code) context.code.push(node.data);
        else if (!context.inside) {
          if (!pending) pendingDepth = depth;
          pending += node.data;
        }
        return;
      }
      if (node.nodeType !== 1) return;
      const element = node, tag = element.localName;
      if (SKIP.has(tag) || element.getAttribute('aria-hidden') === 'true') return;
      if (tag === 'input' && type(element) === 'hidden') return;
      // A line break inside running text: two of them end a paragraph.
      if (tag === 'br') {
        if (context.code) context.code.push('\n');
        else if (pending && !context.inside && context.visible) pending += LINE_BREAK;
        return;
      }
      if (!laidOut(element, context.folded)) return;
      const style = getComputedStyle(element);
      const visible = style.visibility === 'visible';
      const block = !style.display.startsWith('inline') && style.display !== 'contents';
      if (block) { flush(); if (context.code) context.code.push(LINE_BREAK); }
      const pointer = style.cursor === 'pointer';
      let kind = visible ? role(element) : '';
      // Preformatted text with no role of its own is a block of code.
      const code = tag === 'pre' && visible && !kind && !context.code ? [] : null;
      // A page's drag source is what a gesture grabs, so a gesture addresses
      // it by ref: a list item that is also a listitem is still the thing a
      // hand moves. Roles only a container carries keep their words.
      if (!INTERACTIVE.has(kind) && kind !== 'heading' && element.draggable) kind = 'draggable';
      // A page that keeps a menu hidden until the pointer rests on it names
      // the element a hand has to reach, so a gesture can address it by ref.
      // A box a hover only tints is not a box it opens: a hand has nothing
      // new to reach, so it is not listed as one a hover reveals.
      if (!INTERACTIVE.has(kind) && kind !== 'heading' && hovering.has(element) && hoverRevealsUnder(element))
        kind = 'hoverable';
      // A hover-only menu's trigger has no role of its own, no click and no
      // pointer cursor; it is found only by the rule its hover applies to.
      if (!kind && visible && !context.inside && tag !== 'body' && tag !== 'html' && hoverReveals(element))
        kind = 'hint';
      if (!kind && visible && !context.inside && tag !== 'label' && tag !== 'body' && tag !== 'html' &&
          (element.hasAttribute('onclick') || (element.hasAttribute('tabindex') && element.tabIndex >= 0) ||
           (pointer && !context.pointer))) kind = 'clickable';
      // A thing a gesture acts on is named for that gesture before the role
      // its element carries, so a list row that is dragged is not just a row.
      if (tag === 'canvas' && visible && !context.inside) kind = 'canvas';
      else if (visible && !context.inside && element.getAttribute('draggable') === 'true') kind = 'draggable';
      // A bound label's words are its control's name; listing them again is noise.
      const inner = {inside: context.inside || (tag === 'label' && !!element.control), pointer, visible,
        folded: folds(element, style), code: code || context.code};
      if (kind === 'img') {
        flush();
        const label = name(element, kind);
        emit(depth, label ? `${kind} ${quote(label)}` : kind);
        return;
      }
      // Code is one thing to read, whatever a highlighter cut it into. A
      // block a gesture can reach (one that scrolls, one the page made
      // clickable) keeps its ref, and so does what is acted on inside it.
      if (code) {
        flush();
        const at = lines.length;
        if (!emit(depth, 'code')) return;
        inner.inside = true;
        walk(element, depth + 1, inner);
        const reached = INTERACTIVE.has(kind) || element.scrollHeight > element.clientHeight ||
          element.scrollWidth > element.clientWidth;
        placeCode(at, depth, code, reached ? ` [ref=${refOf(element)}]` : '');
        return;
      }
      // A drawing surface is one element a gesture draws on, never the boxes
      // of whatever the page painted into it. Its size is part of what a
      // gesture needs: a point is drawn in the surface's own coordinates.
      if (tag === 'canvas') {
        flush();
        const label = name(element, kind);
        const size = `${Math.round(element.width || element.clientWidth || 0)}x${Math.round(element.height || element.clientHeight || 0)}`;
        emit(depth, `${kind}${label ? ' ' + quote(label) : ''} ${size} [ref=${refOf(element)}]`);
        return;
      }
      if (kind === 'iframe') {
        flush();
        // A frame is its own document: the line names the id an action takes to
        // run there, and the address, so the caller knows what it would reach.
        // A frame whose mark-up this document holds is listed here even when
        // its document is another site's: it is a frame of this page, and the
        // id its line carries is one an action can take to it.
        const label = clean(element.title || element.name, MAX_NAME);
        const parts = [kind];
        if (label) parts.push(quote(label));
        parts.push(`[frame=${frameMark(element)}]`);
        const src = element.src ? `src=${quote(clean(element.src, 300))}` : '';
        if (src) parts.push(src);
        const line = lines.length;
        if (emit(depth, parts.join(' '))) frames.push({ref: refOf(element), line, shown: frameShown(element), label});
        return;
      }
      if (INTERACTIVE.has(kind) || kind === 'heading') {
        flush();
        const label = name(element, kind, {naming: kind === 'hint'}), parts = [kind];
        if (label) parts.push(quote(label));
        if (INTERACTIVE.has(kind)) parts.push(`[ref=${refOf(element)}]`);
        parts.push(...details(element, kind));
        // What each extra kind is for, so an agent knows which action uses it.
        if (kind === 'hint') parts.push('(a hover opens more)');
        if (kind === 'draggable') parts.push('(draggable)');
        if (!emit(depth, parts.join(' '))) return;
        if (tag === 'select') {
          const options = [...element.options];
          for (const option of options.slice(0, MAX_OPTIONS))
            emit(depth + 1, `option ${quote(clean(option.label, MAX_NAME))}${option.selected ? ' selected' : ''}${option.disabled ? ' disabled' : ''}`);
          if (options.length > MAX_OPTIONS) emit(depth + 1, `text "… ${options.length - MAX_OPTIONS} more options"`);
          return;
        }
        if (kind === 'textbox' || kind === 'searchbox') return;
        // The line's name already carries this element's text; keep looking
        // only for interactive elements nested inside it.
        inner.inside = true;
        walk(element, depth + 1, inner);
        return;
      }
      if (CONTAINERS.has(kind)) {
        const label = ['region', 'dialog', 'alertdialog', 'form', 'group', 'table', 'navigation', 'figure'].includes(kind)
          ? name(element, kind) : '';
        const start = lines.length;
        if (!emit(depth, label ? `${kind} ${quote(label)}` : kind)) return;
        // A box a person can scroll has to be reachable to turn a wheel over
        // it: its ref is named even though a scroll is not its role.
        if (element.scrollHeight > element.clientHeight || element.scrollWidth > element.clientWidth) {
          const at = lines[lines.length - 1].lastIndexOf('[ref=');
          if (at < 0) lines[lines.length - 1] += ` [ref=${refOf(element)}]`;
        }
        walk(element, depth + 1, inner);
        flush();
        const count = lines.length - start - 1;
        if (count === 0 && !KEEP_EMPTY.has(kind) && !truncated) lines.pop();
        else if (count === 1 && !label && COLLAPSE.has(kind)) {
          const child = lines.pop().trimStart().slice(2);
          lines[start] += ': ' + (child.startsWith('text "') ? child.slice(5) : child);
        }
        return;
      }
      // What a closed shadow tree shows is not read, and the read says so:
      // the line names the element, so a picture of it can be asked for.
      if (visible && !context.inside && sealed(element)) {
        flush();
        emit(depth, `text ${quote(`… <${tag}> shows what is in a closed shadow tree, which a read cannot reach; ` +
          'a capture of it shows it')} [ref=${refOf(element)}]`);
        return;
      }
      walk(element, depth, inner);
      if (block) { flush(); if (context.code) context.code.push(LINE_BREAK); }
    };
    if (document.body) visit(document.body, 0, {inside: false, pointer: false, visible: true});
    flush();
    prune();
    return {lines, truncated, frames};
  }

  // The outline is read a part at a time: as much as one read carries, from
  // the line the caller names (the first when it names none). The page is
  // walked whole each time, so a line keeps its number between reads of a
  // page that has not changed, and a read that stopped is read on from where
  // it stopped.
  function outlineLines(from) {
    const {lines: all, truncated: beyond, frames: listed} = walkLines(MAX_ALL_LINES, MAX_ALL_CHARS);
    const first = Math.max(1, Math.floor(from) || 1) - 1;
    if (first > 0 && first >= all.length)
      throw failure('invalid_request', `The page's outline has ${all.length} lines; there is no line ${first + 1} to read from`);
    let end = first, chars = 0;
    while (end < all.length && end - first < MAX_LINES && chars < MAX_CHARS) chars += all[end++].length + 1;
    const lines = all.slice(first, end);
    // What stopped the read, so the reader knows whether more of the page is
    // still there to reach and how to reach it.
    const cap = chars >= MAX_CHARS && lines.length < MAX_LINES
      ? `${MAX_CHARS} characters of outline` : `${MAX_LINES} outline lines`;
    const cut = end < all.length
      ? `this read carries ${cap} (lines ${first + 1} to ${end} of ${beyond ? 'more than ' : ''}${all.length}) and the page goes on past them: ` +
        `read on from line ${end + 1} with from, or name an element by where it is on the screen with read-page-element-at`
      : beyond ? `this read ends at line ${end}, as far as a page is read, and the page goes on past them: ` +
        'name an element by where it is on the screen with read-page-element-at' : '';
    if (cut) lines.push(`- text "… ${cut}"`);
    // The frames the lines of this read list, each by the line that names it.
    const held = listed.filter(frame => frame.line >= first && frame.line < end)
      .map(frame => ({ref: frame.ref, at: frame.line - first, shown: frame.shown, label: frame.label}));
    return {lines, cut, frames: held, first: first + 1, last: end, total: all.length};
  }

  // Where the page says something: the outline lines that hold the words,
  // by their numbers, so a reader goes to them instead of reading the page
  // through to find them. It reads what the outline reads, so what the
  // outline hides (a secret's value) is not found either.
  const MAX_SEARCH_HITS = 30, SEARCH_CONTEXT = 160;
  function search(text) {
    const wanted = clean(text, MAX_TEXT);
    if (!wanted) throw failure('invalid_request', 'A search needs the words to look for');
    // A line holds its words as it prints them, quotes escaped.
    const needle = quote(wanted).slice(1, -1).toLowerCase();
    const {lines: all, truncated: beyond} = walkLines(MAX_ALL_LINES, MAX_ALL_CHARS);
    const hits = [];
    let total = 0;
    all.forEach((line, index) => {
      const at = line.toLowerCase().indexOf(needle);
      if (at < 0) return;
      total++;
      if (hits.length >= MAX_SEARCH_HITS) return;
      const start = Math.max(0, at - SEARCH_CONTEXT), end = Math.min(line.length, at + needle.length + SEARCH_CONTEXT);
      hits.push({line: index + 1, text: (start ? '…' : '') + line.slice(start, end).trim() + (end < line.length ? '…' : '')});
    });
    const where = frameId() ? ` in frame ${frameId()}` : '';
    if (!total)
      return {ok: true, message: `Nothing on the page${where} says ${quote(wanted)}${beyond ? ', as far as a page is read' : ''}. ` +
        'Words on either side of a link, or in two paragraphs, are on separate lines: search for fewer of them' +
        (framesNote() ? '.' + framesNote() : ''),
        outline: '', data: {matches: [], total: 0, frames: shownFrames()}};
    const listed = total > hits.length ? `, the first ${hits.length} listed` : '';
    return {ok: true, message: `${total} ${total === 1 ? 'line says' : 'lines say'} ${quote(wanted)}${where}${listed}; ` +
      'read on from a line by giving its number as from',
      outline: hits.map(hit => `${hit.line}: ${hit.text}`).join('\n'), data: {matches: hits, total, frames: shownFrames()}};
  }

  // What the page shows that an agent can act on, as the outline prints it:
  // the same role words, the same names, the same order, so a caller who read
  // a line names the element back by what it says. The walk keeps the places
  // a line sits under — a named dialog, form or group, a table with a caption
  // — so a box is found by the words its own line is printed beneath. One
  // walk, so a find stays a bounded read of the page whatever the page does.
  const MAX_FIND_CANDIDATES = 40000, MAX_FIND_NEAR = 20, MAX_FIND_AMBIGUOUS = 12;
  // A box the outline names, and what a line printed inside it sits under:
  // the named ones print their name, the rest a word for what they hold.
  const FIND_NAMED = new Set(['dialog', 'alertdialog', 'form', 'group', 'table', 'region']);

  // The kind the outline lists an element under, read as the outline reads
  // it: the role its element carries, a drag source, a thing a pointer asks
  // for, and what a bare element with a click is.
  function kindOf(element, visible, inside, pointer) {
    if (!visible) return '';
    let kind = role(element);
    if (!INTERACTIVE.has(kind) && kind !== 'heading' && element.draggable) kind = 'draggable';
    const tag = element.localName;
    if (!kind && visible && !inside && tag !== 'body' && tag !== 'html' && tag !== 'label' &&
        (element.hasAttribute('onclick') || (element.hasAttribute('tabindex') && element.tabIndex >= 0) ||
         (getComputedStyle(element).cursor === 'pointer' && !pointer))) kind = 'clickable';
    if (!kind && tag === 'body') return '';
    return kind;
  }

  // The tree a person sees is the flat tree, and every read follows it: an
  // element with a shadow tree shows that tree instead of its own children,
  // and a slot shows the nodes it was given, or its own children when it was
  // given none. A slot handed on to a slot of another shadow tree is one of
  // the nodes that slot was given, and is followed in its turn.
  function flatChildren(parent) {
    const root = parent.shadowRoot;
    if (root) { watchRoot(root); return root.childNodes; }
    const given = parent.localName === 'slot' && parent.assignedNodes ? parent.assignedNodes() : null;
    return given && given.length ? given : parent.childNodes;
  }
  // An element is on the page when it is laid out. One whose display is
  // `contents` has no box of its own and is there none the less: what it
  // holds is laid out in its place. A slot is one, and so is the article of
  // a page that sets its parts in a grid. Only where what holds it is folded
  // away is it off the page with the rest.
  function laidOut(element, folded) {
    if (!element.checkVisibility || element.checkVisibility()) return true;
    return !folded && getComputedStyle(element).display === 'contents';
  }
  // Whether an element shows itself and keeps what it holds off the page: a
  // details that is closed, past its summary, and content its style skips.
  const folds = (element, style) => style.contentVisibility === 'hidden' ||
    (element.localName === 'details' && !element.open);
  // A closed shadow tree is out of a script's reach, so what it shows is not
  // read. It is known by what is left of it: a custom element that is laid
  // out with a box, has no shadow tree a script may see, and shows none of
  // its own children.
  function sealed(element) {
    if (!element.localName.includes('-') || element.shadowRoot || !element.matches(':defined')) return false;
    for (const child of element.childNodes) {
      if (child.nodeType === 1 && laidOut(child)) return false;
      if (child.nodeType === 3 && child.data.trim()) {
        const range = document.createRange();
        range.selectNodeContents(child);
        if (range.getClientRects().length) return false;
      }
    }
    const box = element.getBoundingClientRect();
    return box.width > 0 && box.height > 0;
  }
  // The words an element shows, gathered along the flat tree: what its own
  // children say nothing of when the words are its shadow tree's or a slot's.
  const MAX_SHOWN_TEXT_NODES = 2000;
  function shownText(element) {
    let text = '', visited = 0;
    const gather = (parent, visible, folded) => {
      for (const child of flatChildren(parent)) {
        if (text.length > MAX_NAME * 2 || ++visited > MAX_SHOWN_TEXT_NODES) return;
        if (child.nodeType === 3) { if (visible && !folded) text += child.data; continue; }
        if (child.nodeType !== 1 || SKIP.has(child.localName) || child.getAttribute('aria-hidden') === 'true' ||
            !laidOut(child, folded)) continue;
        const style = getComputedStyle(child);
        if (!style.display.startsWith('inline') && style.display !== 'contents') text += ' ';
        gather(child, style.visibility === 'visible', folds(child, style));
      }
    };
    gather(element, true, false);
    return text;
  }
  // A shadow tree is watched as the document is, from the first read that
  // meets it: a password field in it that the page turns into plain text is
  // still one.
  const watchedRoots = new WeakSet();
  let typeWatch = null;
  function watchRoot(root) {
    if (watchedRoots.has(root)) return;
    watchedRoots.add(root);
    for (const field of root.querySelectorAll('input[type=password i]')) wasPassword.add(field);
    if (typeWatch) typeWatch.observe(root, {subtree: true, attributes: true, attributeOldValue: true, attributeFilter: ['type']});
  }

  // Every element of this document the outline prints with a ref, in the
  // outline's own order, each with the kind, the name, the places its line
  // may sit under, and the line itself.
  function findCandidates() {
    const found = [];
    let visited = 0;
    const under = (places, text) => `${places}${text}${text.endsWith(':') ? ' ' : ': '}`;
    const visit = (element, inside, places, pointer, folded) => {
      if (++visited > MAX_FIND_CANDIDATES) return;
      const tag = element.localName;
      if (SKIP.has(tag) || element.getAttribute('aria-hidden') === 'true') return;
      if (tag === 'input' && type(element) === 'hidden') return;
      if (!laidOut(element, folded)) return;
      const style = getComputedStyle(element);
      const visible = style.visibility === 'visible', within = folds(element, style);
      const kind = kindOf(element, visible, inside, pointer || style.cursor === 'pointer');
      // A frame is its own document, read with the frame's id.
      if (kind === 'iframe') return;
      const inner = inside || (tag === 'label' && !!element.control);
      if (INTERACTIVE.has(kind) || kind === 'heading') {
        const label = name(element, kind, {naming: kind === 'hint'}) || '';
        found.push({element, kind, label, places, line: `${places}${lineOf(label, kind, element)}`});
        // A select prints its own line and its options beneath it.
        if (tag === 'select') for (const option of element.options) {
          const optionName = clean(option.label, MAX_NAME) || '';
          found.push({element: option, kind: 'option', label: optionName, places: under(places, `combobox ${quote(label)}`),
            line: `${under(places, `combobox ${quote(label)}`)}option ${quote(optionName)}`});
        }
        // The line's own name carries what is inside it, so what is found
        // within is found under the line it is printed beneath.
        if (kind === 'textbox' || kind === 'searchbox') return;
        for (const child of flatChildren(element)) if (child.nodeType === 1)
          visit(child, true, under(places, `${kind}${label ? ' ' + quote(label) : ''}`), style.cursor === 'pointer', within);
        return;
      }
      if (CONTAINERS.has(kind)) {
        const label = FIND_NAMED.has(kind) ? name(element, kind) : '';
        const bar = `${kind}${label ? ' ' + quote(label) : ''}`;
        for (const child of flatChildren(element)) if (child.nodeType === 1)
          visit(child, inner, label ? under(places, bar) : places, pointer || style.cursor === 'pointer', within);
        return;
      }
      for (const child of flatChildren(element)) if (child.nodeType === 1)
        visit(child, inner, places, pointer || style.cursor === 'pointer', within);
    };
    if (document.body) visit(document.body, false, '', false);
    return found;
  }

  // A line as the outline prints it, without the depth it is printed at: the
  // words a caller read, which is what a find answers with.
  function lineOf(label, kind, element) {
    const parts = [kind];
    if (label) parts.push(quote(label));
    if (INTERACTIVE.has(kind)) parts.push(`[ref=${refOf(element)}]`);
    parts.push(...details(element, kind));
    if (kind === 'hint') parts.push('(a hover opens more)');
    if (kind === 'draggable') parts.push('(draggable)');
    return parts.join(' ');
  }

  // How a name matches what a line prints: as the whole name exactly, then
  // in any letter case, then as a part of it.
  const MATCHES = [
    (label, wanted) => label === wanted,
    (label, wanted) => label.toLowerCase() === wanted.toLowerCase(),
    (label, wanted) => label.toLowerCase().includes(wanted.toLowerCase())
  ];
  const LOOSE = ['', ' in any letter case', ' by part of its name'];

  function described(role, name) {
    return [role ? `role ${quote(role)}` : '', name ? `name ${quote(name)}` : '']
      .filter(Boolean).join(' and ');
  }

  // The distance about, so the nearest names are listed first: a letter
  // slipped in a word is near, and so is a name sharing its start, which is
  // how a person hears two names as alike.
  function closeness(a, b) {
    const left = a.toLowerCase(), right = b.toLowerCase();
    const row = Array.from({length: right.length + 1}, (_, index) => index);
    for (let i = 1; i <= left.length; i++) {
      let corner = row[0];
      row[0] = i;
      for (let j = 1; j <= right.length; j++) {
        const held = row[j];
        row[j] = Math.min(row[j] + 1, row[j - 1] + 1, corner + (left[i - 1] === right[j - 1] ? 0 : 1));
        corner = held;
      }
    }
    let shared = 0;
    while (shared < left.length && shared < right.length && left[shared] === right[shared]) shared++;
    return row[right.length] - shared / (Math.max(left.length, right.length) + 1);
  }

  // A find and a search read one document. A frame holds its own, so a
  // caller told nothing matches is told where else to look, by the numbers
  // those frames' own reads take, rather than left believing the page holds
  // none of it.
  function framesNote() {
    const elsewhere = [...document.querySelectorAll('iframe,frame')].filter(frameShown).slice(0, 8).map(element => {
      const label = clean(element.title || element.name, MAX_NAME);
      return `frame=${frameMark(element)}${label ? ` (${quote(label)})` : ''}`;
    });
    return elsewhere.length
      ? ` A frame is its own document: read it with read-page {${elsewhere.join(', ')}} or name it with frame, such as ${elsewhere[0].split(' ')[0]}`
      : '';
  }

  // What the page holds, so a caller who described nothing it has is told
  // what it nearly said instead of being sent looking again.
  function findsNothing(role, name, all) {
    const asked = described(role, name);
    const frames = framesNote();
    if (!all.length)
      return failure('not_found', `Nothing on the page is ${asked}; the page shows no element an action can act on.${frames}`);
    const scored = all.map(candidate => ({candidate, near: name ? closeness(candidate.label, name)
      : role ? closeness(candidate.kind, role) : 0}));
    scored.sort((a, b) => a.near - b.near);
    const nearest = scored.slice(0, MAX_FIND_NEAR).map(({candidate}) => candidate.line);
    return failure('not_found', `Nothing on the page is ${asked}. The nearest are: ${nearest.join('; ')}.${frames} ` +
      'A find names controls and headings; words in the page\'s text are found by searching for them');
  }

  // The element a stable description names, answered with its ref in this
  // document as it is now, so a step recorded once plays again after the page
  // has loaded. Nothing is guessed: several equal matches without a number are
  // refused and listed, and a description the page does not hold names the
  // nearest it has.
  function find(command) {
    const role = typeof command.role === 'string' && command.role.trim() ? command.role.trim().toLowerCase() : null;
    const name = typeof command.name === 'string' && command.name.trim() ? command.name.trim() : null;
    if (!role && !name)
      throw failure('invalid_request', 'find-page-element needs a role, a name, or both, such as name="Email" or role=button name="Remove"');
    const nth = typeof command.nth === 'number' ? command.nth : null;
    const all = findCandidates();
    const ofRole = role ? all.filter(candidate => candidate.kind === role) : all;
    const asked = described(role, name);
    // Every match of this document, for a find that looks through the
    // page's frames as well: the browser process counts them together, by
    // the closest way any document matched.
    if (command.all === true) {
      for (const step of [0, 1, 2]) {
        if (!name && step > 0) break;
        const matches = name ? ofRole.filter(candidate => MATCHES[step](candidate.label, name)) : ofRole;
        if (!matches.length) continue;
        return {ok: true, message: `${matches.length} found`, data: {step, total: matches.length, asked,
          lines: matches.slice(0, MAX_FIND_AMBIGUOUS).map(candidate => candidate.line), frames: shownFrames()}};
      }
      return {ok: true, message: 'None found', data: {step: 3, total: 0, asked, lines: [], frames: shownFrames(),
        nothing: findsNothing(role, name, all).message}};
    }
    // The whole name first; then in any letter case, then by a part of it,
    // each only once nothing matched the way before it.
    for (const step of [0, 1, 2]) {
      if (!name && step > 0) break;
      const matches = name ? ofRole.filter(candidate => MATCHES[step](candidate.label, name)) : ofRole;
      if (!matches.length) continue;
      const where = matches[0].places ? ` under ${matches[0].places.replace(/: $/, '')}` : '';
      const loosely = name ? LOOSE[step] : '';
      if (nth === null && matches.length > 1) {
        const listed = matches.slice(0, MAX_FIND_AMBIGUOUS)
          .map((candidate, index) => `(${index + 1}) ${candidate.line}`);
        const more = matches.length > listed.length ? ` and ${matches.length - listed.length} more` : '';
        throw failure('ambiguous', `${matches.length} elements are ${asked}${loosely}${where}: ` +
          `${listed.join('; ')}${more}. Name one by its number, nth`);
      }
      const pick = nth === null ? matches[0] : matches[nth - 1];
      if (!pick)
        throw failure('not_found', `The page has ${matches.length} element${matches.length === 1 ? '' : 's'} ${asked}${loosely}` +
          `${where}, so there is no (${nth}); name one from 1 to ${matches.length}`);
      const ref = refOf(pick.element);
      const number = nth === null ? 1 : nth;
      const data = {element: ref, role: pick.kind, name: pick.label, line: pick.line,
        nth: number, matches: matches.length};
      return {ok: true, ...data, data,
        message: `Found ${pick.line}${nth === null ? '' : ` of ${matches.length}`}`};
    }
    // Nothing here: the frames this document shows are where the browser
    // process looks on.
    const nothing = findsNothing(role, name, all);
    nothing.frames = shownFrames();
    nothing.asked = asked;
    return nothing;
  }

  function clip(rect) {
    const left = Math.max(rect.left, 0), top = Math.max(rect.top, 0);
    const right = Math.min(rect.right, innerWidth), bottom = Math.min(rect.bottom, innerHeight);
    return right > left && bottom > top ? {x: (left + right) / 2, y: (top + bottom) / 2} : null;
  }
  function point(element) {
    element.scrollIntoView({block: 'center', inline: 'center', behavior: 'instant'});
    for (const rect of element.getClientRects()) {
      const center = rect.width > 0 && rect.height > 0 ? clip(rect) : null;
      if (center) return center;
    }
    return null;
  }
  function topmost(x, y) {
    let hit = document.elementFromPoint(x, y);
    while (hit && hit.shadowRoot) {
      let inner = hit.shadowRoot.elementFromPoint(x, y);
      // Over words an element hands to a slot of its shadow tree the answer
      // is that element again: what is drawn there is the box of its shadow
      // tree that holds the slot, a button around its own label.
      if (inner === hit) inner = hit.shadowRoot.elementsFromPoint(x, y).find(under => under.getRootNode() === hit.shadowRoot);
      if (!inner || inner === hit) break;
      hit = inner;
    }
    return hit;
  }
  // Whether a node is drawn inside an element: up the flat tree, so what a
  // slot was given is inside what holds the slot.
  function within(node, element) {
    for (; node; node = node.assignedSlot || node.parentNode || node.host) if (node === element) return true;
    return false;
  }
  function describe(element) {
    if (!element) return 'nothing';
    const id = element.id ? `#${element.id}` : '';
    const classes = typeof element.className === 'string' && element.className.trim()
      ? '.' + element.className.trim().split(/\s+/).slice(0, 2).join('.') : '';
    return `${summary(element)} (<${element.localName}${id}${classes}>)`;
  }
  function enabled(element, ref) {
    if (element.disabled || element.getAttribute('aria-disabled') === 'true')
      throw failure('disabled', `${ref} is disabled`);
  }

  function locate(ref, crossOrigin) {
    const element = resolve(ref);
    enabled(element, ref);
    chooserRefusal(ref, element, 'operating');
    // Clicking a submit button submits, so it needs the same consent.
    if ((element.localName === 'button' || element.localName === 'input') &&
        (element.type === 'submit' || element.type === 'image') && element.form)
      guardSubmission(element.form, element, crossOrigin);
    let target = element, center = point(element);
    if (!center || !within(topmost(center.x, center.y), element)) {
      // Styled checkboxes hide the input itself and are operated through
      // their label, exactly as a person clicks them.
      for (const label of element.labels || []) {
        const labelCenter = point(label);
        if (labelCenter && within(topmost(labelCenter.x, labelCenter.y), label)) {
          target = label;
          center = labelCenter;
          break;
        }
      }
    }
    if (!center) throw failure('not_visible', `${ref} has no visible area to click; scroll or open its container first`);
    const hit = topmost(center.x, center.y);
    if (!within(hit, target))
      throw failure('obscured', `${ref} is covered by ${describe(hit)}; close or scroll past it first`);
    return {ok: true, x: center.x, y: center.y, message: `Clicked ${summary(element)}`};
  }

  // A pointer step named this element, so the box it is stepped around
  // is the element's own scroll-adjusted box, never a point inside it.
  function locateBox(ref) {
    const element = resolve(ref);
    const box = element.getBoundingClientRect();
    if (!(box.width > 0 && box.height > 0))
      throw failure('not_visible', `${ref} has no box on the page`);
    return {ok: true, message: `Found ${summary(element)}`, data: {x: box.left, y: box.top, width: box.width, height: box.height}};
  }

  // Where a picture of the page is taken: the named element's own box, or the
  // view, both in the page's own CSS pixels. A box is scrolled into view first,
  // so what the caller is told the picture holds is what it holds.
  function captureRegion(ref) {
    if (!ref) return {ok: true, message: 'The view', data: {x: scrollX, y: scrollY, width: innerWidth, height: innerHeight}};
    const element = resolve(ref);
    element.scrollIntoView({block: 'center', inline: 'center', behavior: 'instant'});
    const box = element.getBoundingClientRect();
    if (!(box.width > 0 && box.height > 0))
      throw failure('not_visible', `${ref} has no box on the page`);
    return {ok: true, message: `Found ${summary(element)}`,
      data: {x: box.left + scrollX, y: box.top + scrollY, width: box.width, height: box.height}};
  }

  // The colour `text` names, as the `#rrggbb` an input's value holds, or null
  // when it names none.
  function colorValue(text) {
    const written = String(text ?? '').trim().toLowerCase();
    if (HEX_COLOR.test(written)) {
      // An input holds a colour in full: `#abc` is the colour `#aabbcc`.
      const digits = written.slice(1);
      if (digits.length === 3) return '#' + [...digits].map(digit => digit + digit).join('');
      return written;
    }
    return NAMED_COLORS[written] ?? null;
  }

  // A colour input shows a colour and opens the platform's own picker when it
  // is operated; neither a picker nor a typed word reaches the page's value,
  // so the colour is set through the input's own setter and announced.
  function fillColor(ref, element, text) {
    if (element.disabled) throw failure('not_editable', `${ref} is disabled`);
    const colour = colorValue(text);
    if (!colour)
      throw failure('invalid_color',
        `${ref} is a colour input and ${quote(clean(text, MAX_VALUE))} is not a colour; give one as #rrggbb, such as #1d70b8`);
    element.scrollIntoView({block: 'center', behavior: 'instant'});
    const set = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set;
    set.call(element, colour);
    element.dispatchEvent(new InputEvent('input', {bubbles: true, composed: true, inputType: 'insertText', data: colour}));
    element.dispatchEvent(new Event('change', {bubbles: true}));
    element.blur();
    if (colorValue(element.value) !== colour)
      throw failure('rejected',
        `${ref} did not accept ${colour}; it holds ${quote(clean(element.value, MAX_VALUE))}`);
    return {ok: true, message: `Filled ${summary(element)} with ${colour}`};
  }

  // A colour input's operation opens the platform's own colour picker, which
  // the engine cannot see or close: the action would hold the tab until
  // somebody closed a window that is not part of the page. A file input's
  // chooser is held by the browser process instead, and answered by
  // fill-page-files, so only the colour picker is refused here.
  function chooserRefusal(ref, element, verb) {
    if (element.localName !== 'input' || type(element) !== 'color') return;
    throw failure('native_chooser',
      `${ref} is a colour input, and ${verb} it opens the platform's colour picker, which no agent can see or use; set the colour with fill-page instead, giving #rrggbb such as #1d70b8`);
  }

  function fill(ref, text, allowSecret) {
    const element = resolve(ref), tag = element.localName, kind = type(element);
    if (secret(element) && !allowSecret)
      throw failure('secret_field', `${ref} is a password or card field; fill it with fill_secret, which keeps the text out of logs and history`);
    const field = tag === 'textarea' || (tag === 'input' && !NOT_TEXT.has(kind));
    if (kind === 'file')
      throw failure('not_editable', `${ref} (${summary(element)}) takes files, not text; choose them with fill-page-files`);
    // A colour is set as one colour, not typed into it as text: the input
    // takes no text and refuses anything but `#rrggbb`.
    if (kind === 'color' && tag === 'input') return fillColor(ref, element, text);
    if (!field && !element.isContentEditable)
      throw failure('not_editable', `${ref} (${summary(element)}) is not a text field; click or select it instead`);
    if (element.disabled || element.readOnly) throw failure('not_editable', `${ref} is disabled or read-only`);
    element.scrollIntoView({block: 'center', behavior: 'instant'});
    element.focus({preventScroll: true});
    let inserted = false;
    if (!(tag === 'input' && SET_VALUE.has(kind))) {
      if (field) element.select();
      else {
        const range = document.createRange();
        range.selectNodeContents(element);
        const selection = getSelection();
        selection.removeAllRanges();
        selection.addRange(range);
      }
      // A region is emptied here and then typed into by keys (see `typed`),
      // so the page hears the edit it would hear from a person: every key's
      // beforeinput, and no one write of the whole text in between.
      inserted = document.execCommand(field && text ? 'insertText' : 'delete', false, text);
    }
    if (field && (!inserted || element.value !== text)) {
      // Controls without editing (dates, colours) or pages that cancel the
      // edit: set the value through the native setter and announce it.
      const prototype = tag === 'textarea' ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
      Object.getOwnPropertyDescriptor(prototype, 'value').set.call(element, text);
      element.dispatchEvent(new InputEvent('input', {bubbles: true, composed: true, inputType: 'insertText', data: text}));
    } else if (!field && !inserted) {
      element.textContent = text;
      element.dispatchEvent(new InputEvent('input', {bubbles: true, composed: true, inputType: 'insertText', data: text}));
    }
    element.dispatchEvent(new Event('change', {bubbles: true}));
    if (field && element.value !== text)
      throw failure('rejected', secret(element) ? `${ref} did not accept the text`
        : `${ref} did not accept ${quote(clean(text, MAX_VALUE))}; it holds ${quote(clean(element.value, MAX_VALUE))}`);
    if (field) return {ok: true, message: `Filled ${summary(element)}`};
    if (!text) return {ok: true, message: `Filled ${summary(element)}`};
    // The native editing command runs none of the page's beforeinput
    // listeners and writes one paragraph per line, while a person at the
    // keyboard gives the page a line at a time, each edit heard before it is
    // made. So the region is emptied here and its text typed by keys, which
    // the host does (see `typed`).
    // The region is empty and focused, with the caret where its text was:
    // what it holds now is what the typing over it writes.
    const caret = document.createRange();
    caret.selectNodeContents(element);
    caret.collapse(false);
    const selection = getSelection();
    selection.removeAllRanges();
    selection.addRange(caret);
    typedRegions.set(element, {text});
    return {ok: true, message: `Filled ${summary(element)}`, input: inputRuns(text),
      kept: true, shown: secret(element) ? '' : caretText(element)};
  }

  // The region's text as a caret writes it: a block boundary is a line break,
  // a <br> that ends a block is the empty line after it, and the trailing
  // break a region puts at its end is not a line of its own.
  function caretText(element) {
    let text = '';
    const isBlock = /^(ADDRESS|ARTICLE|ASIDE|BLOCKQUOTE|DD|DIV|DL|DT|FIELDSET|FIGCAPTION|FIGURE|FOOTER|FORM|H[1-6]|HEADER|LI|MAIN|NAV|OL|P|PRE|SECTION|TABLE|UL)$/;
    const walk = node => {
      for (const child of node.childNodes) {
        if (child.nodeType === 3) { text += child.data; continue; }
        if (child.nodeType !== 1) continue;
        const name = child.localName.toUpperCase();
        if (name === 'BR') { text += '\n'; continue; }
        const as_block = isBlock.test(name);
        if (as_block && text && !text.endsWith('\n')) text += '\n';
        walk(child);
        if (as_block && !text.endsWith('\n')) text += '\n';
      }
    };
    walk(element);
    return text.endsWith('\n') ? text.slice(0, -1) : text;
  }

  // What typing writes, line by line: the field's whole text is its own first
  // line, and text with line breaks is one line after another. A carriage
  // return is not a line break of its own.
  function inputRuns(text) {
    const unified = text.replace(/\r\n?/g, '\n');
    if (!unified.includes('\n')) return [unified];
    return unified.split('\n');
  }

  // The region a typed fill is writing, once its last key has reached the page.
  // It says whether the text is all there, and withdraws the change event that
  // was announced before the typing, so the page hears one edit and one
  // completion, as it does from a person.
  const typedRegions = new WeakMap();
  // Text as the lines a person would read, whatever blocks a page made of them.
  function linesOf(text) {
    return text.split('\n').map(line => line.replace(/\s+/g, ' ').trim()).filter(Boolean).join('\n');
  }

  function typed() {
    const element = activeTextElement();
    const region = element && typedRegions.get(element);
    typedRegions.delete(element);
    if (!region) throw failure('rejected', 'The field the text was typed into is gone');
    const shown = caretText(element);
    // An editor with a model of its own draws the text again after every edit,
    // with blocks and line breaks of its making: what it holds is the lines
    // typed, not the nodes they were typed as.
    if (linesOf(shown) !== linesOf(region.text)) {
      // Nothing typed reached a page that changed its own text: say what the
      // page says, not what the fill meant to write.
      throw failure('rejected', `${refOf(element)} did not take the whole text; it holds ${quote(clean(shown, MAX_VALUE))}`);
    }
    return {ok: true, message: `Filled ${summary(element)}`, shown};
  }

  function select(ref, values) {
    const element = resolve(ref);
    if (element.localName !== 'select')
      throw failure('not_selectable', `${ref} (${summary(element)}) is not a list box; click it, then click an option`);
    enabled(element, ref);
    const options = [...element.options], label = option => clean(option.label, MAX_NAME);
    const chosen = values.map(value => options.find(option => option.value === value) ||
      options.find(option => label(option) === clean(value, MAX_NAME)) ||
      options.find(option => label(option).toLowerCase() === clean(value, MAX_NAME).toLowerCase()));
    const missing = values.findIndex((value, index) => !chosen[index]);
    if (missing >= 0)
      throw failure('no_option', `${ref} has no option ${quote(values[missing])}; it offers ${options.slice(0, 12).map(option => quote(label(option))).join(', ')}`);
    if (!element.multiple && chosen.length > 1) throw failure('invalid_request', `${ref} accepts one option`);
    const disabled = chosen.find(option => option.disabled);
    if (disabled) throw failure('disabled', `${quote(label(disabled))} is disabled`);
    for (const option of options) option.selected = chosen.includes(option);
    element.dispatchEvent(new Event('input', {bubbles: true, composed: true}));
    element.dispatchEvent(new Event('change', {bubbles: true}));
    return {ok: true, message: `Selected ${chosen.map(option => quote(label(option))).join(', ')} in ${summary(element)}`};
  }

  function submit(ref, crossOrigin) {
    const element = resolve(ref);
    const form = element.localName === 'form' ? element : element.form || element.closest('form');
    if (!form) throw failure('no_form', `${ref} is not in a form; click its submit button or press Enter instead`);
    const submitter = (element.localName === 'button' || element.localName === 'input') &&
      (element.type === 'submit' || element.type === 'image') && element.form === form ? element : null;
    guardSubmission(form, submitter, crossOrigin);
    const invalid = [...form.elements].find(control => control.willValidate && !control.checkValidity());
    if (invalid)
      throw failure('invalid_form', `${refOf(invalid)} (${summary(invalid)}): ${clean(invalid.validationMessage, 300)}`);
    if (submitter) form.requestSubmit(submitter); else form.requestSubmit();
    return {ok: true, message: `Submitted ${summary(form)}`};
  }

  function focus(ref, key, crossOrigin) {
    const element = resolve(ref);
    enabled(element, ref);
    chooserRefusal(ref, element, 'pressing a key on');
    // Enter in a form field submits it implicitly.
    if (key === 'Enter' && element.form) guardSubmission(element.form, null, crossOrigin);
    element.scrollIntoView({block: 'center', behavior: 'instant'});
    element.focus({preventScroll: true});
    if (element.getRootNode().activeElement !== element)
      throw failure('not_focusable', `${ref} (${summary(element)}) cannot take keyboard focus`);
    return {ok: true, message: `Focused ${summary(element)}`};
  }

  // A file input's files are not writable from script: the browser process
  // sets them through the DevTools DOM domain, which is what makes the page
  // see a real picker's result. This side is the element's own rule: the ref
  // must name a file input, and each chosen file must pass its `accept`. The
  // host then answers the page with the same trusted input and change events
  // a person's picker dispatches.
  const FILE_EXTENSIONS = {pdf: ['application/pdf'], jpg: ['image/jpeg'],
    jpeg: ['image/jpeg'], png: ['image/png'], gif: ['image/gif'], webp: ['image/webp'],
    txt: ['text/plain'], csv: ['text/csv'], json: ['application/json'],
    doc: ['application/msword'], docx: ['application/vnd.openxmlformats-officedocument.wordprocessingml.document'],
    odt: ['application/vnd.oasis.opendocument.text'], rtf: ['application/rtf']};
  const MIME_EXTENSIONS = {pdf: 'pdf', jpeg: 'jpg', png: 'png', gif: 'gif', webp: 'webp',
    plain: 'txt', csv: 'csv', json: 'json'};
  function baseName(path) {
    const name = String(path).split(/[\\/]/).pop() || '';
    return name;
  }
  function mimeOf(name) {
    const dot = name.lastIndexOf('.');
    if (dot < 1 || dot === name.length - 1) return '';
    const extension = name.slice(dot + 1).toLowerCase();
    return (FILE_EXTENSIONS[extension] || [''])[0] || 'application/octet-stream';
  }
  function acceptsFile(accept, name) {
    const rules = String(accept || '').split(',').map(rule => rule.trim().toLowerCase()).filter(Boolean);
    if (!rules.length) return true;
    const lower = name.toLowerCase(), dot = lower.lastIndexOf('.');
    const extension = dot < 1 ? '' : lower.slice(dot + 1);
    const mime = mimeOf(lower);
    return rules.some(rule => rule === '*/*' ||
      (rule.startsWith('.') ? (MIME_EXTENSIONS[mime.split('/').pop()] || extension) === rule.slice(1)
        : rule.endsWith('/*') ? mime.startsWith(rule.slice(0, -1)) : mime === rule));
  }
  function fileField(ref) {
    const element = resolve(ref);
    enabled(element, ref);
    if (element.localName !== 'input' || type(element) !== 'file')
      throw failure('not_file_input', `${ref} (${summary(element)}) is not a file input; click it to open the page's own chooser`);
    return element;
  }
  // Validate one fill and answer what the outline should now show. The host
  // performs it and re-reads the line from the page.
  function fillFilesTarget(ref, paths) {
    const element = fileField(ref);
    if (!Array.isArray(paths) || !paths.length)
      throw failure('invalid_request', 'A file input needs at least one path');
    if (!element.multiple && paths.length > 1)
      throw failure('multiple_files', `${ref} takes one file; it was given ${paths.length}`);
    const accept = element.getAttribute('accept') || '';
    for (const path of paths) {
      const name = baseName(path);
      if (!name) throw failure('invalid_request', `${path} names no file`);
      if (!acceptsFile(accept, name))
        throw failure('not_accepted', `${ref} accepts ${quote(clean(accept, MAX_VALUE))}, not ${quote(name)}`);
    }
    return {ok: true, message: `Choosing ${paths.map(baseName).join(', ')} for ${summary(element)}`};
  }
  function fillFilesElement(ref) { return resolve(ref); }

  // Private text-input leases share this isolated world, not the page's agent
  // log. Preparation never focuses a field or exports its contents. A final
  // edit consumes its lease before page handlers run and uses Chromium's undo
  // path; unlike fill(), it must never fall back to setting another value.
  let textLease = null, committingTextLease = null;
  let nativeTextElement = null, nativeTextEditor = null;
  const nativeSelection = ShadowRoot.prototype.getSelection;
  function nativeTextInputElement() { return nativeTextElement; }
  function configureNativeTextEditor(editor) {
    const root = editor?.getRootNode();
    // UA roots are internal, not author 'open'/'closed' roots. Reading their
    // public mode getter hits Blink's unreachable branch; bind by native host.
    if (!nativeTextElement || nativeTextElement !== activeTextElement() ||
        !nativeTextElement.isConnected || root?.host !== nativeTextElement ||
        !['email', 'number'].includes(type(nativeTextElement)) ||
        typeof nativeSelection !== 'function') return false;
    nativeTextEditor = editor;
    return true;
  }
  const retireTextLease = () => {
    if (textLease) {
      textLease.revoked = true; clearTimeout(textLease.timer); textLease.observer.disconnect();
    }
    textLease = null; nativeTextElement = null; nativeTextEditor = null;
    if (committingTextLease) committingTextLease.revoked = true;
  };
  addEventListener('blur', retireTextLease);
  addEventListener('focusout', retireTextLease, true);
  addEventListener('input', event => {
    const owned = committingTextLease;
    if (!owned?.nativeEdit || !event.isTrusted || !composedContains(owned.element, event.target))
      retireTextLease();
  }, true);
  // Whether composition is still active is __tiloTextComposition's call.
  addEventListener('compositionstart', retireTextLease, true);

  function activeTextElement() {
    let element = document.activeElement;
    while (element?.shadowRoot?.activeElement) element = element.shadowRoot.activeElement;
    return element;
  }
  function composedParent(node) {
    if (node?.assignedSlot) return node.assignedSlot;
    if (node?.parentNode || node?.host) return node.parentNode || node.host;
    // Same-origin frame ancestors carry eligibility and mutation revocation.
    // No universal-access world or cross-origin DOM bypass is requested.
    return node?.nodeType === 9 ? node.defaultView?.frameElement : null;
  }
  function composedContains(ancestor, node) {
    for (; node; node = composedParent(node)) if (node === ancestor) return true;
    return false;
  }
  function composedReadOnly(element) {
    for (let node = element; node; node = composedParent(node)) {
      if (node.nodeType === 9 && node.defaultView.parent !== node.defaultView && !node.defaultView.frameElement)
        throw failure('not_editable', 'The original frame ancestors cannot be observed');
      if (node.nodeType === 1 && node.matches('[inert],[aria-readonly="true"],[aria-disabled="true"]')) return true;
    }
    return false;
  }
  function textSnapshot(element, allowNative = false) {
    const composition = globalThis.__tiloTextComposition;
    if (!composition || typeof composition.active !== 'function' || composition.active())
      throw failure('text_input_composition', 'Composition state is active or unavailable');
    if (!element?.isConnected || !document.hasFocus() || activeTextElement() !== element)
      throw failure('text_input_changed', 'The original text field is no longer available');
    const textSecurity = getComputedStyle(element).webkitTextSecurity;
    if (secret(element) || /(^|\s)(cc-[a-z-]+|one-time-code|current-password|new-password)(\s|$)/i.test(element.getAttribute('autocomplete') || '') ||
        (textSecurity && textSecurity !== 'none'))
      throw failure('secret_field', 'This field cannot accept deferred text input');
    if (element.matches(':disabled') || element.readOnly ||
        composedReadOnly(element))
      throw failure('not_editable', 'This field is disabled or read-only');
    const field = element.localName === 'textarea' ||
      (element.localName === 'input' && ['text', 'search', 'email', 'url', 'tel', 'number'].includes(type(element)));
    if (field) {
      if (element.value.length > 262144) throw failure('text_input_large', 'This field is too large for deferred input');
      if (!Number.isInteger(element.selectionStart) || !Number.isInteger(element.selectionEnd)) {
        if (!allowNative || !['email', 'number'].includes(type(element)))
          throw failure('not_editable', 'This field does not expose an editable selection');
        if (nativeTextElement !== element || !nativeTextEditor?.isConnected ||
            nativeTextEditor.getRootNode().host !== element) {
          nativeTextElement = element; nativeTextEditor = null;
          throw failure('text_input_native_required', 'The original field requires a native caret');
        }
        const root = nativeTextEditor.getRootNode();
        const selection = Reflect.apply(nativeSelection, root, []);
        if (!selection || selection.rangeCount !== 1 ||
            !nativeTextEditor.contains(selection.anchorNode) ||
            !nativeTextEditor.contains(selection.focusNode))
          throw failure('not_editable', 'The native field has no local editable selection');
        const raw = nativeTextEditor.textContent;
        if (raw.length > 262144)
          throw failure('text_input_large', 'This field is too large for deferred input');
        return {field: true, multiline: false, maxLength: element.maxLength, native: true, editor: nativeTextEditor, root,
          value: element.value, raw, anchor: selection.anchorNode, focus: selection.focusNode,
          start: selection.anchorOffset, end: selection.focusOffset};
      }
      return {field: true, multiline: element.localName === 'textarea', maxLength: element.maxLength, value: element.value, start: element.selectionStart,
        end: element.selectionEnd, direction: element.selectionDirection};
    }
    if (!element.isContentEditable || element.innerHTML.length > 262144)
      throw failure('not_editable', 'The focused element is not an editable text field');
    const selection = getSelection();
    if (!selection || selection.rangeCount !== 1 || !element.contains(selection.anchorNode) ||
        !element.contains(selection.focusNode))
      throw failure('not_editable', 'The focused editor has no local editable selection');
    const range = selection.getRangeAt(0);
    const editableEndpoint = node => {
      const owner = node.nodeType === Node.ELEMENT_NODE ? node : node.parentElement;
      return owner?.isContentEditable && !composedReadOnly(owner);
    };
    // An editable root can contain locked chips, widgets or readonly islands.
    // Native replacement must not delete those merely because both endpoints
    // belong to the surrounding editor. Check the complete original range too.
    if (!editableEndpoint(selection.anchorNode) || !editableEndpoint(selection.focusNode) ||
        [...element.querySelectorAll('[contenteditable="false" i], [aria-readonly="true" i], [readonly], :disabled')]
          .some(island => range.intersectsNode(island)))
      throw failure('not_editable', 'The original selection includes noneditable content');
    return {field: false, multiline: true, whitespace: getComputedStyle(element).whiteSpace, value: element.innerHTML, anchor: selection.anchorNode,
      focus: selection.focusNode, start: selection.anchorOffset, end: selection.focusOffset};
  }
  function validateNativeReplacement(lease, text) {
    const snapshot = lease.snapshot;
    if ([...text].some(character => /\p{Cc}/u.test(character) &&
        !(character === '\n' && snapshot.multiline === true)))
      throw failure('rejected', 'The original field cannot accept those control characters');
    if (!snapshot.field && text.includes('\n') &&
        !['pre', 'pre-wrap', 'break-spaces'].includes(snapshot.whitespace) &&
        (text.startsWith('\n') || text.endsWith('\n') ||
         text.split('\n').some(line => line && (/^\s/u.test(line) || /\s$/u.test(line)))))
      throw failure('rejected', 'This rich field cannot preserve literal whitespace at paragraph boundaries');
    if (snapshot.field && !snapshot.native && lease.element.maxLength >= 0 &&
        snapshot.value.length - (snapshot.end - snapshot.start) + text.length > lease.element.maxLength)
      throw failure('rejected', 'The original field cannot accept that much text');
    if (!snapshot.native) return;
    const prefixLength = (node, offset) => {
      const range = document.createRange();
      range.selectNodeContents(snapshot.editor); range.setEnd(node, offset);
      return range.toString().length;
    };
    const anchor = prefixLength(snapshot.anchor, snapshot.start);
    const focus = prefixLength(snapshot.focus, snapshot.end);
    const value = snapshot.raw.slice(0, Math.min(anchor, focus)) + text +
      snapshot.raw.slice(Math.max(anchor, focus));
    if (lease.element.maxLength >= 0 && value.length > lease.element.maxLength)
      throw failure('rejected', 'The original field cannot accept that much text');
    // Native number editing filters invalid characters and can otherwise
    // acknowledge an edit that silently deletes the selected original value.
    // Validate the complete prospective value before dispatching page handlers.
    if (type(lease.element) === 'number' &&
        (!/^-?(?:[0-9]+(?:\.[0-9]+)?|\.[0-9]+)(?:[eE][+-]?[0-9]+)?$/.test(value) ||
         !Number.isFinite(Number(value))))
      throw failure('rejected', 'The original number field requires a finite number');
  }
  function sameTextSnapshot(lease) {
    if (lease.revoked || performance.now() >= lease.expires) return false;
    if (lease.observer.takeRecords().some(record => touchesTextLease(lease, record))) return false;
    const current = textSnapshot(lease.element, lease.snapshot.native === true), original = lease.snapshot;
    return Object.keys(original).every(key => current[key] === original[key]);
  }
  function touchesTextLease(lease, record) {
    if (composedContains(lease.element, record.target)) return true;
    if (record.type !== 'childList') return composedContains(record.target, lease.element);
    // Sibling notifications/toolbars do not mutate this field. Removing or
    // reparenting an original ancestor still retires it, even after restoration.
    return [...record.addedNodes, ...record.removedNodes].some(node =>
      lease.ancestors.has(node) || composedContains(node, lease.element));
  }
  addEventListener('selectionchange', () => {
    if (textLease) {
      try { if (!sameTextSnapshot(textLease)) retireTextLease(); }
      catch (_) { retireTextLease(); }
    }
  }, true);
  function insertParagraphText(owned, text, expires) {
    // TypingCommand::InsertText splits LF into block paragraphs. In an inline
    // rich selection Chromium can add a third rendered LF and lose formatting.
    // Native line breaks stay literal; collapsed formatting commands retain the
    // original typing style and remain in the same native typing undo group.
    const toggles = ['bold', 'italic', 'underline', 'strikeThrough', 'subscript', 'superscript'];
    let formats = owned.snapshot.field ? [] : toggles.map(name =>
      [name, document.queryCommandState(name)]);
    const valueNames = ['fontName', 'fontSize', 'foreColor', 'hiliteColor'];
    let values = owned.snapshot.field ? [] : valueNames.map(name =>
      [name, document.queryCommandValue(name)]);
    const check = () => {
      if (owned.revoked || Date.now() >= expires)
        throw failure('text_input_changed', 'The original paragraph edit was interrupted');
      // Every native command can dispatch page input handlers. Recheck focus,
      // secrecy, writable state and composition before issuing another command;
      // never redirect the rest of a paragraph after a handler changes focus.
      const current = textSnapshot(owned.element, owned.snapshot.native === true);
      if (current.multiline !== owned.snapshot.multiline ||
          current.whitespace !== owned.snapshot.whitespace ||
          current.maxLength !== owned.snapshot.maxLength)
        throw failure('text_input_changed', 'The original paragraph policy changed');
    };
    const edit = (name, value) => {
      check();
      owned.nativeEdit = true;
      try {
        if (!document.execCommand(name, false, value))
          throw failure('rejected', 'The original field did not accept the paragraph');
      } finally { owned.nativeEdit = false; }
      check();
    };
    for (const [index, part] of text.split('\n').entries()) {
      if (index) edit('insertLineBreak');
      for (const [name, enabled] of formats) {
        check();
        if (document.queryCommandState(name) !== enabled) edit(name);
      }
      for (const [name, value] of values) {
        check();
        if (value && document.queryCommandValue(name) !== value) edit(name, value);
      }
      if (part) edit('insertText', part);
      if (index === 0 && part && !owned.snapshot.field) {
        // A mixed-format selection has indeterminate query state. Let native
        // replacement choose its initial typing style, then carry that collapsed
        // style across the remaining breaks rather than formatting the selection.
        formats = toggles.map(name => [name, document.queryCommandState(name)]);
        values = valueNames.map(name => [name, document.queryCommandValue(name)]);
      }
    }
  }
  function guardedTextInput(command) {
    if (!Number.isFinite(command.expires_ms) || Date.now() >= command.expires_ms)
      throw failure('text_input_expired', 'The original text request expired');
    if (textLease && performance.now() >= textLease.expires) retireTextLease();
    if (command.action === 'prepare_text_input') {
      if (textLease || committingTextLease) throw failure('busy', 'A text-input lease is already active');
      const element = activeTextElement(), snapshot = textSnapshot(element, command.native_caret === true);
      const bytes = crypto.getRandomValues(new Uint8Array(16));
      const lease = [...bytes].map(byte => byte.toString(16).padStart(2, '0')).join('');
      textLease = {element, snapshot, lease, expires: performance.now() + 180000, revoked: false, started: false};
      const owned = textLease;
      owned.observer = new MutationObserver(records => {
        if ((textLease === owned || committingTextLease === owned) &&
            records.some(record => touchesTextLease(owned, record))) retireTextLease();
      });
      const options = {subtree: true, childList: true, characterData: true, attributes: true,
        attributeFilter: ['type', 'readonly', 'disabled', 'autocomplete', 'contenteditable',
          'aria-readonly', 'aria-disabled', 'inert', 'style', 'class', 'hidden', 'slot', 'name', 'maxlength']};
      // Observe every composed root: nested shadow hosts carry eligibility too.
      const roots = new Set([document]);
      if (snapshot.native) roots.add(snapshot.root);
      owned.ancestors = new Set();
      for (let node = element; node; node = composedParent(node)) {
        owned.ancestors.add(node);
        roots.add(node.getRootNode());
      }
      for (const root of roots) owned.observer.observe(root, options);
      owned.timer = setTimeout(() => { if (textLease === owned) retireTextLease(); }, 180000);
      return {ok: true, message: 'Text input prepared', data: {lease, multiline: snapshot.multiline}, history: nativeHistory()};
    }
    if (!textLease || command.lease !== textLease.lease)
      throw failure('text_input_retired', 'This text-input lease is no longer active');
    const owned = textLease;
    if (command.action === 'cancel_text_input') {
      retireTextLease();
      return {ok: true, message: 'Text input canceled'};
    }
    let unchanged;
    try { unchanged = sameTextSnapshot(owned); }
    catch (error) { retireTextLease(); throw error; }
    if (!unchanged) {
      retireTextLease();
      throw failure('text_input_changed', 'The original text field or selection changed');
    }
    if (command.action === 'start_text_input') {
      if (owned.started) throw failure('text_input_changed', 'This original session has already started');
      owned.started = true;
      clearTimeout(owned.timer);
      owned.expires = performance.now() + 180000;
      owned.timer = setTimeout(() => { if (textLease === owned) retireTextLease(); }, 180000);
      return {ok: true, message: 'Original text session started', data: {lease: owned.lease, multiline: owned.snapshot.multiline}, history: nativeHistory()};
    }
    if (command.action === 'check_text_input') return {ok: true, message: 'Text input unchanged', data: {lease: owned.lease, multiline: owned.snapshot.multiline}, history: nativeHistory()};
    if (command.action === 'undo_text_input' || command.action === 'redo_text_input') {
      if (command.history_scope !== 'page')
        throw failure('rejected', 'Field-scoped native history is not available');
      if (owned.started) throw failure('text_input_changed', 'An active speech session owns this field');
      try {
        const verb = command.action === 'undo_text_input' ? 'undo' : 'redo';
        if (!Reflect.apply(execNativeHistory, document, [verb]))
          throw failure('rejected', 'The original field has no available history entry');
        return {ok: true, message: 'Original page history invoked'};
      } finally { retireTextLease(); }
    }
    if (typeof command.text !== 'string'  || !command.text ||
        new TextEncoder().encode(command.text).length > 4096)
      throw failure('invalid_request', 'The final text is empty or too large');
    textLease = null; clearTimeout(owned.timer); committingTextLease = owned;
    try {
      validateNativeReplacement(owned, command.text);
      const event = new InputEvent('beforeinput', {bubbles: true, composed: true,
        cancelable: true, inputType: 'insertText', data: command.text});
      if (!owned.element.dispatchEvent(event) || Date.now() >= command.expires_ms || !sameTextSnapshot(owned))
        throw failure('text_input_changed', 'The page rejected or changed the original field');
      validateNativeReplacement(owned, command.text);
      if (command.text.includes('\n')) insertParagraphText(owned, command.text, command.expires_ms);
      else if (!document.execCommand('insertText', false, command.text))
        throw failure('rejected', 'The original field did not accept the text');
      return {ok: true, message: 'Text input committed'};
    } finally { owned.observer.disconnect(); committingTextLease = null; nativeTextElement = null; nativeTextEditor = null; }
  }

  // Reader view: the document's article alone, in a calm reading layout. The
  // page's own body is set aside untouched and put back on leaving, so its
  // scripts, state and scroll position survive. The article is shown in a
  // shadow root, out of reach of the page's style sheets.
  let readerSaved = null;
  const READER_MIN_TEXT = 500;
  const READER_DROP = 'script,style,link,noscript,template,iframe,object,embed,form,button,' +
    'input,select,textarea,nav,aside,footer,dialog,[role=navigation],[role=complementary],' +
    '[role=contentinfo],[role=dialog],[aria-hidden=true],[hidden]';
  const READER_STYLE = `
    :host { all: initial; display: block; color-scheme: light dark; }
    .page { box-sizing: border-box; min-height: 100vh; padding: 56px 24px 96px;
      background: light-dark(#fbfaf8, #1b1b1d); color: light-dark(#1d1d1f, #e8e8ea);
      font: 20px/1.6 system-ui, sans-serif; }
    .article { max-width: 38em; margin: 0 auto; overflow-wrap: break-word; }
    .site { font-size: 15px; color: light-dark(#6e6e73, #98989d); margin: 0 0 8px; }
    h1 { font-size: 1.9em; line-height: 1.2; margin: 0 0 1em; }
    h2, h3, h4 { line-height: 1.3; margin: 1.6em 0 0.6em; }
    p, ul, ol, blockquote, pre, figure, table { margin: 0 0 1em; }
    img, video, picture, svg { max-width: 100%; height: auto; }
    figure { margin-inline: 0; } figcaption { font-size: 0.8em; color: light-dark(#6e6e73, #98989d); }
    a { color: light-dark(#0b57d0, #8ab4f8); }
    blockquote { margin-inline: 0; padding-inline-start: 1em; border-inline-start: 3px solid light-dark(#d2d2d7, #3a3a3c); }
    pre, code { font-family: ui-monospace, monospace; font-size: 0.85em; }
    pre { overflow-x: auto; padding: 12px; border-radius: 8px; background: light-dark(#f0f0f2, #2a2a2d); }
    table { border-collapse: collapse; display: block; overflow-x: auto; }
    td, th { padding: 4px 8px; border-bottom: 1px solid light-dark(#e5e5ea, #3a3a3c); }`;
  const paragraphText = element => {
    let total = 0;
    for (const p of element.querySelectorAll('p')) total += p.textContent.trim().length;
    return total;
  };
  // An article as old pages write it: no article and no paragraph elements,
  // the text set in the cells of a layout table, or run on in one block with
  // line breaks between its paragraphs and font tags around it. What counts
  // is the text a block holds itself, outside its links: navigation is
  // links, an essay is not.
  const READER_BLOCKS = new Set(['address', 'article', 'aside', 'blockquote', 'body', 'center', 'dd', 'details',
    'dialog', 'div', 'dl', 'dt', 'fieldset', 'figcaption', 'figure', 'footer', 'form', 'h1', 'h2', 'h3', 'h4', 'h5',
    'h6', 'header', 'hgroup', 'li', 'main', 'nav', 'ol', 'p', 'pre', 'section', 'table', 'tbody', 'td', 'tfoot',
    'th', 'thead', 'tr', 'ul']);
  const READER_ROWS = new Set(['tr', 'tbody', 'thead', 'tfoot']);
  const READER_OUTSIDE = 'nav,aside,footer,form,dialog,[hidden],[aria-hidden=true],[role=navigation],' +
    '[role=complementary],[role=contentinfo],[role=dialog]';
  const READER_MIN_RUN = 25;
  // What an old page says of its looks in attributes, which the reading
  // layout does not take over; a picture keeps its size.
  const READER_LOOKS = ['bgcolor', 'background', 'align', 'valign', 'face', 'size', 'color', 'cellpadding',
    'cellspacing', 'border', 'nowrap'];
  const READER_SIZED = new Set(['img', 'video', 'canvas', 'svg', 'picture']);
  function runningText(block) {
    let total = 0;
    const walk = node => {
      for (const child of node.childNodes) {
        if (child.nodeType === 3) total += child.data.trim().length;
        else if (child.nodeType === 1 && child.localName !== 'a' && !SKIP.has(child.localName) &&
            !READER_BLOCKS.has(child.localName)) walk(child);
      }
    };
    walk(block);
    return total;
  }
  function runningArticle() {
    if (!document.body) return null;
    const scores = new Map();
    const add = (element, text) => scores.set(element, (scores.get(element) || 0) + text);
    let visited = 0;
    for (const block of [document.body, ...document.body.querySelectorAll('*')]) {
      if (++visited > MAX_VISITED) break;
      const tag = block.localName;
      if (!READER_BLOCKS.has(tag) || READER_ROWS.has(tag) || tag === 'table') continue;
      const text = runningText(block);
      if (text < READER_MIN_RUN || block.closest(READER_OUTSIDE)) continue;
      if (block.checkVisibility && !block.checkVisibility()) continue;
      // A block that holds a whole essay as running text is the article
      // itself; shorter blocks are the article their holder gathers, and a
      // cell's holder is its table, not its row.
      if (text >= READER_MIN_TEXT) add(block, text);
      let holder = block.parentElement;
      while (holder && (READER_ROWS.has(holder.localName) || !READER_BLOCKS.has(holder.localName))) holder = holder.parentElement;
      if (holder) add(holder, text);
    }
    let best = null, bestText = 0;
    for (const [block, text] of scores) {
      // Of a block and the holder that gathers nothing but it, the block.
      if (text > bestText || (text === bestText && best && best.contains(block))) { best = block; bestText = text; }
    }
    return bestText >= READER_MIN_TEXT ? best : null;
  }
  function articleRoot() {
    let best = null, bestText = 0;
    for (const candidate of document.querySelectorAll('article, main, [role=main], [itemprop=articleBody]')) {
      const text = paragraphText(candidate);
      if (text > bestText) { best = candidate; bestText = text; }
    }
    if (bestText < READER_MIN_TEXT) {
      // No marked article: the block whose own paragraphs hold the most text.
      const blocks = new Map();
      for (const p of document.querySelectorAll('p')) {
        const length = p.textContent.trim().length;
        if (length >= 40 && p.parentElement) blocks.set(p.parentElement, (blocks.get(p.parentElement) || 0) + length);
      }
      for (const [block, text] of blocks) if (text > bestText) { best = block; bestText = text; }
    }
    return bestText >= READER_MIN_TEXT ? best : runningArticle();
  }
  // The article as the page shows it, copied along the flat tree. A clone
  // carries an element's own children and nothing of its shadow tree, where
  // a component keeps what it shows, and a cloned component draws itself
  // anew with nothing to show. So what a shadow tree or a slot shows is
  // copied in its place, without the parts its own styles hide, and a
  // component is copied as a plain box.
  function shownCopy(node, shadowed) {
    if (node.nodeType === 3) return document.createTextNode(node.data);
    if (node.nodeType !== 1 || (shadowed && !laidOut(node))) return null;
    const tag = node.localName, custom = tag.includes('-'), root = node.shadowRoot;
    const given = tag === 'slot' && node.assignedNodes ? node.assignedNodes() : [];
    const display = tag === 'slot' ? 'contents' : custom ? getComputedStyle(node).display : '';
    const copy = display === 'contents' ? document.createDocumentFragment()
      : custom ? document.createElement(display.startsWith('inline') ? 'span' : 'div') : node.cloneNode(false);
    // What says a component is not part of the article is kept on its box.
    if (custom && copy.nodeType === 1)
      for (const kept of ['hidden', 'aria-hidden', 'role']) if (node.hasAttribute(kept)) copy.setAttribute(kept, node.getAttribute(kept));
    for (const child of root ? root.childNodes : given.length ? given : node.childNodes) {
      const part = shownCopy(child, root ? true : given.length ? false : shadowed);
      if (part) copy.appendChild(part);
    }
    return copy;
  }
  function reader(enabled) {
    if (!enabled) {
      if (readerSaved) {
        document.body.replaceWith(readerSaved.body);
        window.scrollTo(0, readerSaved.scroll);
        readerSaved = null;
      }
      return {ok: true, message: 'Reader view closed', data: {reader: false}};
    }
    if (readerSaved && document.body === readerSaved.shown) return {ok: true, message: 'Reader view', data: {reader: true}};
    const root = articleRoot();
    if (!root) throw failure('no_article', 'This page has no article to read');
    let article = shownCopy(root, false);
    // A body or a table cell is the article's block only where it stood, and
    // an element with no box of its own was never one: in the reading layout
    // what it held stands in a block of its own.
    if (article.nodeType === 11 || ['body', 'td', 'th'].includes(root.localName)) {
      const block = document.createElement('div');
      block.append(...article.childNodes);
      article = block;
    }
    for (const dropped of article.querySelectorAll(READER_DROP)) dropped.remove();
    for (const element of [article, ...article.querySelectorAll('*')]) {
      element.removeAttribute('style');
      element.removeAttribute('class');
      for (const look of READER_LOOKS) element.removeAttribute(look);
      if (!READER_SIZED.has(element.localName)) { element.removeAttribute('width'); element.removeAttribute('height'); }
      // Lazily loaded images keep their address in a data attribute.
      if (element.localName === 'img' && !element.getAttribute('src')) {
        const lazy = element.getAttribute('data-src') || element.getAttribute('data-lazy-src');
        if (lazy) element.setAttribute('src', lazy);
      }
    }
    const title = clean(document.title, 300);
    const words = (article.textContent.match(/\S+/g) || []).length;
    const body = document.createElement('body');
    const host = body.appendChild(document.createElement('div'));
    const shadow = host.attachShadow({mode: 'open'});
    const style = shadow.appendChild(document.createElement('style'));
    style.textContent = READER_STYLE;
    const page = shadow.appendChild(document.createElement('div'));
    page.className = 'page';
    const column = page.appendChild(document.createElement('div'));
    column.className = 'article';
    const site = column.appendChild(document.createElement('p'));
    site.className = 'site';
    site.textContent = location.hostname.replace(/^www\./, '');
    if (title && !article.querySelector('h1')) column.appendChild(document.createElement('h1')).textContent = title;
    column.appendChild(article);
    readerSaved = {body: document.body, shown: body, scroll: window.scrollY};
    document.body.replaceWith(body);
    window.scrollTo(0, 0);
    return {ok: true, message: 'Reader view', data: {reader: true, title, words}};
  }

  // One pointer event at a viewport point, for a page that reads pointer
  // events rather than trusted input alone.
  function pointerEvent(element, kind, clientX, clientY, buttons) {
    const box = element.getBoundingClientRect();
    return element.dispatchEvent(new PointerEvent(kind, {bubbles: true, cancelable: true, composed: true,
      pointerId: 1, pointerType: 'mouse', isPrimary: true, button: kind === 'pointermove' ? -1 : 0,
      buttons: buttons === undefined ? (kind === 'pointerup' ? 0 : 1) : buttons,
      clientX, clientY, offsetX: clientX - box.left, offsetY: clientY - box.top}));
  }

  // A drag the native path could not complete, replayed for pages that react
  // to drag events only: the source's own `dragstart` handler, the drop
  // target's `dragover`, then its `drop`. Elements move exactly as a
  // person's drag makes them.  `end` is the source in viewport pixels.
  function syntheticDrag(end, target) {
    const transfer = new DataTransfer();
    const source = end.element;
    const fire = (node, kind, allowDrop = false) => {
      const event = new DragEvent(kind, {bubbles: true, cancelable: true, composed: true,
        clientX: end.x, clientY: end.y, dataTransfer: transfer});
      if (allowDrop) event.preventDefault();
      return node.dispatchEvent(event);
    };
    const started = source ? !fire(source, 'dragstart') : true;
    if (source) fire(source, 'dragenter', true);
    // A handler that calls preventDefault on dragover accepts the drop.
    fire(target, 'dragenter', true);
    const accepted = !fire(target, 'dragover', true);
    const removed = started && transferred(source);
    if (accepted) fire(target, 'drop', true);
    if (source) fire(source, 'dragend');
    return {accepted, started, removed, native: false};
  }
  // A source that left the document was moved by its page's own handler.
  function transferred(source) { return !source || !source.isConnected; }

  // What a gesture changed, as lines added to and removed from the outline.
  function gestureResult(before, message, data) {
    const after = revealDelta(before);
    const changed = after.revealing || (data && data.removed);
    return {ok: true, message: changed ? `${message}; ${after.note}` : message,
      outline: after.revealing ? after.added.map(line => '+ ' + line.trimStart()).join('\n') : undefined,
      data: {...data, changed: !!changed, added: after.added}};
  }
  // Answer a hover: what the pointer is over once it has rested there, and
  // what the hover revealed.
  function hovered(x, y, before) {
    const hit = topmost(x, y);
    if (!hit) return gestureResult(before, `Nothing at ${x}, ${y}`, {x, y, hover: null, changed: false});
    const target = targetAt(hit);
    const data = {x, y, hover: describe(hit), element: describe(hit)};
    if (target) Object.assign(data, {ref: refOf(target), target: summary(target)});
    const where = target ? `${summary(target)} [ref=${data.ref}]` : describe(hit);
    return gestureResult(before, `Hovering ${where}`, data);
  }
  // The pointer reached the element it was sent to; the page's own hover
  // styles and handlers do the rest.
  function hoveredAt(element, x, y, before) {
    // The answer is read where the pointer was rested; the engine's own last
    // point is the truth when it moved the pointer before asking.
    const at = pointerAt || {x, y};
    const hit = topmost(at.x, at.y);
    if (element && !within(hit, resolve(element)))
      return gestureResult(before, `${element} is covered by ${describe(hit)}`, {x: at.x, y: at.y, hover: describe(hit), obscured: true});
    return hovered(at.x, at.y, before);
  }

  // The source a drag starts from: the element's own viewport point and the
  // element itself, or nothing when the drag names only two points.
  function dragEnd(ref, x, y, verb) {
    const end = landing(ref, x, y, verb);
    return {x: end.x, y: end.y, element: end.element};
  }

  // Who set what a field holds. Several actors may work in one tab (agents,
  // and people typing into the page), and whoever reviews the form afterwards
  // is owed the truth about each answer: which actor's action wrote it,
  // whether it was typed straight into the page, and whether anything changed
  // it after. An action opens the window its own input lands in, and each
  // edit it causes keeps the window open a little longer; an edit outside
  // any window is direct input when the browser trusts its event, and the
  // page's own script when it does not.
  const ACTS = new Set(['locate', 'fill', 'fill_files', 'select', 'submit', 'focus', 'arm', 'fill_form',
    'key_target', 'locate_box']);
  const ACTING_MS = 1500, GESTURE_ACTING_MS = 20000;
  const provenance = new WeakMap();
  let acting = null;
  function heldValue(element) {
    const tag = element.localName, kind = type(element);
    if (tag === 'input' && (kind === 'checkbox' || kind === 'radio')) return element.checked ? 'checked' : 'unchecked';
    if (tag === 'input' && kind === 'file') return [...(element.files || [])].map(file => file.name).join(', ');
    if (tag === 'select') return [...element.selectedOptions].map(option => clean(option.label, MAX_NAME)).join(', ');
    if (tag === 'input' || tag === 'textarea')
      return secret(element) ? `<${element.value.length} characters hidden>` : element.value;
    if (element.isContentEditable) return element.innerText;
    const aria = element.getAttribute('aria-checked');
    return aria === null ? '' : aria === 'true' ? 'checked' : 'unchecked';
  }
  const noteEdit = event => {
    const path = event.composedPath ? event.composedPath() : [];
    const element = path[0] && path[0].nodeType === 1 ? path[0] : event.target;
    if (!element || element.nodeType !== 1) return;
    const now = performance.now();
    const live = acting && now < acting.until ? acting : null;
    if (live) live.until = Math.max(live.until, now + ACTING_MS);
    // An event that leaves a field holding what it held says nothing new of
    // who set it: a field tells of its change again when focus leaves it,
    // which may be during someone else's action.
    const held = heldValue(element), known = provenance.get(element);
    const how = live ? 'action' : event.isTrusted ? 'input' : 'page';
    // What an action's own edit did its answer says; anything else that
    // changed a field is told to whoever reviews the page.
    const was = lastHeld.get(element);
    lastHeld.set(element, held);
    if (how !== 'action' && was !== held) changedSoon().fields.set(element, how);
    if (!known || known.value !== held) {
      provenance.set(element, {by: live ? live.actor : '', how, seq: live ? live.seq : 0, value: held});
      if (how === 'input') typedInto(element);
    }
    // A field says it is done with when the person leaves it.
    if (event.type === 'change' && burst && burst.element === element) endBurst();
  };
  addEventListener('input', noteEdit, true);
  addEventListener('change', noteEdit, true);
  addEventListener('focusout', event => {
    const path = event.composedPath ? event.composedPath() : [];
    if (burst && burst.element === (path[0] || event.target)) endBurst();
  }, true);
  addEventListener('pagehide', () => { endBurst(); tellChanged(); });

  // What this world tells the browser process without being asked: what a
  // person typed into a field, and that the page changed a field or a form
  // itself. The function is the browser process's own, put into this world
  // alone: the page's scripts can neither see nor call it.
  const tell = note => {
    const send = globalThis.__tiloPageNote;
    if (typeof send !== 'function') return;
    try {
      // The address of this document, which the record keeps of a frame.
      note.document_url = clean(location.origin + location.pathname, 300);
      send(JSON.stringify(note));
    } catch (_) {}
  };
  // Direct input is set down in the tab's record a burst at a time: what a
  // person typed into one field, until they stop for a moment, leave the
  // field or an action begins. The entry says what the field then holds,
  // and of a secret only that it was set.
  const BURST_MS = 1000;
  let burst = null;
  function endBurst() {
    const ended = burst;
    burst = null;
    if (!ended) return;
    clearTimeout(ended.timer);
    const element = ended.element, note = {kind: 'input'};
    try {
      note.said = summary(element);
      const target = element.isConnected ? targetOf(element) : null;
      if (target) note.target = target;
      if (secret(element)) note.secret = true;
      else note.text = clean(heldValue(element), MAX_VALUE);
    } catch (_) {}
    tell(note);
  }
  function typedInto(element) {
    if (burst && burst.element !== element) endBurst();
    if (!burst) burst = {element, timer: 0};
    clearTimeout(burst.timer);
    burst.timer = setTimeout(endBurst, BURST_MS);
  }

  // The page changes its own fields and forms: a script writes a value,
  // shows a question, takes an answer away. Whoever reviews the page is
  // told when, instead of reading it again and again to find out. What the
  // inputs, selects and text areas hold is compared a few times a second (a
  // script that writes a value sends no event), a form's shape whenever
  // the document changed, and what was found is told at most once in a
  // while, however busy the page is. It says which fields and which forms,
  // never what a field holds.
  const WATCH_MS = 400, CHANGED_MS = 250, MAX_CHANGED_FIELDS = 8, WATCH_SHAPES_EVERY = 10;
  const NOT_WATCHED = new Set(['hidden', 'button', 'submit', 'reset', 'image']);
  const lastHeld = new WeakMap();
  let shapes = null, shapesDirty = true, watchTicks = 0, watching = false, changed = null;
  function changedSoon() {
    if (!changed) changed = {fields: new Map(), forms: new Map(), timer: setTimeout(tellChanged, CHANGED_MS)};
    return changed;
  }
  function tellChanged() {
    const told = changed;
    changed = null;
    if (!told) return;
    clearTimeout(told.timer);
    const note = {kind: 'changed', fields: [], forms: [...told.forms.values()]};
    try {
      if (told.fields.size) {
        const listed = new Map(findCandidates().map(candidate => [candidate.element, candidate]));
        const questions = askedOf();
        let more = 0;
        for (const [element, how] of told.fields) {
          const candidate = element.isConnected ? listed.get(element) : null;
          if (!candidate) continue;
          if (note.fields.length >= MAX_CHANGED_FIELDS) { more++; continue; }
          const field = {element: refOf(element), role: candidate.kind, name: candidate.label, how};
          const asked = questions.get(element);
          if (asked) { field.form = asked.form; field.question = asked.question; }
          note.fields.push(field);
        }
        if (more) note.more = more;
      }
    } catch (_) {}
    if (note.fields.length || note.forms.length) tell(note);
  }
  function heldNow() {
    let seen = 0;
    for (const control of document.querySelectorAll('input,select,textarea')) {
      if (++seen > MAX_FIELDS) break;
      if (control.localName === 'input' && NOT_WATCHED.has(type(control))) continue;
      const held = heldValue(control), was = lastHeld.get(control);
      lastHeld.set(control, held);
      if (was !== undefined && was !== held) changedSoon().fields.set(control, 'page');
    }
  }
  function shapesNow() {
    const now = new Map();
    for (const surface of readForms())
      now.set(surface.id, {form: surface.id, signature: surface.signature, offers: surface.offers});
    if (shapes) {
      for (const [id, shape] of now) {
        const was = shapes.get(id);
        if (!was || was.signature !== shape.signature || was.offers !== shape.offers) changedSoon().forms.set(id, shape);
      }
      for (const id of shapes.keys()) if (!now.has(id)) changedSoon().forms.set(id, {form: id, gone: true});
    }
    shapes = now;
  }
  function watchTick() {
    setTimeout(watchTick, WATCH_MS);
    try {
      heldNow();
      if (shapesDirty || ++watchTicks % WATCH_SHAPES_EVERY === 0) { shapesDirty = false; shapesNow(); }
    } catch (_) {}
  }
  // The watch starts with the first thing this world is asked, and only in
  // the world the browser process listens to.
  function watchPage() {
    if (watching || typeof globalThis.__tiloPageNote !== 'function') return;
    watching = true;
    try {
      typeWatch = new MutationObserver(records => {
        shapesDirty = true;
        // A password field the page turns into plain text is still one.
        for (const record of records)
          if (record.type === 'attributes' && record.attributeName === 'type' &&
              String(record.oldValue).toLowerCase() === 'password') wasPassword.add(record.target);
      });
      typeWatch.observe(document, {subtree: true, childList: true, attributes: true, attributeOldValue: true});
      heldNow();
      shapesNow();
      shapesDirty = false;
    } catch (_) {}
    setTimeout(watchTick, WATCH_MS);
  }
  // The question each field answers, as the page's forms name it, so a
  // review, an answer and a change speak of a field by the same name.
  function askedOf() {
    const questions = new Map();
    for (const surface of readForms()) {
      for (const asked of surface.fields) {
        if (grouped(asked))
          for (const option of asked.options) questions.set(option.control, {form: surface.id, question: asked.name, answer: option.label});
        else questions.set(asked.control, {form: surface.id, question: asked.name});
      }
    }
    return questions;
  }

  // The description that names an element wherever its document sits in its
  // history: the role and name `find` reads back, and its number among equal
  // matches when there are several. What an action did is recorded by it, so
  // the record still names the field once its document's refs are gone.
  function targetOf(element) {
    try {
      const all = findCandidates();
      const own = all.find(candidate => candidate.element === element);
      if (!own) return null;
      const same = all.filter(candidate => candidate.kind === own.kind && (!own.label || candidate.label === own.label));
      const target = {role: own.kind};
      if (own.label) target.name = own.label;
      if (same.length > 1) target.nth = same.indexOf(own) + 1;
      return target;
    } catch (_) { return null; }
  }
  // What a form asks may change with an answer. A field that appears when
  // another is answered is said in that answer, so its caller does not go
  // on with a form it has not read.
  const askedNames = () => {
    try { return readForms().flatMap(surface => surface.fields.map(field => `${surface.id}\n${field.name}`)); }
    catch (_) { return null; }
  };
  function opening(act) {
    const before = askedNames();
    const result = act();
    if (!before || !result || result.ok === false || typeof result.message !== 'string') return result;
    const known = new Set(before);
    const opened = (askedNames() || []).filter(name => !known.has(name)).map(name => name.split('\n')[1]);
    if (opened.length) result.message += `; the form now also asks ${opened.slice(0, 8).map(quote).join(', ')}`;
    return result;
  }
  const acted = (ref, result) => {
    if (result && result.ok !== false && typeof ref === 'string') {
      try {
        const element = resolve(ref), target = targetOf(element);
        if (target) result.target = target;
        // What was written into a secret is kept out of the tab's record
        // by what the field is, not by what the caller said of its text.
        if (secret(element)) result.secret = true;
      } catch (_) {}
    }
    return result;
  };
  // Where a key pressed without naming an element goes: the element that
  // has the keyboard, by the description the record keeps, and whether it
  // is a secret. Focus inside a frame is that frame's to say: the frame is
  // named, and the browser process asks its document in turn.
  function keyTarget() {
    const active = activeTextElement(), said = {ok: true, message: 'Key'};
    if (!active || active === document.body || active === document.documentElement) return said;
    if (active.localName === 'iframe' || active.localName === 'frame') { said.framed = refOf(active); return said; }
    const target = targetOf(active);
    if (target) said.target = target;
    if (secret(active)) said.secret = true;
    return said;
  }

  // Every field of the document with what it holds now and who set it: the
  // read a filled form is reviewed by. It reads values only, and asks the
  // page's own validity without running its validation events.
  const FIELD_KINDS = new Set(['textbox', 'searchbox', 'combobox', 'listbox', 'checkbox', 'radio', 'switch',
    'slider', 'spinbutton', 'file', 'color']);
  const MAX_FIELDS = 400;
  function fields() {
    const listed = [], lines = [];
    const counts = {action: 0, input: 0, page: 0, changed: 0, unset: 0};
    let cut = 0;
    const questions = askedOf();
    for (const candidate of findCandidates()) {
      const element = candidate.element;
      if (!FIELD_KINDS.has(candidate.kind) || element.localName === 'option') continue;
      if (listed.length >= MAX_FIELDS) { cut++; continue; }
      const held = heldValue(element);
      const field = {element: refOf(element), role: candidate.kind, name: candidate.label,
        value: clean(held, MAX_VALUE)};
      if (candidate.places) field.under = candidate.places.replace(/: $/, '');
      if (element.required || element.getAttribute('aria-required') === 'true') field.required = true;
      if (element.disabled || element.readOnly) field.locked = true;
      const validity = element.validity;
      if ((validity && !validity.valid) || element.getAttribute('aria-invalid') === 'true') {
        field.invalid = true;
        const problem = clean(element.validationMessage || '', MAX_VALUE);
        if (problem) field.problem = problem;
      }
      // A radio's answer is the one that is chosen: what an unchosen one was
      // set to before says nothing of the answer its group gives now.
      const set = candidate.kind === 'radio' && held === 'unchecked' ? null : provenance.get(element);
      let said = 'not set since this page was first read';
      if (set) {
        const who = set.how === 'action' ? `${set.by ? quote(set.by) : 'an unnamed actor'}, action ${set.seq}`
          : set.how === 'input' ? 'direct input into the page' : "the page's own script";
        field.set = {how: set.how};
        if (set.how === 'action') { field.set.by = set.by; field.set.seq = set.seq; }
        if (set.value !== held) { field.set.changed = true; counts.changed++; said = `changed after it was set by ${who}`; }
        else { counts[set.how]++; said = `set by ${who}`; }
      } else counts.unset++;
      const asked = questions.get(element);
      if (asked) {
        field.form = asked.form;
        field.question = asked.question;
        if (asked.answer !== undefined) field.answer = asked.answer;
      }
      listed.push(field);
      lines.push(asked ? `- ${quote(asked.question)}: ${candidate.line} (${said})` : `- ${candidate.line} (${said})`);
    }
    const where = frameId() ? ` in frame ${frameId()}` : '';
    const more = cut ? `, and ${cut} more fields past the ${MAX_FIELDS} this read carries` : '';
    const message = `${listed.length} fields${where}: ${counts.action} set by actions, ${counts.input} by direct input, ` +
      `${counts.page} by the page's script, ${counts.changed} changed after they were set, ${counts.unset} not set${more}`;
    return {ok: true, message, outline: lines.join('\n'), data: {fields: listed, counts}};
  }

  // A page's forms as typed actions. A form is what a page asks of whoever
  // fills it in: named questions, each of a kind, some with the answers it
  // offers. Read that way, an agent answers a form by its questions' names
  // in one action instead of finding and operating each control, and the
  // same answers work on the next visit, since a name outlives the refs of
  // the document it was read in. A form is known again by its signature,
  // which says what it asks and where it sends it, so a caller that learned
  // a form once can tell it has changed. What this cannot place (a secret,
  // a file, a field written by typing, a name the form no longer asks) is
  // said field by field with the ref the element's own action takes, so the
  // caller falls back to that action for those fields only.
  const MAX_FORMS = 24, MAX_FORM_FIELDS = 150, MAX_CHOICES = 80, MAX_SAID_CHOICES = 12;
  const INPUT_KINDS = new Set(['email', 'tel', 'url', 'number', 'date', 'time', 'datetime-local', 'month', 'week',
    'range', 'color']);
  const YES = new Set(['true', 'yes', 'on', 'checked', '1']), NO = new Set(['false', 'no', 'off', 'unchecked', '0']);
  const lower = text => String(text).trim().toLowerCase();
  // A form's own members, read past any control the page named like them.
  const formElements = form => [...Reflect.get(HTMLFormElement.prototype, 'elements', form)];
  function signatureOf(text) {
    let hash = 0x811c9dc5;
    for (let i = 0; i < text.length; i++) { hash ^= text.charCodeAt(i); hash = Math.imul(hash, 0x01000193); }
    return (hash >>> 0).toString(16).padStart(8, '0');
  }
  function groupName(element) {
    const group = element.closest('fieldset,[role=group],[role=radiogroup]');
    return group ? name(group, 'group') : '';
  }
  function formLabel(form) {
    const own = name(form, 'form');
    if (own) return own;
    // The heading a form stands under names it. A heading inside it names a
    // part of it, and is the form's only when nothing stands over the form.
    // A heading of something the page is telling its reader (the summary of
    // what is wrong with the answers) is not a name for the form under it.
    let last = null;
    for (const heading of document.querySelectorAll('h1,h2,h3')) {
      if (!(heading.compareDocumentPosition(form) & Node.DOCUMENT_POSITION_FOLLOWING)) break;
      if (!form.contains(heading) && !heading.closest('[role=alert],[role=alertdialog],[role=status]')) last = heading;
    }
    if (last) return clean(last.innerText, MAX_NAME);
    const inside = form.querySelector('h1,h2,h3,h4');
    return inside ? clean(inside.innerText, MAX_NAME) : '';
  }
  function formSurface(id, form, controls) {
    const fields = [], submit = [], groups = new Map();
    const shares = control => controls.some(other => other !== control && other.localName === 'input' &&
      type(other) === 'checkbox' && other.name === control.name);
    for (const control of controls) {
      if (fields.length >= MAX_FORM_FIELDS) break;
      const tag = control.localName, kind = tag === 'input' ? type(control) : tag;
      if (!['input', 'select', 'textarea', 'button'].includes(tag) || kind === 'hidden' || control.disabled) continue;
      if (control.checkVisibility && !control.checkVisibility()) continue;
      if (tag === 'button' || ['submit', 'image', 'button', 'reset'].includes(kind)) {
        const sends = tag === 'button' ? control.type === 'submit' : kind === 'submit' || kind === 'image';
        if (sends) submit.push({label: name(control, 'button') || 'Submit', control});
        continue;
      }
      // Radios of one name are one question with one answer; checkboxes of
      // one name are one question with any of its answers.
      if (kind === 'radio' || (kind === 'checkbox' && control.name && shares(control))) {
        const key = `${kind}:${control.name}`;
        let group = groups.get(key);
        if (!group) {
          group = {label: groupName(control) || clean(control.name, MAX_NAME), group: '',
            kind: kind === 'radio' ? 'choice' : 'choices', options: []};
          groups.set(key, group);
          fields.push(group);
        }
        if (group.options.length < MAX_CHOICES)
          group.options.push({label: name(control, role(control)) || clean(control.value, MAX_NAME), control});
        if (control.required) group.required = true;
        continue;
      }
      const field = {label: name(control, role(control)) || clean(control.name, MAX_NAME), group: groupName(control), control};
      if (tag === 'select') {
        field.kind = control.multiple ? 'choices' : 'choice';
        field.options = [...control.options].filter(option => option.value !== '' && !option.disabled)
          .slice(0, MAX_CHOICES).map(option => ({label: clean(option.label, MAX_NAME)}));
      } else if (kind === 'checkbox') field.kind = 'flag';
      else if (kind === 'file') field.kind = 'file';
      else if (secret(control)) field.kind = 'secret';
      else field.kind = INPUT_KINDS.has(kind) ? kind : 'text';
      if (control.required || control.getAttribute('aria-required') === 'true') field.required = true;
      if (control.readOnly) field.locked = true;
      fields.push(field);
    }
    // A question is answered by its name, so each name is one question's: a
    // label two questions share ("Day") is said with the group it is in.
    const shared = label => fields.filter(field => field.label === label).length > 1;
    const taken = new Map();
    for (const field of fields) {
      const base = (field.group && field.label && shared(field.label) ? `${field.group}: ${field.label}` : field.label) || 'field';
      const count = (taken.get(base) || 0) + 1;
      taken.set(base, count);
      field.name = count === 1 ? base : `${base} (${count})`;
    }
    let where = '';
    const method = form ? (form.getAttribute('method') || 'get').toUpperCase() : '';
    if (form) {
      try {
        const url = new URL(form.getAttribute('action') || '', location.href);
        where = url.origin === location.origin ? url.pathname : url.origin + url.pathname;
      } catch (_) {}
    }
    const signature = signatureOf([method, where,
      ...fields.map(field => `${field.name}:${field.kind}:${field.required ? 1 : 0}`)].join('|'));
    // The answers its choices offer, as a second value beside the
    // signature: a list of dates that is another list tomorrow is the same
    // form asking the same questions, offering other answers.
    const offers = signatureOf(fields.filter(field => field.options)
      .map(field => `${field.name}=${field.options.map(option => option.label).join('\u001f')}`).join('|'));
    return {id, form, label: form ? formLabel(form) : '', method, where, signature, offers, fields, submit};
  }
  function readForms() {
    const read = [...document.querySelectorAll('form')].slice(0, MAX_FORMS)
      .map((form, index) => formSurface(`form${index + 1}`, form, formElements(form)));
    const loose = [...document.querySelectorAll('input,select,textarea,button')].filter(control => !control.form);
    if (loose.some(control => control.localName !== 'button')) read.push(formSurface('page', null, loose));
    return read.filter(surface => surface.fields.length);
  }
  const grouped = field => !field.control;
  function fieldValue(field) {
    if (grouped(field)) return field.options.filter(option => option.control.checked).map(option => option.label);
    if (field.kind === 'choice' || field.kind === 'choices')
      return [...field.control.selectedOptions].filter(option => option.value !== '').map(option => clean(option.label, MAX_NAME));
    if (field.kind === 'flag') return field.control.checked;
    if (field.kind === 'secret') return field.control.value ? `<${field.control.value.length} characters hidden>` : '';
    if (field.kind === 'file') return [...(field.control.files || [])].map(file => clean(file.name, MAX_NAME));
    return clean(field.control.value, MAX_VALUE);
  }
  const unanswered = value => Array.isArray(value) ? !value.length : value === '' || value === false;
  function forms() {
    const lines = [], listed = [];
    let asked = 0;
    for (const surface of readForms()) {
      const head = [surface.id];
      if (surface.label) head.push(quote(surface.label));
      if (surface.method) head.push(`(${surface.method} ${surface.where})`);
      head.push(`signature=${surface.signature}`, `offers=${surface.offers}`);
      lines.push(`- ${head.join(' ')}`);
      const out = {form: surface.id, label: surface.label, method: surface.method, action: surface.where,
        signature: surface.signature, offers: surface.offers, fields: [], submit: surface.submit.map(button => button.label)};
      for (const field of surface.fields) {
        const value = fieldValue(field), entry = {name: field.name, kind: field.kind};
        const said = [field.kind];
        if (field.options) {
          entry.choices = field.options.map(option => option.label);
          // An answer that is a control of its own is said with the ref its
          // own action takes; a select's options have none.
          const some = field.options.slice(0, MAX_SAID_CHOICES)
            .map(option => option.control ? `${quote(option.label)} [ref=${refOf(option.control)}]` : quote(option.label)).join(', ');
          const more = entry.choices.length > MAX_SAID_CHOICES ? `, … ${entry.choices.length - MAX_SAID_CHOICES} more` : '';
          said[0] = `${field.kind === 'choice' ? 'one of' : 'any of'} ${some}${more}`;
        }
        if (field.kind === 'flag') said[0] = 'yes or no';
        if (field.required) { entry.required = true; said.push('required'); }
        if (field.locked) { entry.locked = true; said.push('read-only'); }
        if (!unanswered(value)) { entry.value = value; said.push(`now ${quote(Array.isArray(value) ? value.join(', ') : value === true ? 'yes' : value)}`); }
        if (grouped(field)) entry.elements = field.options.map(option => refOf(option.control));
        else { entry.element = refOf(field.control); said.push(`[ref=${entry.element}]`); }
        out.fields.push(entry);
        lines.push(`  - ${quote(field.name)}: ${said.join(', ')}`);
        asked++;
      }
      for (const button of surface.submit) lines.push(`  - submit ${quote(button.label)}`);
      listed.push(out);
    }
    const message = listed.length ? `${listed.length} ${listed.length === 1 ? 'form asks' : 'forms ask'} ${asked} questions`
      : 'This page has no form';
    return {ok: true, message, outline: lines.join('\n'), data: {forms: listed}};
  }
  // One answer placed in the field it names, as the element's own action
  // would place it; what it left the field holding is its return.
  function placeAnswer(field, value) {
    const ref = grouped(field) ? '' : refOf(field.control);
    if (field.kind === 'secret')
      throw failure('secret_field', `${quote(field.name)} is a password or card field; fill ${ref} with fill_secret, which keeps the text out of logs and history`);
    if (field.kind === 'file') throw failure('needs_files', `${quote(field.name)} takes files; choose them for ${ref} with fill_files`);
    if (field.locked) throw failure('not_editable', `${quote(field.name)} is read-only`);
    if (grouped(field)) {
      const wanted = field.kind === 'choices' && Array.isArray(value) ? value : [value];
      const chosen = wanted.map(one => {
        const option = field.options.find(option => option.label === String(one)) ||
          field.options.find(option => lower(option.label) === lower(one));
        if (!option)
          throw failure('no_option', `${quote(field.name)} has no answer ${quote(String(one))}; it offers ${field.options.map(option => quote(option.label)).join(', ')}`);
        return option;
      });
      for (const option of field.options) {
        const want = chosen.includes(option);
        if (field.kind === 'choice' ? want && !option.control.checked : option.control.checked !== want) option.control.click();
      }
      const missed = chosen.find(option => !option.control.checked);
      if (missed) throw failure('rejected', `The page did not take ${quote(missed.label)} for ${quote(field.name)}`);
      return chosen.map(option => option.label).join(', ');
    }
    if (field.kind === 'choice' || field.kind === 'choices') {
      select(ref, (Array.isArray(value) ? value : [value]).map(String));
      return [...field.control.selectedOptions].map(option => clean(option.label, MAX_NAME)).join(', ');
    }
    if (field.kind === 'flag') {
      const want = typeof value === 'boolean' ? value : YES.has(lower(value)) ? true : NO.has(lower(value)) ? false : null;
      if (want === null) throw failure('invalid_request', `${quote(field.name)} is answered yes or no`);
      if (field.control.checked !== want) field.control.click();
      if (field.control.checked !== want) throw failure('rejected', `The page did not take the answer to ${quote(field.name)}`);
      return want ? 'yes' : 'no';
    }
    if (typeof value !== 'string' && typeof value !== 'number') throw failure('invalid_request', `${quote(field.name)} takes text`);
    const filled = fill(ref, String(value), false);
    if (filled && Array.isArray(filled.input))
      throw failure('needs_typing', `${quote(field.name)} is written by typing; fill ${ref} on its own`);
    return clean(String(value), MAX_VALUE);
  }
  function fillForm(id, answers, submitWith, crossOrigin) {
    const read = readForms();
    const surface = read.find(candidate => candidate.id === id) || read.find(candidate => candidate.signature === id);
    if (!surface) {
      // Two refusals: the page asks nothing at all, or it asks, but not as
      // the form the caller named did: that form changed, or was another
      // page's.
      if (!read.length) throw failure('no_form', 'This page has no form');
      const has = read.map(candidate => candidate.label ? `${candidate.id} ${quote(candidate.label)}` : candidate.id).join(', ');
      throw failure('form_changed', `This page has no form ${quote(String(id))}: it has ${has}. A form named by a signature it no longer has has changed; read the page's forms again`);
    }
    const before = new Set(surface.fields.map(field => field.name));
    const results = [], set = [];
    // Answers are placed in the order the form asks its questions, whatever
    // order they were given in, and the form is read again after each: an
    // answer may open the question another answer is for.
    const waiting = new Map(Object.entries(answers || {}));
    const named = (fields, asked) => {
      const alike = fields.filter(field => lower(field.name) === lower(asked));
      return fields.find(field => field.name === asked) || (alike.length === 1 ? alike[0] : null);
    };
    let asks = surface;
    while (waiting.size) {
      const answered = new Map();
      for (const asked of waiting.keys()) {
        const field = named(asks.fields, asked);
        if (field && !answered.has(field)) answered.set(field, asked);
      }
      const field = asks.fields.find(candidate => answered.has(candidate));
      if (!field) break;
      const asked = answered.get(field), value = waiting.get(asked);
      waiting.delete(asked);
      try {
        set.push({name: field.name, value: placeAnswer(field, value)});
        results.push({name: field.name, ok: true});
      } catch (error) {
        const result = {name: field.name, ok: false, code: error && error.code || 'script',
          message: clean(error && error.message || error, 300)};
        if (!grouped(field)) result.element = refOf(field.control);
        results.push(result);
      }
      if (waiting.size) asks = readForms().find(candidate => candidate.id === surface.id) || asks;
    }
    for (const asked of waiting.keys())
      results.push({name: asked, ok: false, code: 'no_field', message: `The form asks nothing named ${quote(asked)}`});
    const placed = set.length, missed = results.filter(result => !result.ok);
    // What the form asks now: an answer may have opened a question the form
    // did not ask before, and a required one left empty holds a submit back.
    const after = readForms().find(candidate => candidate.id === surface.id);
    const opened = after ? after.fields.filter(field => !before.has(field.name)).map(field => field.name) : [];
    const empty = after ? after.fields.filter(field => field.required && unanswered(fieldValue(field))).map(field => field.name) : [];
    let submitted = false, held = '';
    if (submitWith !== undefined && submitWith !== null && submitWith !== false) {
      try {
        if (missed.length) held = 'not sent, since not every answer was placed';
        else if (!surface.form) held = 'not sent: these fields are in no form, so click the button that sends them';
        else {
          const button = typeof submitWith === 'string'
            ? surface.submit.find(candidate => candidate.label === submitWith) ||
              surface.submit.find(candidate => lower(candidate.label) === lower(submitWith))
            : surface.submit[0];
          if (typeof submitWith === 'string' && !button)
            throw failure('no_submit', `the form has no button ${quote(submitWith)}; it has ${surface.submit.map(candidate => quote(candidate.label)).join(', ') || 'none'}`);
          guardSubmission(surface.form, button ? button.control : null, crossOrigin);
          const invalid = formElements(surface.form).find(control => control.willValidate && !control.validity.valid);
          if (invalid) held = `not sent: ${summary(invalid)} [ref=${refOf(invalid)}] ${clean(invalid.validationMessage, 200)}`;
          else {
            const send = HTMLFormElement.prototype.requestSubmit;
            if (button) Reflect.apply(send, surface.form, [button.control]); else Reflect.apply(send, surface.form, []);
            submitted = true;
          }
        }
      } catch (error) {
        held = `not sent: ${clean(error && error.message || error, 300)}`;
      }
    }
    const said = [`Answered ${placed} of ${results.length} questions of ${surface.id}${surface.label ? ' ' + quote(surface.label) : ''}`];
    if (missed.length) said.push(`not placed: ${missed.map(result => `${quote(result.name)} (${result.message})`).join('; ')}`);
    if (submitted) said.push('sent'); else if (held) said.push(held);
    if (opened.length) said.push(`the form now also asks ${opened.map(quote).join(', ')}`);
    if (!submitted && empty.length) said.push(`still required: ${empty.map(quote).join(', ')}`);
    // What the form is now, by the signature a read of it would give: its
    // caller knows at once that the form it answered asks something else.
    if (after && after.signature !== surface.signature && !opened.length)
      said.push(`the form asks something else now, signature ${after.signature}`);
    if (!placed && !submitted) throw failure(missed[0] ? missed[0].code : 'invalid_request', said.join('; '));
    const data = {form: surface.id, signature: surface.signature, results, set, submitted, now_asks: opened, unanswered: empty};
    if (after) data.became = {signature: after.signature, offers: after.offers};
    return {ok: true, message: said.join('; '), data};
  }

  const page = {
    nativeTextInputElement, configureNativeTextEditor, fillFilesElement, fence, typed, frameNode,
    // One command of the browser process, answered as its action answers.
    answer(command) {
      try {
        if (!doc) doc = command.doc;
        frameAddress = typeof command.frame === 'string' ? command.frame : '';
        // Who asks, when the caller says: its own read baseline, and the name
        // the edits its action causes are set down under.
        if (typeof command.actor === 'string' || typeof command.seq === 'number' ||
            command.action === 'outline' || command.action === 'changes')
          actorId = typeof command.actor === 'string' ? command.actor : '';
        watchPage();
        if (typeof command.seq === 'number') {
          const begins = ACTS.has(command.action) || command.action === 'gesture_begin' || command.action === 'acting';
          // What a person was typing when an action begins is set down
          // before the action's own edits are.
          if (begins && !(acting && acting.seq === command.seq)) endBurst();
          if (ACTS.has(command.action))
            acting = {actor: actorId, seq: command.seq, until: performance.now() + ACTING_MS};
          else if (command.action === 'gesture_begin' || command.action === 'acting')
            acting = {actor: actorId, seq: command.seq, until: performance.now() + GESTURE_ACTING_MS};
          else if ((command.action === 'gesture_end' || command.action === 'changes') && acting && acting.seq === command.seq)
            acting.until = Math.min(acting.until, performance.now() + ACTING_MS);
        }
        // A gesture made of pointer steps says it has begun, so the edits
        // its steps cause are its own.
        if (command.action === 'acting') return {ok: true, message: 'Acting'};
        // A gesture that spans two engine steps: read what is about to
        // change, let the pointer do its work, then answer with the change.
        if (command.action === 'gesture_begin') {
          gestureBefore = {lines: outlineLines().lines, reveals: new Map()};
          gestureMirror = command.mirror === true;
          gestureKind = command.kind || null;
          // The ends the engine's pointer will travel between, resolved before
          // it moves: an element's own point, or the points the caller gave.
          const end = (ref, x, y, verb) => {
            const at = landing(ref, x, y, verb);
            return {x: at.x, y: at.y, element: at.element};
          };
          if (command.kind === 'hover' || command.kind === 'drag' || command.kind === 'path') {
            // A path names one element and draws in its own box, so only a
            // hover or a drag has two ends to resolve here.
            const from = command.kind === 'path'
              ? {x: 0, y: 0, element: resolve(command.element)}
              : end(command.element, command.x, command.y, command.kind === 'hover' ? 'hover' : command.kind);
            const answer = {ok: true, gesture_page: true, x: from.x, y: from.y,
              from: from.element ? refOf(from.element) : undefined};
            if (command.kind === 'drag') {
              const to = end(command.to, command.to_x, command.to_y, 'drop on');
              answer.to_x = to.x; answer.to_y = to.y;
              answer.to_target = to.element ? refOf(to.element) : undefined;
            }
            return answer;
          }
        }
        if (command.action === 'pointer_rest')
          return {ok: true, rested: !!restPointer(command.nowhere ? null : command.x, command.y, gestureMirror)};
        if (command.action === 'gesture_end') {
          const before = gestureBefore || {lines: [], reveals: new Map()};
          gestureBefore = null;
          gestureKind = null;
          // The button came up; nothing reveals more, so nothing stays applied.
          restPointer(null);
          pointerAt = null;
          if (command.kind === 'hover') return hoveredAt(command.element, command.x, command.y, before);
          // A drag answers through its replay; only a hover or a path comes back
          // here, with the one end the pointer rested at.
          if (command.kind === 'path')
            return gestureResult(before, command.message
              || `Drew ${command.drew ? command.drew + ' points' : 'the pointer path'} on the page`, {drew: command.drew});
          return gestureResult(before, command.message || 'Dragged', {});
        }
        switch (command.action) {
          case 'prepare_text_input': case 'start_text_input': case 'check_text_input': case 'commit_text_input': case 'cancel_text_input': case 'undo_text_input': case 'redo_text_input':
            return guardedTextInput(command);
          case 'outline': return outline(command.from, command.most);
          // The ref of the element a stable description names, read from the
          // page as it is now.
          case 'find': return find(command);
          // The engine mirrors a hover through the page's CSS only when that
          // CSS opens something on hover; otherwise the browser's own pointer
          // is enough and nothing is applied.
          case 'need_styles': { const n = markHoverTargets(); return {ok: true, need_styles: n > 0, doc: typeof doc === 'number' ? doc : command.doc}; }
          case 'changes': return changes();
          case 'fields': return fields();
          case 'forms': return forms();
          case 'search': return search(command.text);
          case 'fill_form': arm('script'); return fillForm(command.form, command.answers, command.submit, command.cross_origin);
          // A turn of the wheel: where it turns, and what is left of the
          // distance once the page has given what it has.
          case 'scroll_target': return scrollTarget(command);
          case 'scroll_left': return scrollLeft();
          case 'at': return frameId() && command.inside !== true ? atInFrame()
            : at(command.x, command.y, command.inside === true ? {x: command.page_x, y: command.page_y} : null);
          case 'frame_offset': return frameOffset(command.element, command.anchored);
          case 'frame_fence': return fence();
          case 'settle': return settle(command.quiet_ms, command.timeout_ms);
          case 'locate': arm('pointer'); return acted(command.element, locate(command.element, command.cross_origin));
          case 'locate_box': return locateBox(command.element);
          case 'capture': return captureRegion(command.element);
          case 'fill': { arm('script'); return acted(command.element, opening(() => fill(command.element, command.text, command.secret))); }
          case 'fill_files': arm('script'); return acted(command.element, fillFilesTarget(command.element, command.paths));
          case 'select': arm('script'); return acted(command.element, opening(() => select(command.element, command.values)));
          case 'submit': arm('script'); return acted(command.element, submit(command.element, command.cross_origin));
          case 'focus': {
            watch = null;
            const focused = focus(command.element, command.key, command.cross_origin);
            arm('key');
            return acted(command.element, focused);
          }
          case 'arm': return arm('key');
          case 'key_target': return keyTarget();
          case 'reader': return reader(command.enabled === true);
          // The engine moves the pointer to a point computed here.
          case 'hover': {
            const end = landing(command.element, command.x, command.y, 'hover');
            return {ok: true, x: end.x, y: end.y};
          }
          // The engine presses at one end, moves and releases at the other.
          case 'drag': {
            const from = dragEnd(command.element, command.x, command.y, 'drag');
            const to = dragEnd(command.to, command.to_x, command.to_y, 'drop on');
            return {ok: true, x: from.x, y: from.y, to_x: to.x, to_y: to.y,
              from: from.element ? refOf(from.element) : undefined, to_target: to.element ? refOf(to.element) : undefined};
          }
          // The drag's own drop, replayed as the page's own drag and drop
          // events between the same two ends.
          case 'drag_fallback': {
            const from = dragEnd(command.element, command.x, command.y, 'drag');
            const to = dragEnd(command.to, command.to_x, command.to_y, 'drop on');
            const target = topmost(to.x, to.y) || document.body;
            // Nothing native moved the source: the gesture's release is the
            // only drop the page has seen.
            const moved = false;
            const report = moved ? {accepted: true, started: true, removed: true, replayed: false}
              : syntheticDrag({x: to.x, y: to.y, element: from.element}, target);

            const before = gestureBefore || {lines: [], reveals: new Map()};
            gestureBefore = null;
            const said = `Dragged ${from.element ? describe(from.element) : 'the pointer'} to ${describe(target)}`;
            const data = {...report, from: describe(from.element), to: describe(target), native: false};
            // A page that received no drop did not reorder; say so instead of
            // reporting a drag it never took.
            if (!report.accepted)
              return failure('no_drop', `${said}, which received no drop; the page takes drags only from its own drag events`);
            return gestureResult(before, said, data);
          }
          // Where the engine's pointer is; the page's hover styles follow it.
          case 'pointer_rest': return {ok: true, rested: false};
          // A contact path for the engine's pointer, in viewport pixels.
          case 'path': {
            const {points} = contactPath(command.element, command.points || []);
            return {ok: true, points, heard: heardMove.n, message: `Drawing ${points.length} points on ${command.element}`};
          }
          case 'pointer_heard': {
            const wait = Math.max(0, Math.min(Number(command.wait_ms) || 0, 1000));
            return pointerHeard(Number(command.x), Number(command.y), Number(command.after) || 0, wait);
          }
          // A contact path the engine cannot send, replayed as pointer events.
          case 'path_fallback': {
            const element = resolve(command.element);
            const box = element.getBoundingClientRect();
            for (const [x, y] of command.points || []) pointerEvent(element, 'pointermove', box.left + x, box.top + y);
            return {message: `Drew ${(command.points || []).length} points on ${command.element}`};
          }
        }
        return failure('invalid_request', 'Unknown page action');
      } catch (error) {
        if (error && error.ok === false) return error;
        return failure('script', clean(error && error.message || error, 300));
      }
    },
  };
  // Every answer says which document gave it: the number its refs carry and,
  // from a frame, the address of the frame's own document, which the tab's
  // record keeps beside the frame's number.
  const located = result => {
    if (result && typeof result === 'object') {
      result.doc = doc;
      if (frameAddress) result.document_url = clean(location.origin + location.pathname, 300);
    }
    return result;
  };
  page.run = command => {
    const result = page.answer(command);
    return result && typeof result.then === 'function' ? result.then(located) : located(result);
  };
  return page;
})();
