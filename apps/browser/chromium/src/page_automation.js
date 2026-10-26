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
  const MAX_TEXT = 200, MAX_NAME = 120, MAX_VALUE = 200, MAX_OPTIONS = 30;
  const INPUT_ROLES = {button: 'button', submit: 'button', reset: 'button', image: 'button',
    file: 'file', color: 'button', checkbox: 'checkbox', radio: 'radio', range: 'slider',
    number: 'spinbutton', search: 'searchbox'};
  const INTERACTIVE = new Set(['link', 'button', 'checkbox', 'radio', 'switch', 'textbox',
    'searchbox', 'combobox', 'listbox', 'option', 'slider', 'spinbutton', 'tab', 'menuitem',
    'menuitemcheckbox', 'menuitemradio', 'treeitem', 'clickable', 'file', 'hint', 'draggable',
    'canvas', 'hoverable']);
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
  const SET_VALUE = new Set(['date', 'time', 'datetime-local', 'month', 'week', 'color', 'range']);

  const ids = new WeakMap(), refs = new Map();
  let next = 0, doc = 0;
  // The outlines this tab's agent last saw, per frame id ('' for the main
  // document), for `changes`.
  const lastLinesByFrame = new Map();
  const getLastLines = () => lastLinesByFrame.get(frameId()) ?? null;
  const setLastLines = lines => lastLinesByFrame.set(frameId(), lines);
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
  const MAX_SCROLL_STILL = 4;
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
  const room = box => Math.max(scrollable(box) - scrollYOffset(box), 0);
  // How far down a box is scrolled. The page's own box needs no special case:
  // the window holds the document's scrolling element and the document, and
  // asking either of them for what it holds never moves the page.
  const scrollYOffset = box => box.scrollTop;
  // How far the last scroll moved the box the wheel turned over, taken by the
  // read that answers it: null when no scroll is being answered.
  let scrollMoved = null;
  const takeScrollMoved = () => { const moved = scrollMoved; scrollMoved = null; return moved; };
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
      const from = scrollYOffset(box);
      const left = delta > 0 ? Math.min(delta, room(box)) : Math.max(delta, -from);
      scrolling = {box, x: point.x, y: point.y, rest: left, travelled: Math.sign(left), start: from, still: 0};
      return {ok: true, x: point.x, y: point.y, left};
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
    const from = scrollYOffset(scroller);
    let left = 0, travelled = 0;
    if (command.to === 'top' || command.to === 'bottom') {
      // An end of the page is a whole page away, more than any distance the
      // wheel is allowed: the wheel keeps turning until the page stops
      // moving, so a page that loads more on the way down goes on, and the
      // turns end only when a turn gives nothing.
      travelled = command.to === 'bottom' ? 1 : -1;
      left = travelled * WHEEL_TO_THE_END;
    } else if (typeof command.by === 'number') {
      // A distance is the whole distance asked for: how far it actually moves
      // the page is the wheel's own business, and what the box cannot take it
      // keeps looking for as the page under it grows.
      travelled = Math.sign(command.by);
      left = command.by;
    }
    // A box already at the end the wheel would take it to leaves the wheel
    // nothing to do, and the answer says so at once.
    if (left * travelled <= 0) { scrollMoved = 0; return {ok: true, x: point.x, y: point.y, left: 0}; }
    scrolling = {box: scroller, x: point.x, y: point.y, rest: left, travelled, start: from, still: 0};
    return {ok: true, x: point.x, y: point.y, left};
  }
  // What the box the wheel turns over has left of the distance after a turn:
  // a list that renders more as it scrolls says so here, and the wheel turns
  // again, as a hand's next turn would.
  function scrollLeft() {
    if (!scrolling) return {ok: true, x: innerWidth / 2, y: innerHeight / 2, left: 0};
    const box = scrolling.box, now = scrollYOffset(box), moved = Math.abs(now - scrolling.start);
    scrollMoved = (scrollMoved || 0) + Math.round(now - scrolling.start);
    scrolling.rest -= scrolling.travelled > 0 ? Math.min(moved || now, scrolling.rest)
      : -Math.min(moved || -now, -scrolling.rest);
    scrolling.start = now;
    // A turn that moved nothing may have been given nothing by the page yet —
    // a frame still settling, a box whose content arrives later — so it is
    // tried again, a few times, before the wheel says the page has stopped.
    // The turns the wheel is allowed are what ends a page that never moves.
    if (!moved && ++scrolling.still <= MAX_SCROLL_STILL)
      return {ok: true, x: scrolling.x, y: scrolling.y, left: scrolling.travelled > 0 ? Math.min(scrolling.rest, room(box))
        : -Math.max(-scrolling.rest - Math.max(now, 0), 0)};
    const left = !moved ? 0 : scrolling.travelled > 0 ? Math.min(scrolling.rest, room(box))
      : -Math.max(-scrolling.rest - Math.max(now, 0), 0);
    return {ok: true, x: scrolling.x, y: scrolling.y, left: Math.round(left)};
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

  // Frame 1 is the first frame in this document, and a frame of a frame is
  // addressed `1.2`: the number is the frame's place in the document now,
  // counted the way an action counts it, so the first frame a page shows is
  // frame 1 however many frames the page has had, and a number printed is a
  // number the actions take.
  const framesByNumber = new Map();
  function frameNumber(element) {
    if (!element.isConnected) return 1;
    if (framesByNumber.size >= 4096) framesByNumber.clear();
    const number = [...document.querySelectorAll('iframe,frame')].indexOf(element) + 1;
    framesByNumber.set(number, new WeakRef(element));
    return number;
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
        .map(part => part.getAttribute('alt') || part.getAttribute('aria-label') || part.textContent).join(' '), MAX_NAME);
      if (text) return text;
    }
    return clean(element.title, MAX_NAME);
  }
  const secret = element => type(element) === 'password' || /(^|\s)cc-/.test(element.getAttribute('autocomplete') || '');
  function checked(element) {
    const aria = element.getAttribute('aria-checked');
    if (aria === 'mixed' || element.indeterminate) return 'mixed';
    return (element.localName === 'input' ? element.checked : aria === 'true') ? 'checked' : 'unchecked';
  }
  function details(element, kind) {
    const out = [], tag = element.localName;
    if (kind === 'heading') out.push(`level=${/^h[1-6]$/.test(tag) ? tag[1] : element.getAttribute('aria-level') || 2}`);
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
  function markHoverTargets() {
    hoverTargets = new Set();
    let seen = 0;
    for (const sheet of document.styleSheets) {
      let rules = null;
      try { rules = sheet.cssRules; } catch (error) { rules = null; }
      if (!rules) continue;
      for (const rule of rules) {
        if (seen++ > MAX_HOVER_RULES) return hoverTargets.size;
        const trigger = hoverTrigger(rule.selectorText);
        if (!trigger) continue;
        // The marker is what a resting pointer puts on the element; it is
        // borrowed here to read which elements a hover would land on.
        const bare = trigger.split('.' + MARKER).join('');
        if (!bare) continue;
        for (const element of document.querySelectorAll('*')) {
          let beside = false;
          try { beside = element.matches(bare); } catch (error) { beside = false; }
          if (!beside) continue;
          const had = element.classList.contains(MARKER);
          if (!had) element.classList.add(MARKER);
          let wanted = false;
          try { wanted = element.matches(trigger); } catch (error) { wanted = false; }
          if (!had) element.classList.remove(MARKER);
          if (!wanted) continue;
          if (hoverTargets.size >= MAX_HOVER_TARGETS) return hoverTargets.size;
          hoverTargets.add(element);
        }
      }
    }
    return hoverTargets.size;
  }
  // The element on which a hover opens something: itself when a rule names
  // it, an ancestor when the rule hovers one, else the parent whose hovered
  // trigger opened a child that starts hidden. Words inside a trigger are the
  // trigger's own, so a hover over them opens what the trigger opens.
  function hoverReveals(element) {
    if (!hoverTargets) markHoverTargets();
    let depth = 0;
    for (let node = element; node && depth++ < 16; node = node.parentElement) {
      if (hoverTargets.has(node)) return true;
      for (const child of node.children) {
        if (hoverTargets.has(child) && !child.checkVisibility?.()) return true;
      }
    }
    return false;
  }

  // How the caller addresses this frame's document: the dotted ids its
  // ancestors' outlines printed, '' for the main document itself.
  let frameAddress = '';
  const frameId = () => (window.frameElement ? frameAddress : '');

  // The frame an action named, by the number the page prints for it: the frame
  // element this document holds it in, or the frame element the page numbered.
  function numberedFrame(number) {
    if (!/^\d+$/.test(number || '')) return null;
    // A frame this document may not read is still placed from the frame
    // element that holds it, which this document does see.
    const held = holdingFrame(Number(number));
    return held || framesByNumber.get(String(Number(number)))?.deref?.() || null;
  }

  // The frame element this document numbers `number`: the one holding this
  // document itself, or the one holding the frame this document numbers. The
  // walk descends only into frames this document may read: a frame out of
  // reach is placed from the mark-up around it, which is never out of reach.
  function holdingFrame(number) {
    if (document.frameElement) return document.frameElement;
    const walk = root => {
      for (const element of [...root.querySelectorAll('iframe,frame')].slice(0, 4096)) {
        let child = null;
        try { child = element.contentDocument; } catch { child = null; }
        if (!child) continue;
        const inner = child.querySelectorAll?.('iframe,frame') || [];
        if (inner.length < number) return element;
        const found = walk(child);
        if (found) return found;
      }
      return null;
    };
    return walk(document);
  }

  // Where a frame this document lists sits in it now, in this document's
  // coordinates with its scrolling: a point read in the frame's own document
  // moves by this to become a point in this one.
  function frameOffset(number, anchored) {
    if (!/^\d+$/.test(number || '')) throw failure('invalid_request', 'A frame of this document is named by its number');
    // Measuring a frame's place can be the first thing this document is asked,
    // so the numbering is read from the document rather than from an outline
    // that may never have run here.
    const element = numberedFrame(number);
    if (!element)
      throw failure('no_frame', `Frame ${number} has left the page; read the page again`);
    // The click is a pointer event in the top document, at a point the caller
    // never saw: the frame is brought into view so that point is reachable.
    // A gesture stands where its step is, though: the box the step resolves to
    // is read in the frame's own document against coordinates the frame's
    // place then has to match, so it is measured where the page put it.
    if (anchored !== true) element.scrollIntoView({block: 'nearest', inline: 'nearest'});
    const rect = element.localName === 'frame'
      ? {x: Number(element.getAttribute('x')) || 0, y: Number(element.getAttribute('y')) || 0}
      // An iframe is a replaced element: its content view starts inside its
      // border, and a point placed in it is that view's own.
      : (() => { const box = element.getBoundingClientRect(), style = getComputedStyle(element);
        return {x: box.x + (parseFloat(style.borderLeftWidth) || 0), y: box.y + (parseFloat(style.borderTopWidth) || 0)}; })();
    // A point read in the frame's document is a point in the frame's viewport,
    // and a click is placed from the page's viewport, so the frame's own
    // viewport position is the whole of the offset: no scroll is added.
    return {ok: true, message: `Frame ${number}`, x: rect.x, y: rect.y};
  }

  // A point in a frame's own document means nothing in the page: the caller
  // reads points in the main document, where clicking happens.
  function atInFrame() {
    return failure('invalid_request', `Read the element at a point in the page itself, not in frame ${frameId()}`);
  }

  function outline() {
    const {lines, frames: listed} = outlineLines();
    setLastLines(lines);
    const frame = frameId();
    return {ok: true, message: frame ? `Read ${lines.length} lines in frame ${frame}` : `Read ${lines.length} lines`,
      outline: lines.join('\n'), data: {frames: listed}};
  }

  // Lines added and removed since the last read, by longest common
  // subsequence; refs are stable within a document, so an unchanged element
  // keeps its line.
  function changes() {
    // What a scroll answers says how far it took the page, so a turn that
    // changed no line still tells the caller the page moved, and by how much.
    const moved = takeScrollMoved();
    const before = getLastLines(), {lines} = outlineLines();
    setLastLines(lines);
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
    const told = moved === null ? diff : moved ? `Scrolled ${Math.abs(moved)}px ${moved > 0 ? 'down' : 'up'}; ${diff[0].toLowerCase()}${diff.slice(1)}` : `Did not move; ${diff[0].toLowerCase()}${diff.slice(1)}`;
    return {ok: true, message: told, outline: out.join('\n')};
  }

  // What a click or a hover at viewport point (x, y) would act on: the
  // element itself when it is one an agent can act on, else the nearest
  // ancestor that is.
  function targetAt(hit) {
    for (let node = hit; node && node.nodeType === 1; node = node.parentNode || node.host) {
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

  // What a click at viewport point (x, y) would hit.
  function at(x, y) {
    const hit = topmost(x, y);
    if (!hit) return {ok: true, message: `Nothing at ${x}, ${y}`, data: {x, y, element: null}};
    const target = targetAt(hit);
    const data = {x, y, element: describe(hit), interactive: !!target};
    if (target) Object.assign(data, {ref: refOf(target), target: summary(target)});
    return {ok: true, message: target ? `${summary(target)} [ref=${data.ref}] is at ${x}, ${y}` : `${describe(hit)} is at ${x}, ${y}`, data};
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

  // Where each frame this document lists sits in it, so the browser process can
  // turn a point in a frame's document into a point in the page.
  function frames() {
    const out = [];
    for (const [number, frame] of framesByNumber) {
      const element = frame.deref?.();
      if (!element || !element.isConnected) continue;
      const x = element.localName === 'frame' ? Number(element.getAttribute('x')) || 0 : element.getBoundingClientRect().x + scrollX;
      const y = element.localName === 'frame' ? Number(element.getAttribute('y')) || 0 : element.getBoundingClientRect().y + scrollY;
      out.push({number: String(number), x, y});
    }
    return out;
  }

  function outlineLines() {
    const lines = [];
    let chars = 0, visited = 0, truncated = false, pending = '', pendingDepth = 0;
    const emit = (depth, text) => {
      if (lines.length >= MAX_LINES || chars >= MAX_CHARS) { truncated = true; return false; }
      const line = '  '.repeat(Math.min(depth, 24)) + '- ' + text;
      lines.push(line);
      chars += line.length + 1;
      return true;
    };
    const flush = () => {
      const text = clean(pending, MAX_TEXT);
      pending = '';
      if (text) emit(pendingDepth, `text ${quote(text)}`);
    };
    markHoverTargets();
    const hovering = hoverTargets || new Set();
    const children = parent => {
      if (parent.shadowRoot) return parent.shadowRoot.childNodes;
      const assigned = parent.localName === 'slot' ? parent.assignedNodes({flatten: true}) : null;
      return assigned && assigned.length ? assigned : parent.childNodes;
    };
    const walk = (parent, depth, context) => {
      for (const child of children(parent)) {
        if (truncated) return;
        visit(child, depth, context);
      }
    };
    const visit = (node, depth, context) => {
      if (++visited > MAX_VISITED) { truncated = true; return; }
      if (node.nodeType === 3) {
        if (!context.inside && context.visible) {
          if (!pending) pendingDepth = depth;
          pending += node.data;
        }
        return;
      }
      if (node.nodeType !== 1) return;
      const element = node, tag = element.localName;
      if (SKIP.has(tag) || element.getAttribute('aria-hidden') === 'true') return;
      if (tag === 'input' && type(element) === 'hidden') return;
      if (element.checkVisibility && !element.checkVisibility()) return;
      const style = getComputedStyle(element);
      const visible = style.visibility === 'visible';
      const block = !style.display.startsWith('inline') && style.display !== 'contents';
      if (block) flush();
      const pointer = style.cursor === 'pointer';
      let kind = visible ? role(element) : '';
      // A page's drag source is what a gesture grabs, so a gesture addresses
      // it by ref: a list item that is also a listitem is still the thing a
      // hand moves. Roles only a container carries keep their words.
      if (!INTERACTIVE.has(kind) && kind !== 'heading' && element.draggable) kind = 'draggable';
      // A page that keeps a menu hidden until the pointer rests on it names
      // the element a hand has to reach, so a gesture can address it by ref.
      if (!INTERACTIVE.has(kind) && kind !== 'heading' && hovering.has(element)) kind = 'hoverable';
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
      const inner = {inside: context.inside || (tag === 'label' && !!element.control), pointer, visible};
      if (kind === 'img') {
        flush();
        const label = name(element, kind);
        emit(depth, label ? `${kind} ${quote(label)}` : kind);
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
        const label = clean(element.title || element.name, MAX_NAME);
        const id = frameNumber(element);
        const parts = [kind];
        if (label) parts.push(quote(label));
        parts.push(`[frame=${id}]`);
        const src = element.src ? `src=${quote(clean(element.src, 300))}` : '';
        if (src) parts.push(src);
        emit(depth, parts.join(' '));
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
      walk(element, depth, inner);
      if (block) flush();
    };
    if (document.body) visit(document.body, 0, {inside: false, pointer: false, visible: true});
    flush();
    prune();
    if (truncated) lines.push('- text "… outline truncated; act on what is listed or scroll and read again"');
    return {lines, truncated, frames: frames()};
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
      const inner = hit.shadowRoot.elementFromPoint(x, y);
      if (!inner || inner === hit) break;
      hit = inner;
    }
    return hit;
  }
  function within(node, element) {
    for (; node; node = node.parentNode || node.host) if (node === element) return true;
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

  function fill(ref, text, allowSecret) {
    const element = resolve(ref), tag = element.localName, kind = type(element);
    if (secret(element) && !allowSecret)
      throw failure('secret_field', `${ref} is a password or card field; fill it with fill_secret, which keeps the text out of logs and history`);
    const field = tag === 'textarea' || (tag === 'input' && !NOT_TEXT.has(kind));
    if (kind === 'file')
      throw failure('not_editable', `${ref} (${summary(element)}) takes files, not text; choose them with fill-page-files`);
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
      inserted = document.execCommand(text ? 'insertText' : 'delete', false, text);
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
    return {ok: true, message: `Filled ${summary(element)}`};
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
  let textLease = null, committingTextLease = null, composing = false;
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
  addEventListener('compositionstart', () => { composing = true; retireTextLease(); }, true);
  addEventListener('compositionend', () => { composing = false; }, true);

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
    if (!element?.isConnected || !document.hasFocus() || activeTextElement() !== element || composing)
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
    return bestText >= READER_MIN_TEXT ? best : null;
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
    const article = root.cloneNode(true);
    for (const dropped of article.querySelectorAll(READER_DROP)) dropped.remove();
    for (const element of [article, ...article.querySelectorAll('*')]) {
      element.removeAttribute('style');
      element.removeAttribute('class');
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

  return {
    nativeTextInputElement, configureNativeTextEditor, fillFilesElement, fence,
    run(command) {
      try {
        if (!doc) doc = command.doc;
        frameAddress = typeof command.frame === 'string' ? command.frame : '';
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
          case 'outline': return outline();
          // The engine mirrors a hover through the page's CSS only when that
          // CSS opens something on hover; otherwise the browser's own pointer
          // is enough and nothing is applied.
          case 'need_styles': { const n = markHoverTargets(); return {ok: true, need_styles: n > 0, doc: typeof doc === 'number' ? doc : command.doc}; }
          case 'changes': return changes();
          // A turn of the wheel: where it turns, and what is left of the
          // distance once the page has given what it has.
          case 'scroll_target': return scrollTarget(command);
          case 'scroll_left': return scrollLeft();
          case 'at': return frameId() ? atInFrame() : at(command.x, command.y);
          case 'frame_offset': return frameOffset(command.frame, command.anchored);
          case 'frame_fence': return fence();
          case 'settle': return settle(command.quiet_ms, command.timeout_ms);
          case 'locate': arm('pointer'); return locate(command.element, command.cross_origin);
          case 'locate_box': return locateBox(command.element);
          case 'fill': arm('script'); return fill(command.element, command.text, command.secret);
          case 'fill_files': arm('script'); return fillFilesTarget(command.element, command.paths);
          case 'select': arm('script'); return select(command.element, command.values);
          case 'submit': arm('script'); return submit(command.element, command.cross_origin);
          case 'focus': {
            watch = null;
            const focused = focus(command.element, command.key, command.cross_origin);
            arm('key');
            return focused;
          }
          case 'arm': return arm('key');
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
            return {ok: true, points, message: `Drawing ${points.length} points on ${command.element}`};
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
})();
