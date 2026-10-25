// Page reading and operation for browser actions. The browser process
// evaluates this in an isolated world of the main frame: it shares the page's
// DOM, but the page's own scripts can neither see nor call it. Refs are
// `d<document>e<element>`; the document number is the world's, so a ref never
// resolves in a later document.
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
    file: 'button', color: 'button', checkbox: 'checkbox', radio: 'radio', range: 'slider',
    number: 'spinbutton', search: 'searchbox'};
  const INTERACTIVE = new Set(['link', 'button', 'checkbox', 'radio', 'switch', 'textbox',
    'searchbox', 'combobox', 'listbox', 'option', 'slider', 'spinbutton', 'tab', 'menuitem',
    'menuitemcheckbox', 'menuitemradio', 'treeitem', 'clickable', 'hint', 'draggable',
    'canvas']);
  const CONTAINERS = new Set(['navigation', 'main', 'banner', 'contentinfo', 'complementary',
    'form', 'dialog', 'alertdialog', 'region', 'search', 'list', 'listitem', 'table', 'row',
    'cell', 'columnheader', 'rowheader', 'menu', 'menubar', 'tablist', 'tabpanel', 'tree',
    'grid', 'group', 'radiogroup', 'toolbar', 'alert', 'status', 'article', 'figure']);
  const KEEP_EMPTY = new Set(['dialog', 'alertdialog', 'alert', 'status']);
  const COLLAPSE = new Set(['list', 'listitem', 'row', 'cell', 'columnheader', 'rowheader', 'group']);
  const NAMED_FROM_CONTENT = new Set(['link', 'button', 'heading', 'option', 'tab', 'menuitem',
    'menuitemcheckbox', 'menuitemradio', 'treeitem', 'checkbox', 'radio', 'switch', 'cell',
    'columnheader', 'rowheader', 'clickable']);
  const SKIP = new Set(['script', 'style', 'noscript', 'template', 'head', 'meta', 'link',
    'title', 'base', 'svg', 'object', 'embed']);
  const NOT_TEXT = new Set(['button', 'submit', 'reset', 'image', 'file', 'checkbox', 'radio',
    'hidden']);
  const SET_VALUE = new Set(['date', 'time', 'datetime-local', 'month', 'week', 'color', 'range']);

  const ids = new WeakMap(), refs = new Map();
  let next = 0, doc = 0;
  // The outline this tab's agent last saw, for `changes`.
  let lastLines = null;
  const MAX_DIFF_CELLS = 4000000, MAX_SETTLE_POLL_MS = 50;
  // Revealed lines one hover reports, and moves between a drag's two ends.
  const MAX_REVEAL_LINES = 40, MAX_DRAG_STEPS = 64;
  // The outline a gesture started from, so its answer says what it changed.
  let gestureBefore = null;

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
    if (NAMED_FROM_CONTENT.has(kind)) {
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
    const kind = role(element) || element.localName, label = name(element, kind, options);
    return label ? `${kind} ${quote(label)}` : kind;
  }

  // Elements a `:hover` rule in the page's own sheets targets are the
  // triggers of hover-only menus; the outline lists them with a ref so a
  // hover can name what its author cannot see. Bounded against a hostile
  // sheet, and rebuilt per outline so a changed page is read as it is.
  const MAX_HOVER_RULES = 4000, MAX_HOVER_TARGETS = 2000;
  let hoverTargets = null;
  function markHoverTargets() {
    hoverTargets = new Set();
    let seen = 0;
    for (const sheet of document.styleSheets) {
      let rules = null;
      try { rules = sheet.cssRules; } catch (error) { rules = null; }
      if (!rules) continue;
      for (const rule of rules) {
        if (++seen > MAX_HOVER_RULES) return;
        const selector = rule.selectorText;
        if (!selector || !selector.includes(':hover')) continue;
        let matched = [];
        try { matched = document.querySelectorAll(selector.replace(/:hover(?![\w-])/g, '.__tilo-hover__')); }
        catch (error) { matched = []; }
        for (const element of matched) {
          if (hoverTargets.size >= MAX_HOVER_TARGETS) return;
          hoverTargets.add(element);
        }
      }
    }
  }
  // The element on which a hover opens something: itself when a rule names
  // it, else the parent whose rule opened a child that starts hidden.
  function hoverReveals(element) {
    if (!hoverTargets) markHoverTargets();
    if (hoverTargets.has(element)) return true;
    if (element.localName === 'canvas') return false;
    for (const parent of [element, element.parentElement]) {
      if (!parent) continue;
      for (const child of parent.children) {
        if (hoverTargets.has(child) && !child.checkVisibility?.()) return true;
      }
    }
    return false;
  }

  function outline() {
    const {lines} = outlineLines();
    lastLines = lines;
    return {ok: true, message: `Read ${lines.length} lines`, outline: lines.join('\n')};
  }

  // Lines added and removed since the last read, by longest common
  // subsequence; refs are stable within a document, so an unchanged element
  // keeps its line.
  function changes() {
    const before = lastLines, {lines} = outlineLines();
    lastLines = lines;
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
    return {ok: true, message: added || removed ? `${added} lines added, ${removed} removed` : 'Nothing changed',
      outline: out.join('\n')};
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
    const points = raw.map(([x, y]) => ({x: ref ? origin.left + x : x, y: ref ? origin.top + y : y}));
    return {points, local: raw.map(([x, y]) => [x, y])};
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
      if (!kind && visible && !context.inside && tag !== 'label' && tag !== 'body' && tag !== 'html' &&
          (element.hasAttribute('onclick') || (element.hasAttribute('tabindex') && element.tabIndex >= 0) ||
           (pointer && !context.pointer))) kind = 'clickable';
      // A hover-only menu's trigger has no role of its own, no click and no
      // pointer cursor; it is found only by the rule its hover applies to.
      if (!kind && visible && !context.inside && tag !== 'body' && tag !== 'html' && hoverReveals(element))
        kind = 'hint';
      if (tag === 'canvas' && visible && !context.inside) kind = 'canvas';
      if (!kind && visible && !context.inside && element.getAttribute('draggable') === 'true')
        kind = 'draggable';
      // A bound label's words are its control's name; listing them again is noise.
      const inner = {inside: context.inside || (tag === 'label' && !!element.control), pointer, visible};
      if (kind === 'img' || kind === 'iframe' || kind === 'canvas') {
        flush();
        // A drawing surface has a ref and a size; its pixels are not text.
        const label = kind === 'img' ? name(element, kind)
          : kind === 'iframe' ? clean(element.title || element.name || element.src, MAX_NAME)
          : name(element, kind);
        const size = kind === 'canvas' ? ` ${Math.round(element.width || element.clientWidth || 0)}x${Math.round(element.height || element.clientHeight || 0)} [ref=${refOf(element)}]` : '';
        emit(depth, (label ? `${kind} ${quote(label)}` : kind) + size);
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
    return {lines, truncated};
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

  function fill(ref, text, allowSecret) {
    const element = resolve(ref), tag = element.localName, kind = type(element);
    if (secret(element) && !allowSecret)
      throw failure('secret_field', `${ref} is a password or card field; fill it with fill_secret, which keeps the text out of logs and history`);
    const field = tag === 'textarea' || (tag === 'input' && !NOT_TEXT.has(kind));
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
  function transferred(source) { return !source.isConnected; }

  // What a gesture changed, as lines added to and removed from the outline.
  function gestureResult(before, message, data) {
    const after = revealDelta(before);
    const changed = after.revealing || (data && data.removed);
    return {ok: true, message: changed ? `${message}; ${after.note}` : message,
      outline: after.revealing ? after.added.join('\n') : undefined, data: {...data, changed: !!changed, added: after.added}};
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
    const hit = topmost(x, y);
    if (element && !within(hit, resolve(element)))
      return gestureResult(before, `${element} is covered by ${describe(hit)}`, {x, y, hover: describe(hit), obscured: true});
    return hovered(x, y, before);
  }

  // The source a drag starts from: the element's own viewport point and the
  // element itself, or nothing when the drag names only two points.
  function dragEnd(ref, x, y, verb) {
    const end = landing(ref, x, y, verb);
    return {x: end.x, y: end.y, element: end.element};
  }

  return {
    nativeTextInputElement, configureNativeTextEditor,
    run(command) {
      try {
        if (!doc) doc = command.doc;
        // A gesture that spans two engine steps: read what is about to
        // change, let the pointer do its work, then answer with the change.
        if (command.action === 'gesture_begin') gestureBefore = {lines: outlineLines().lines, reveals: new Map()};
        if (command.action === 'gesture_end') {
          const before = gestureBefore || {lines: [], reveals: new Map()};
          gestureBefore = null;
          if (command.kind === 'hover') return hoveredAt(command.element, command.x, command.y, before);
          return gestureResult(before, command.message || (command.kind === 'path' ? 'Drew the pointer path' : 'Dragged'), {});
        }
        switch (command.action) {
          case 'prepare_text_input': case 'start_text_input': case 'check_text_input': case 'commit_text_input': case 'cancel_text_input': case 'undo_text_input': case 'redo_text_input':
            return guardedTextInput(command);
          case 'outline': return outline();
          case 'changes': return changes();
          case 'at': return at(command.x, command.y);
          case 'settle': return settle(command.quiet_ms, command.timeout_ms);
          case 'locate': return locate(command.element, command.cross_origin);
          case 'fill': return fill(command.element, command.text, command.secret);
          case 'select': return select(command.element, command.values);
          case 'submit': return submit(command.element, command.cross_origin);
          case 'focus': return focus(command.element, command.key, command.cross_origin);
          case 'reader': return reader(command.enabled === true);
          // The engine moves the pointer to a point computed here.
          case 'hover': {
            const end = landing(command.element, command.x, command.y, 'hover');
            return {x: end.x, y: end.y};
          }
          // The engine presses at one end, moves and releases at the other.
          case 'drag': {
            const from = dragEnd(command.element, command.x, command.y, 'drag');
            const to = dragEnd(command.to, command.to_x, command.to_y, 'drop on');
            return {x: from.x, y: from.y, to_x: to.x, to_y: to.y};
          }
          // A drag the native path could not complete, replayed as drag
          // events between the same two ends.
          case 'drag_fallback': {
            const from = dragEnd(command.element, command.x, command.y, 'drag');
            const to = dragEnd(command.to, command.to_x, command.to_y, 'drop on');
            const target = topmost(to.x, to.y) || document.body;
            const report = syntheticDrag({x: to.x, y: to.y, element: from.element}, target);
            const said = `Dragged ${from.element ? describe(from.element) : 'the pointer'} to ${describe(target)}`;
            if (!report.accepted)
              return gestureResult(gestureBefore, `Dropped on ${describe(target)}, which received no drop`, {...report});
            return gestureResult(gestureBefore, said, {...report, from: describe(from.element), to: describe(target)});
          }
          // A contact path for the engine's pointer, in viewport pixels.
          case 'path': {
            const {points} = contactPath(command.element, command.points || []);
            return {points, message: `Drawing ${points.length} points on ${command.element}`};
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
