// Page reading and operation for browser actions. The browser process
// evaluates this in an isolated world of the main frame: it shares the page's
// DOM, but the page's own scripts can neither see nor call it. Refs are
// `d<document>e<element>`; the document number is the world's, so a ref never
// resolves in a later document.
globalThis.__tiloPage ||= (() => {
  const MAX_LINES = 1200, MAX_CHARS = 100000, MAX_VISITED = 40000;
  const MAX_TEXT = 200, MAX_NAME = 120, MAX_VALUE = 200, MAX_OPTIONS = 30;
  const INPUT_ROLES = {button: 'button', submit: 'button', reset: 'button', image: 'button',
    file: 'button', color: 'button', checkbox: 'checkbox', radio: 'radio', range: 'slider',
    number: 'spinbutton', search: 'searchbox'};
  const INTERACTIVE = new Set(['link', 'button', 'checkbox', 'radio', 'switch', 'textbox',
    'searchbox', 'combobox', 'listbox', 'option', 'slider', 'spinbutton', 'tab', 'menuitem',
    'menuitemcheckbox', 'menuitemradio', 'treeitem', 'clickable']);
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
    'title', 'base', 'svg', 'canvas', 'object', 'embed']);
  const NOT_TEXT = new Set(['button', 'submit', 'reset', 'image', 'file', 'checkbox', 'radio',
    'hidden']);
  const SET_VALUE = new Set(['date', 'time', 'datetime-local', 'month', 'week', 'color', 'range']);

  const ids = new WeakMap(), refs = new Map();
  let next = 0, doc = 0;
  // The outline this tab's agent last saw, for `changes`.
  let lastLines = null;
  const MAX_DIFF_CELLS = 4000000, MAX_SETTLE_POLL_MS = 50;

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
  function name(element, kind) {
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
  function summary(element) {
    const kind = role(element) || element.localName, label = name(element, kind);
    return label ? `${kind} ${quote(label)}` : kind;
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

  // What a click at viewport point (x, y) would hit.
  function at(x, y) {
    const hit = topmost(x, y);
    if (!hit) return {ok: true, message: `Nothing at ${x}, ${y}`, data: {x, y, element: null}};
    let target = null;
    for (let node = hit; node && node.nodeType === 1; node = node.parentNode || node.host) {
      if (INTERACTIVE.has(role(node)) || node.hasAttribute('onclick')) { target = node; break; }
    }
    const data = {x, y, element: describe(hit), interactive: !!target};
    if (target) Object.assign(data, {ref: refOf(target), target: summary(target)});
    return {ok: true, message: target ? `${summary(target)} [ref=${data.ref}] is at ${x}, ${y}` : `${describe(hit)} is at ${x}, ${y}`, data};
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
      // A bound label's words are its control's name; listing them again is noise.
      const inner = {inside: context.inside || (tag === 'label' && !!element.control), pointer, visible};
      if (kind === 'img' || kind === 'iframe') {
        flush();
        const label = kind === 'img' ? name(element, kind) : clean(element.title || element.name || element.src, MAX_NAME);
        emit(depth, label ? `${kind} ${quote(label)}` : kind);
        return;
      }
      if (INTERACTIVE.has(kind) || kind === 'heading') {
        flush();
        const label = name(element, kind), parts = [kind];
        if (label) parts.push(quote(label));
        if (INTERACTIVE.has(kind)) parts.push(`[ref=${refOf(element)}]`);
        parts.push(...details(element, kind));
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

  return {
    run(command) {
      try {
        if (!doc) doc = command.doc;
        switch (command.action) {
          case 'outline': return outline();
          case 'changes': return changes();
          case 'at': return at(command.x, command.y);
          case 'settle': return settle(command.quiet_ms, command.timeout_ms);
          case 'locate': return locate(command.element, command.cross_origin);
          case 'fill': return fill(command.element, command.text, command.secret);
          case 'select': return select(command.element, command.values);
          case 'submit': return submit(command.element, command.cross_origin);
          case 'focus': return focus(command.element, command.key, command.cross_origin);
        }
        return failure('invalid_request', 'Unknown page action');
      } catch (error) {
        if (error && error.ok === false) return error;
        return failure('script', clean(error && error.message || error, 300));
      }
    },
  };
})();
