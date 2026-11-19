// Renders a mount's composition as HTML (`dom: true`). The runtime hands over
// each frame's composition (ui::composition): every widget by its path, what
// it is (its semantics), how its layout node lays its children out (CSS), and
// what it shows of its own. This builds it into elements:
// - a container whose children stand where its layout node puts them
//   (`flows`) is a CSS flex or grid container with that node's style, and
//   its children are its flex items, each sized as the shared layout sized
//   it; any other container places its children at their rectangles;
// - text is text, set the way the widget sets it; a field is an <input>;
//   a document's blocks are paragraphs, headings and list items with their
//   inline marks; a button is a <button>; a widget with a scroll region is a
//   native scroller;
// - a leaf that is its own paint (an icon, a chart, a canvas) is a <canvas>
//   holding its picture at the screen's density, fetched when it changed.
// Elements are kept by path from frame to frame, and only properties that
// changed are written.
//
// The pointer stays the runtime's (dom-input.mjs forwards it to the canvas),
// so what a press, a hover or a drag does is the widget's own, exactly as on
// the canvas. A field and a document's text are the page's: a press there
// places the caret where the browser lays the text out.

const ROLE_TAGS = {
  Button: 'button', ToggleButton: 'button', Link: 'a', Document: 'article',
  Paragraph: 'p', Dialog: 'dialog',
};
const ARIA_ROLES = {
  Toolbar: 'toolbar', TabBar: 'tablist', Tab: 'tab', TabPanel: 'tabpanel', List: 'list',
  ListItem: 'listitem', Table: 'table', Row: 'row', Cell: 'cell', Tree: 'tree', TreeItem: 'treeitem',
  Menu: 'menu', MenuItem: 'menuitem', ListBox: 'listbox', ListBoxOption: 'option', Checkbox: 'checkbox',
  RadioButton: 'radio', Switch: 'switch', Slider: 'slider', Separator: 'separator', ComboBox: 'combobox',
  ProgressBar: 'progressbar', Tooltip: 'tooltip', Image: 'img', Group: 'group', Dialog: 'dialog',
};

// The roles that are on or off.
const CHECKABLE_ROLES = new Set(['Checkbox', 'Switch', 'RadioButton']);

// The roles whose value is a number on a range.
const RANGED_ROLES = new Set(['Slider', 'Separator', 'ProgressBar', 'SpinButton']);

// A role as the composition writes it: a name, or `{Heading: {level}}`.
const roleOf = semantics => {
  const role = semantics?.role;
  if (!role) return undefined;
  if (typeof role === 'string') return {name: role};
  const [name, fields] = Object.entries(role)[0] ?? [];
  return {name, ...fields};
};

// UTF-8 byte offsets in `text` as UTF-16 offsets, which the DOM counts in.
function byteToUnit(text, byte) {
  let bytes = 0;
  for (let unit = 0; unit < text.length; unit++) {
    if (bytes >= byte) return unit;
    const code = text.codePointAt(unit);
    bytes += code < 0x80 ? 1 : code < 0x800 ? 2 : code < 0x10000 ? 3 : 4;
    if (code >= 0x10000) unit++;
  }
  return text.length;
}
function unitToByte(text, unit) {
  return new TextEncoder().encode(text.slice(0, unit)).length;
}

// How much wider than the layout measured them the page may set words before
// it cuts them short: its glyph metrics differ from the layout's by less.
const SLACK = 4;

const rgba = color => color ? `rgba(${color[0]},${color[1]},${color[2]},${(color[3] / 255).toFixed(3)})` : '';

export function createViewPresenter(layer, {onScroll, picture, semantic, onFrame} = {}) {
  // Each widget's element, by path.
  let elements = new Map();
  // Each element's last written properties.
  const written = new WeakMap();
  // The native scrollers, by path, and where the composition last put each.
  let scrollers = new Map();
  // The pictures shown, by path: the version each holds.
  const pictures = new Map();
  // Scrolls the page made itself, which must not echo back.
  const settingScroll = new WeakSet();
  // Pictures to fetch once the frame in hand has been handed over: the
  // runtime is busy presenting it until then.
  const wanted = new Map();
  let fetching = false;
  function fetchPictures() {
    fetching = false;
    for (const [path, canvas] of wanted) {
      wanted.delete(path);
      const shown = picture?.(path, scale);
      stats.pictures++;
      if (shown) {
        const [width, height, pixels] = shown;
        canvas.width = width;
        canvas.height = height;
        canvas.getContext('2d').putImageData(new ImageData(pixels, width, height), 0, 0);
      } else {
        canvas.width = 0;
        canvas.height = 0;
      }
    }
  }
  const stats = {frames: 0, lastMs: 0, elements: 0, writes: 0, created: 0, pictures: 0};
  let scale = globalThis.devicePixelRatio || 1;

  layer.classList.add('tilo-view');

  // What the person does on a control is the widget's own semantic action: a
  // box is toggled, a range takes its value, a choice is activated by the
  // target the widget names it with. The press moves the widget's focus first,
  // as a press on the canvas does.
  function act(element, action, value) {
    const host = element.closest?.('[data-path]');
    const path = host?.dataset.path?.split('#')[0];
    if (!path) return;
    const target = element.dataset.tiloTarget;
    semantic?.(path, undefined, 'focus', '', 0, 0);
    semantic?.(path, target, action, value ?? '', 0, 0);
  }
  // A stepper button's step: the field's value moved by it, kept within bounds.
  function step(element) {
    const by = Number(element.dataset.tiloStepBy ?? 1);
    const number = parseFloat(element.dataset.tiloText ?? '');
    if (!Number.isFinite(number)) return undefined;
    const input = element.parentElement?.querySelector('input');
    const min = input?.min === '' ? undefined : Number(input?.min);
    const max = input?.max === '' ? undefined : Number(input?.max);
    let moved = number + by;
    if (Number.isFinite(min)) moved = Math.max(min, moved);
    if (Number.isFinite(max)) moved = Math.min(max, moved);
    return String(Math.round(moved * 1e6) / 1e6);
  }
  layer.addEventListener('click', event => {
    const element = event.target.closest?.('[data-tilo-act]');
    if (!element || element.disabled || element.getAttribute('aria-disabled') === 'true') return;
    // A link is followed by its widget (an app's own address bar), never by
    // the page leaving.
    if (element.localName === 'a') event.preventDefault?.();
    event.stopPropagation?.();
    const action = element.dataset.tiloAct;
    if (action === 'step') act(element, 'value', step(element));
    else act(element, action, element.dataset.tiloValue);
  });
  // A field the page types into: a range dragged, a number or date held.
  layer.addEventListener('input', event => {
    const element = event.target.closest?.('[data-tilo-act]');
    if (!element || element.dataset.tiloAct !== 'value') return;
    act(element, 'value', element.value);
  });

  function write(element, key, value) {
    let props = written.get(element);
    if (!props) written.set(element, props = {});
    if (props[key] === value) return;
    props[key] = value;
    stats.writes++;
    if (key === 'text') element.textContent = value;
    else if (key === 'css') element.style.cssText = value;
    else if (key.startsWith('@')) {
      const name = key.slice(1);
      if (value === undefined || value === null || value === false) element.removeAttribute(name);
      else element.setAttribute(name, value === true ? '' : value);
    }
  }

  function elementFor(path, tag, next) {
    let element = elements.get(path);
    if (!element || element.localName !== tag) {
      element = document.createElement(tag);
      element.dataset.path = path;
      stats.created++;
    }
    next.set(path, element);
    return element;
  }

  function tagOf(node, role) {
    // The element the widget renders as, when its own HTML names one.
    if (node.tag) return node.tag;
    const own = node.own?.kind;
    if (own === 'field') return 'input';
    if (own === 'text' && role?.name === 'Heading') return `h${Math.min(Math.max(role.level ?? 2, 1), 6)}`;
    if (role && ROLE_TAGS[role.name]) {
      // A button holds no other control.
      if (ROLE_TAGS[role.name] === 'button' && node.children?.length) return 'div';
      return ROLE_TAGS[role.name];
    }
    if (role?.name === 'Group' && node.semantics?.name) return 'section';
    return 'div';
  }

  // The innermost widget of a chain that reports the same scroll region.
  function scrollsItself(node) {
    if (!node.scroll) return false;
    const same = child => child.scroll && child.scroll[1] === node.scroll[1] && child.scroll[2] === node.scroll[2];
    const below = children => children?.some(child => same(child) || below(child.children));
    return !below(node.children);
  }

  // `node` as an element of `parent` (whose composition is `outer`).
  function render(node, outer, next, scrolledBy) {
    const role = roleOf(node.semantics);
    const element = elementFor(node.path, tagOf(node, role), next);
    const [x, y, w, h] = node.rect;
    const scroller = scrollsItself(node);
    const flows = node.flows && !scroller;
    const inFlow = outer?.flows && !outer.scroller;
    let css = '';
    if (inFlow) {
      // A flex item of its parent, at the size the shared layout gave it.
      css += `position:relative;flex:none;width:${w}px;height:${h}px;`;
      // The room it keeps from its siblings, as its layout node keeps it.
      const margin = /(?:^|;)margin:([^;]*)/.exec(node.layout ?? '')?.[1];
      if (margin) css += `margin:${margin};`;
    } else {
      const ox = outer ? outer.rect[0] : 0;
      const oy = outer ? outer.rect[1] - (scrolledBy ?? 0) : 0;
      css += `position:absolute;left:${x - ox}px;top:${y - oy}px;width:${w}px;height:${h}px;`;
    }
    if (flows && node.layout) {
      // The container lays its children out itself, from its node's style;
      // its own size is the one above.
      css += node.layout.replace(/(^|;)(width|height|min-width|min-height|max-width|max-height|flex-grow|flex-shrink|flex-basis|margin|align-self):[^;]*/g, '');
    }
    if (scroller) css += 'overflow-x:hidden;overflow-y:auto;overscroll-behavior:contain;';
    else if (node.clip) css += 'overflow:clip;';
    if (node.ground) {
      const {fill, edge, radius} = node.ground;
      if (fill) css += `background:${rgba(fill)};`;
      // A window or a panel casts its shadow on what it floats over.
      const shadows = node.floats ? [FLOATING_SHADOW] : [];
      if (edge) shadows.push(`inset 0 0 0 1px ${rgba(edge)}`);
      if (shadows.length) css += `box-shadow:${shadows.join(',')};`;
      if (radius) css += `border-radius:${radius}px;`;
    }
    if (node.own?.kind === 'text') {
      css += element.localName === 'li' ? textStyle(node.own) : textCss(node.own, w, h);
      // One line of words in a slot taller than the line (a count on its ground, a table's
      // cell, a part a widget lays out itself) stands in the slot's middle, as painted.
      const line = node.own.size * node.own.line_height;
      if ((node.ground || node.path.includes('#')) && !node.children?.length && h > line + 0.5 && (node.own.max_lines === 1 || Math.floor((h + 0.5) / line) <= 1)) css += `line-height:${h}px;`;
    }
    else if (node.own?.kind === 'face') {
      css += faceCss(node.own);
      // A face that hugs its words grows a little rather than cutting them short.
      if (!node.own.spread && node.own.label) css += slack(w, SLACK * 2);
    }
    else if (node.own?.kind === 'control') {
      // A switch with a caption: the caption leads, the switch keeps the trailing edge.
      const captioned = node.own.controls?.some(control => control.kind === 'check' && control.style === 'switch' && control.label);
      css += captioned
        ? 'display:flex;align-items:center;justify-content:space-between;gap:8px;min-width:0;'
        : 'display:flex;align-items:center;flex-wrap:wrap;gap:8px;min-width:0;';
    }
    else if (node.own?.kind === 'field') {
      const [ix, iy] = node.own.inset ?? [0, 0];
      css += textStyle(node.own.look) + `margin:0;padding:${iy}px ${ix}px;border:0;outline:none;font-family:inherit;resize:none;`;
      if (!node.ground) css += 'background:transparent;';
    }
    write(element, 'css', css);
    // What it is, for assistive technology and for the page's own find,
    // selection and focus.
    const semantics = node.semantics;
    const namedRole = role && ARIA_ROLES[role.name] && element.localName !== ARIA_ROLES[role.name]
      ? ARIA_ROLES[role.name]
      : null;
    // A notice is a status (or an alert when it must interrupt), even where
    // its widget is otherwise a plain group.
    const liveRole = semantics?.live ? (semantics.live === 'assertive' ? 'alert' : 'status') : null;
    const ariaRole = liveRole && (!namedRole || namedRole === 'group') ? liveRole : namedRole;
    write(element, '@role', ariaRole);
    // A notice is read out as it arrives, without moving the keyboard.
    write(element, '@aria-live', semantics?.live ? String(semantics.live).toLowerCase() : null);
    write(element, '@aria-busy', semantics?.state?.busy ? 'true' : null);
    const visibleText = node.own?.kind === 'text' || node.own?.kind === 'field' || node.own?.kind === 'blocks' || Boolean(node.own?.label);
    write(element, '@aria-label', !visibleText && semantics?.name ? semantics.name : null);
    write(element, '@aria-selected', semantics?.state?.selected ? 'true' : null);
    write(element, '@aria-pressed', role?.name === 'ToggleButton' ? String(Boolean(semantics?.state?.checked)) : null);
    write(element, '@aria-expanded', semantics?.state?.expanded === undefined || semantics?.state?.expanded === null ? null : String(semantics.state.expanded));
    // A slider, a split's divider or a progress bar says how far along it is; the number is the
    // one its value starts with, and what it says ("55%") stays as the words read out.
    const ranged = role && RANGED_ROLES.has(role.name);
    const amount = ranged ? String(semantics?.value ?? '') : '';
    const number = amount ? /^-?\d+(?:\.\d+)?/.exec(amount)?.[0] : undefined;
    write(element, '@aria-valuenow', number ?? null);
    write(element, '@aria-valuetext', number && amount !== number ? amount : null);
    // A divider is measured across the way it separates.
    write(element, '@aria-orientation', role?.name === 'Separator' ? (h >= w ? 'vertical' : 'horizontal') : null);
    // A dialog that holds the keyboard says so while it is the one the person is in.
    write(element, '@aria-modal', role?.name === 'Dialog' && semantics?.state?.modal ? 'true' : null);
    write(element, '@aria-disabled', semantics?.state?.disabled ? 'true' : null);
    write(element, '@data-widget', node.widget || null);
    if (element.localName === 'button') write(element, '@type', 'button');
    if (element.localName === 'button') write(element, '@tabindex', '-1');
    // A control's own element carries its role (a box, a range, a chooser's
    // list): the wrapper around it is no control of its own.
    if (node.own?.kind === 'control') write(element, '@role', null);
    // A box, a switch or a radio the widget draws itself says
    // whether it is on, as an <input> of its own would.
    const checks = role && CHECKABLE_ROLES.has(role.name) && node.own?.kind !== 'control';
    const checked = semantics?.state?.mixed ? 'mixed' : semantics?.state?.checked;
    write(element, '@aria-checked', checks && checked !== undefined && checked !== null ? String(checked) : null);
    // What only its own kind of element carries (an <img>'s source, a <progress>'s value).
    const attrs = new Set();
    for (const [name, value] of node.attrs ?? []) {
      attrs.add(name);
      write(element, `@${name}`, value);
    }
    for (const name of element.tiloAttrs ?? []) if (!attrs.has(name)) write(element, `@${name}`, null);
    element.tiloAttrs = attrs;

    const own = node.own;
    const children = [];
    // The surfaces it lays under its children (a sidebar's ground).
    (node.surfaces ?? []).forEach(([[sx, sy, sw, sh], surface], index) => {
      const ground = elementFor(`${node.path}#surface${index}`, 'div', next);
      write(ground, 'css', `position:absolute;left:${sx - x}px;top:${sy - y}px;width:${sw}px;height:${sh}px;background:var(--tilo-${surface},var(--tilo-sidebar));pointer-events:none;`);
      write(ground, '@aria-hidden', 'true');
      children.push(ground);
    });
    if (own?.kind === 'text') showText(element, own);
    else if (own?.kind === 'field') showField(element, node, own);
    else if (own?.kind === 'blocks') children.push(showBlocks(element, node, own, next));
    else if (own?.kind === 'painted') children.push(showPicture(element, node, own.version, next));
    else if (own?.kind === 'face') children.push(...showFace(node, own, next));
    else if (own?.kind === 'control') children.push(...showControls(element, node, own.controls, next));

    const scrollTop = scroller ? node.scroll[0] : 0;
    const context = {flows, scroller, rect: node.rect};
    for (const child of node.children ?? []) {
      const [, , cw, ch] = child.rect;
      // Nothing to show of a child with no room (a closed menu, an empty
      // entry), and nothing of it to reach.
      if (cw <= 0 || ch <= 0) continue;
      children.push(render(child, context, next, scrollTop));
    }
    if (scroller) {
      // The scroller's content is as tall as its region says, wherever the
      // rows it shows now stand in it.
      const sizer = elementFor(`${node.path}#content`, 'div', next);
      write(sizer, 'css', `position:absolute;left:0;top:0;width:1px;height:${node.scroll[1]}px;pointer-events:none;`);
      write(sizer, '@aria-hidden', 'true');
      children.push(sizer);
      scrollers.set(node.path, {element, offset: node.scroll[0]});
    }
    if (!own || own.kind === 'blocks' || own.kind === 'painted' || own.kind === 'face' || own.kind === 'control') place(element, children);
    return element;
  }

  // Put `children` in `element` in order, moving only what is out of place.
  function place(element, children) {
    let at = element.firstChild;
    for (const child of children) {
      if (child === at) { at = at.nextSibling; continue; }
      element.insertBefore(child, at);
    }
    while (at) { const following = at.nextSibling; at.remove(); at = following; }
  }

  function textStyle(look) {
    let css = `font-size:${look.size}px;line-height:${look.line_height};font-weight:${look.weight};`;
    if (look.italic) css += 'font-style:italic;';
    if (look.color) css += `color:${rgba(look.color)};`;
    if (look.mono) css += 'font-family:var(--tilo-mono);';
    css += `text-align:${look.align};`;
    return css;
  }

  // Words in the box the shared layout measured for them. The page's
  // glyphs come out a hair wider or narrower than the layout's, so the box
  // never decides a line break on its own: words the layout set on one line
  // stay on one (spilling a pixel rather than wrapping a word in two), and
  // words it cut short get a few pixels of slack before the ellipsis.
  function textCss(look, w, h) {
    const line = look.size * look.line_height;
    const lines = look.max_lines ?? Math.max(1, Math.floor((h + 0.5) / line));
    let fit;
    if (lines === 1) {
      fit = look.max_lines === 1
        ? `white-space:pre;overflow:hidden;text-overflow:ellipsis;${slack(w)}`
        : 'white-space:pre;';
    } else {
      fit = look.max_lines
        ? `display:-webkit-box;-webkit-box-orient:vertical;-webkit-line-clamp:${look.max_lines};overflow:hidden;white-space:pre-wrap;overflow-wrap:break-word;`
        : 'white-space:pre-wrap;overflow-wrap:break-word;';
    }
    return textStyle(look) + fit;
  }

  // A one-line run that may be cut short: as wide as its words up to a few
  // pixels past the width the layout measured, never narrower.
  function slack(w, by = SLACK) {
    return `width:max-content;min-width:${w}px;max-width:${w + by}px;`;
  }

  function showText(element, look) {
    // An item's words sit in a run of their own, centred in the row and cut
    // short with an ellipsis; anywhere else they are the element's text.
    if (element.localName !== 'li') { write(element, 'text', look.text); return; }
    let words = element.firstElementChild;
    if (!words || !words.classList.contains('tilo-words')) {
      words = document.createElement('span');
      words.className = 'tilo-words';
      element.replaceChildren(words);
      written.set(element, {...written.get(element), text: undefined});
    }
    if (words.textContent !== look.text) words.textContent = look.text;
  }

  // A field is an <input>: the browser's caret, selection, autofill and IME.
  // What the person types goes to the widget as its new value.
  function showField(element, node, own) {
    write(element, '@placeholder', own.placeholder ?? null);
    write(element, '@aria-label', node.semantics?.name ?? null);
    element.dataset.tiloField = 'true';
    if (document.activeElement !== element && element.value !== own.value) element.value = own.value;
    if (!element.tiloBound) {
      element.tiloBound = true;
      element.addEventListener('input', () => semantic?.(element.dataset.path, undefined, 'value', element.value, 0, 0));
      element.addEventListener('keydown', event => {
        // Enter and Escape end the edit the widget's own way.
        if (event.key === 'Enter' || event.key === 'Escape') {
          semantic?.(element.dataset.path, undefined, 'value', element.value, 0, 0);
          element.blur();
        }
        event.stopPropagation();
      });
    }
  }

  // A document's blocks: paragraphs, headings, quotes, list and checklist
  // items, code, each with its inline marks. A press in a block's text puts
  // the caret where the browser laid that text out.
  function showBlocks(element, node, {blocks, edges}, next) {
    const body = elementFor(`${node.path}#blocks`, 'div', next);
    // Its text stands where the widget lays it: a paragraph at its text
    // edge, list items and quotes past their gutters.
    write(body, 'css', `position:relative;display:flex;flex-direction:column;padding-left:${edges.text}px;--tilo-list:${edges.list}px;--tilo-step:${edges.indent}px;--tilo-quote:${edges.quote}px;`);
    body.classList.add('tilo-doc');
    const items = blocks.map(block => {
      const tag = block.kind === 'heading' ? `h${Math.min(Math.max(block.level, 1), 3)}`
        : block.kind === 'quote' ? 'blockquote' : block.kind === 'code' ? 'pre'
        : block.kind === 'divider' ? 'hr' : block.kind === 'bullet' || block.kind === 'numbered' || block.kind === 'check' ? 'li' : 'p';
      const item = elementFor(`${node.path}#block:${block.id}`, tag, next);
      item.className = `tilo-block tilo-${block.kind}${block.soft_break ? ' tilo-soft' : ''}${block.done ? ' tilo-done' : ''}`;
      // Set as the editor sets it: its gap above, its size, leading and weight.
      write(item, 'css', `--tilo-indent:${block.level};margin-top:${block.gap}px;font-size:${block.size}px;line-height:${block.line_height};font-weight:${block.weight};`);
      write(item, '@data-ordinal', block.ordinal ?? null);
      item.dataset.block = block.id;
      item.dataset.editor = node.path;
      if (block.kind === 'divider') return item;
      const key = JSON.stringify([block.text, block.marks, block.selection, block.done]);
      if (item.tiloKey !== key) {
        item.tiloKey = key;
        item.replaceChildren(...inline(block, node.path));
        stats.writes++;
      }
      return item;
    });
    place(body, items);
    return body;
  }

  // A block's text as runs of its marks, with the caret or selection in it.
  function inline(block, editor) {
    const text = block.text;
    const out = [];
    if (block.kind === 'check') {
      const box = document.createElement('input');
      box.type = 'checkbox';
      box.checked = block.done;
      box.className = 'tilo-check';
      box.addEventListener('click', event => {
        event.preventDefault();
        semantic?.(editor, `notes.block-check:${block.id}`, 'toggle', '', 0, 0);
      });
      out.push(box);
    }
    // Every boundary a mark or the selection makes, in UTF-16 units.
    const cuts = new Set([0, text.length]);
    const marks = block.marks.map(mark => ({...mark, from: byteToUnit(text, mark.start), to: byteToUnit(text, mark.end)}));
    for (const mark of marks) { cuts.add(mark.from); cuts.add(mark.to); }
    const selection = block.selection && [byteToUnit(text, block.selection[0]), byteToUnit(text, block.selection[1])].sort((a, b) => a - b);
    if (selection) { cuts.add(selection[0]); cuts.add(selection[1]); }
    const points = [...cuts].sort((a, b) => a - b);
    const caret = () => { const span = document.createElement('span'); span.className = 'tilo-caret'; span.setAttribute('aria-hidden', 'true'); return span; };
    if (selection && selection[0] === selection[1] && selection[0] === 0) out.push(caret());
    for (let i = 0; i + 1 < points.length; i++) {
      const [from, to] = [points[i], points[i + 1]];
      let run = document.createTextNode(text.slice(from, to));
      for (const mark of marks) {
        if (mark.from > from || mark.to < to) continue;
        const tag = {bold: 'strong', italic: 'em', code: 'code', underline: 'u', strike: 's', link: 'a', mention: 'strong'}[mark.mark] ?? 'span';
        const wrap = document.createElement(tag);
        if (mark.mark === 'link' && mark.href) wrap.href = mark.href;
        if (mark.mark === 'mention') wrap.className = 'tilo-mention';
        wrap.append(run);
        run = wrap;
      }
      if (selection && selection[0] !== selection[1] && from >= selection[0] && to <= selection[1]) {
        const wrap = document.createElement('span');
        wrap.className = 'tilo-selected';
        wrap.append(run);
        run = wrap;
      }
      out.push(run);
      if (selection && selection[0] === selection[1] && to === selection[0]) out.push(caret());
    }
    // An empty block still holds a line.
    if (!text) out.push(document.createElement('br'));
    return out;
  }

  // A control's face: its icon, words and trailing icon in a row on its
  // ground, laid out by the page.
  function faceCss(face) {
    let css = `display:flex;align-items:center;justify-content:${face.spread ? 'space-between' : 'center'};gap:${face.gap}px;padding:0 ${face.pad}px;box-sizing:border-box;border-radius:${face.radius}px;white-space:nowrap;`;
    if (face.fill) css += `background:${rgba(face.fill)};`;
    if (face.edge) css += `box-shadow:inset 0 0 0 1px ${rgba(face.edge)};`;
    const ink = face.label?.color ?? face.icon?.color;
    if (ink) css += `color:${rgba(ink)};`;
    return css;
  }

  function showFace(node, face, next) {
    const out = [];
    if (face.icon) out.push(glyph(`${node.path}#icon`, face.icon, next));
    // The label and what follows it share one run, so the label gives way
    // first when the face is narrow.
    if (face.label || face.detail) {
      const words = elementFor(`${node.path}#words`, 'span', next);
      write(words, 'css', `display:flex;align-items:baseline;gap:${face.gap * 2}px;min-width:0;${face.spread ? 'flex:1;justify-content:space-between;' : ''}`);
      const runs = [];
      if (face.label) {
        const label = elementFor(`${node.path}#label`, 'span', next);
        write(label, 'css', textStyle(face.label) + (face.label.max_lines > 1
          ? `min-width:0;display:-webkit-box;-webkit-box-orient:vertical;-webkit-line-clamp:${face.label.max_lines};overflow:hidden;white-space:pre-wrap;`
          : 'min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:pre;'));
        write(label, 'text', face.label.text);
        runs.push(label);
      }
      if (face.detail) {
        const detail = elementFor(`${node.path}#detail`, 'span', next);
        write(detail, 'css', textStyle(face.detail) + 'flex:none;white-space:pre;');
        write(detail, 'text', face.detail.text);
        runs.push(detail);
      }
      place(words, runs);
      out.push(words);
    }
    if (face.trailing) out.push(glyph(`${node.path}#trailing`, face.trailing, next));
    return out;
  }

  // A control is the page's own element: a box, a range, an entry, a chooser
  // with its options, a row of choices or a link, holding what the widget says
  // it holds. What the person does on it goes back as the semantic action the
  // widget performs (a toggle, a value, an activation of a named choice), so
  // there is one input path for the canvas and the page alike.
  function mark(element, action, target, value) {
    element.dataset.tiloAct = action;
    if (target === undefined || target === null) delete element.dataset.tiloTarget;
    else element.dataset.tiloTarget = target;
    if (value === undefined || value === null) delete element.dataset.tiloValue;
    else element.dataset.tiloValue = value;
  }

  // A run of a control's words, set as the widget sets them.
  function wordsFor(path, look, next) {
    const span = elementFor(path, 'span', next);
    write(span, 'css', textStyle(look) + (look.max_lines > 1
      ? `min-width:0;display:-webkit-box;-webkit-box-orient:vertical;-webkit-line-clamp:${look.max_lines};overflow:hidden;white-space:pre-wrap;`
      : 'min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:pre;'));
    write(span, 'text', look.text);
    return span;
  }

  function showControls(element, node, controls, next) {
    const out = [];
    const target = node.semantics?.action_target;
    const disabled = Boolean(node.semantics?.state?.disabled);
    (controls ?? []).forEach((control, index) => {
      const key = `${node.path}#control:${index}`;
      if (control.kind === 'link') {
        // Its element keeps the widget's own place and size: only the ink and
        // the cue that it opens something are the link's.
        element.classList.add('tilo-link');
        write(element, '@href', control.href ?? null);
        out.push(wordsFor(`${key}:label`, control.label, next));
        mark(element, 'activate', target);
        return;
      }
      if (control.kind === 'check') {
        const input = elementFor(`${key}:input`, 'input', next);
        write(input, '@type', control.style === 'radio' ? 'radio' : 'checkbox');
        write(input, '@role', control.style === 'switch' ? 'switch' : null);
        if (input.checked !== control.checked) input.checked = control.checked;
        input.indeterminate = Boolean(control.mixed);
        input.disabled = disabled;
        write(input, '@aria-label', control.label ? null : (node.semantics?.name ?? null));
        mark(input, 'toggle', target);
        const label = control.label && wordsFor(`${key}:label`, control.label, next);
        if (label && control.style === 'switch') out.push(label, input);
        else out.push(input, ...(label ? [label] : []));
        return;
      }
      if (control.kind === 'range') {
        const input = elementFor(`${key}:input`, 'input', next);
        write(input, '@type', 'range');
        write(input, '@aria-label', node.semantics?.name ?? null);
        write(input, '@aria-valuetext', control.text ?? null);
        input.min = String(control.min);
        input.max = String(control.max);
        input.step = control.step === undefined || control.step === null ? 'any' : String(control.step);
        if (document.activeElement !== input && Number(input.value) !== control.value) input.value = String(control.value);
        input.disabled = disabled;
        // How far along its track it is, for the filled part the toolkit paints.
        const span = control.max - control.min;
        const along = span > 0 ? Math.min(100, Math.max(0, (control.value - control.min) / span * 100)) : 0;
        write(input, 'css', `--tilo-along:${along}%;`);
        mark(input, 'value', target);
        out.push(input);
        // What it is at, said beside it as the canvas says it.
        if (control.text && control.shown) {
          const said = elementFor(`${key}:said`, 'span', next);
          write(said, 'css', 'flex:none;min-width:40px;text-align:end;font-size:13px;font-variant-numeric:tabular-nums;color:var(--tilo-text-dim,#6e6e73);');
          write(said, 'text', control.text);
          write(said, '@aria-hidden', 'true');
          out.push(said);
        }
        return;
      }
      if (control.kind === 'entry') {
        const box = elementFor(key, 'span', next);
        write(box, 'css', 'display:flex;align-items:center;gap:2px;min-width:0;height:100%;flex:1;');
        const input = elementFor(`${key}:input`, 'input', next);
        const kinds = {number: 'text', date: 'date', time: 'time', colour: 'color', search: 'search'};
        write(input, '@type', control.secret ? 'password' : (kinds[control.input] ?? 'text'));
        write(input, '@role', control.input === 'number' ? 'spinbutton' : null);
        write(input, '@placeholder', control.placeholder || null);
        write(input, '@aria-label', node.semantics?.name ?? null);
        if (control.min !== undefined) input.min = String(control.min);
        if (control.max !== undefined) input.max = String(control.max);
        if (control.step !== undefined) input.step = String(control.step);
        if (document.activeElement !== input && input.value !== control.text) input.value = control.text;
        input.disabled = disabled;
        // A secret is never the page's field: its characters stay with the
        // widget, and the page shows the mask it was given.
        if (!control.secret) mark(input, 'value', target);
        write(input, 'css', control.input === 'colour'
          ? 'width:30px;height:30px;padding:0;border:0;background:transparent;cursor:pointer;'
          : 'flex:1;min-width:0;background:transparent;border:0;outline:none;color:inherit;font:inherit;padding:0;');
        const parts = [];
        if (control.steppers) {
          const minus = elementFor(`${key}:down`, 'button', next);
          const plus = elementFor(`${key}:up`, 'button', next);
          for (const [button, sign, name] of [[minus, -1, control.decrease], [plus, 1, control.increase]]) {
            write(button, '@aria-label', name ?? null);
            write(button, 'css', 'all:unset;box-sizing:border-box;position:relative;flex:none;width:22px;height:22px;display:flex;align-items:center;justify-content:center;border-radius:6px;color:var(--tilo-text-dim,#6e6e73);font-size:13px;line-height:1;cursor:pointer;');
            // Its minus or plus is drawn as the toolkit's stroke, not a character.
            write(button, 'text', '');
            button.disabled = disabled;
            button.dataset.tiloStepBy = String(sign * (control.step ?? 1));
            button.dataset.tiloText = control.text;
            mark(button, 'step', target);
          }
          parts.push(minus, input, plus);
        } else {
          parts.push(input);
        }
        place(box, parts);
        out.push(box);
        return;
      }
      if (control.kind === 'select') {
        const box = elementFor(key, 'div', next);
        write(box, 'css', 'position:relative;min-width:0;width:100%;height:100%;display:flex;align-items:center;');
        const button = elementFor(`${key}:current`, 'button', next);
        write(button, '@role', 'combobox');
        write(button, '@aria-haspopup', 'listbox');
        write(button, '@aria-expanded', String(Boolean(control.open)));
        write(button, 'css', 'display:flex;align-items:center;gap:6px;width:100%;height:100%;padding:0 10px;border-radius:var(--tilo-radius,8px);box-sizing:border-box;cursor:pointer;');
        button.disabled = disabled;
        mark(button, 'activate', target);
        const runs = [];
        if (control.icon) runs.push(glyph(`${key}:icon`, control.icon, next));
        if (control.current) runs.push(wordsFor(`${key}:value`, control.current, next));
        else {
          const quiet = elementFor(`${key}:placeholder`, 'span', next);
          write(quiet, 'css', 'min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:pre;color:var(--tilo-text-dim,#6e6e73);');
          write(quiet, 'text', control.placeholder);
          runs.push(quiet);
        }
        if (control.chevron) {
          const chevron = glyph(`${key}:chevron`, control.chevron, next);
          chevron.style.marginLeft = 'auto';
          runs.push(chevron);
        }
        place(button, runs);
        const parts = [button];
        if (control.open && control.options.length) {
          const list = elementFor(`${key}:list`, 'ul', next);
          write(list, '@role', 'listbox');
          write(list, 'css', 'position:absolute;left:0;top:calc(100% + 4px);margin:0;padding:4px;min-width:100%;max-height:280px;overflow-y:auto;box-sizing:border-box;border-radius:var(--tilo-radius,8px);background:var(--tilo-overlay,Canvas);box-shadow:0 8px 24px rgba(0,0,0,.18),0 0 0 1px var(--tilo-border,rgba(127,127,127,.25));z-index:3;');
          place(list, control.options.map((option, row) => optionRow(`${key}:option:${row}`, option, target, next, 'option')));
          parts.push(list);
        }
        place(box, parts);
        out.push(box);
        return;
      }
      if (control.kind === 'choices') {
        const box = elementFor(key, 'div', next);
        write(box, '@role', control.list);
        // A menu and a list of options stand in a column; a toolbar, a tab
        // strip, a group of swatches or radios, a row.
        const column = control.list === 'menu' || control.list === 'listbox';
        // Choices the widget places itself (a toolbar's tools around its spacers) stand there,
        // a separator where it draws one.
        const placed = control.options.length > 0 && control.options.every((option) => typeof option.at === 'number');
        write(box, 'css', placed
          ? 'position:relative;width:100%;height:100%;min-width:0;'
          : `display:flex;gap:2px;min-width:0;flex-wrap:wrap;${column ? 'flex-direction:column;align-items:stretch;' : 'align-items:center;'}`);
        const rows = [];
        control.options.forEach((option, row) => {
          const path = `${key}:choice:${row}`;
          if (placed && typeof option.rule === 'number') {
            const rule = elementFor(`${path}:rule`, 'span', next);
            write(rule, '@aria-hidden', 'true');
            write(rule, 'css', `position:absolute;left:${option.rule - 0.5}px;top:25%;width:1px;height:50%;background:var(--tilo-border,rgba(127,127,127,.3));`);
            rows.push(rule);
          }
          rows.push(optionRow(path, option, target, next, control.item,
            placed ? `position:absolute;left:${option.at}px;top:50%;transform:translateY(-50%);` : ''));
        });
        if (control.label) rows.unshift(wordsFor(`${key}:label`, control.label, next));
        place(box, rows);
        out.push(box);
        return;
      }
    });
    return out;
  }

  // One choice of a control: an option of a chooser's list, a tab, a menu row.
  function optionRow(path, option, fallback, next, role, at = '') {
    const item = elementFor(path, role === 'option' ? 'li' : 'button', next);
    write(item, '@role', role);
    write(item, '@aria-selected', role === 'radio' ? null : String(Boolean(option.selected)));
    if (role === 'radio' || role === 'option') write(item, '@aria-checked', role === 'radio' ? String(Boolean(option.selected)) : null);
    write(item, '@aria-disabled', option.disabled ? 'true' : null);
    // A tool on a toolbar is its icon; its name is what a reader hears and a tooltip says.
    const iconOnly = role === 'button' && Boolean(option.icon);
    write(item, '@aria-label', iconOnly ? option.label : null);
    write(item, '@title', iconOnly ? option.label : null);
    write(item, 'css', (iconOnly
      ? 'display:flex;align-items:center;justify-content:center;width:32px;height:32px;padding:0;border-radius:6px;cursor:pointer;' + (option.selected ? 'background:color-mix(in srgb,var(--tilo-accent,#2f6fed) 14%,transparent);' : '')
      : 'display:flex;align-items:center;gap:6px;padding:5px 9px;border-radius:6px;white-space:nowrap;cursor:pointer;' + (option.selected ? 'background:var(--tilo-raised,rgba(127,127,127,.16));' : ''))
      + (option.dot ? 'position:relative;' : '') + at);
    item.disabled = Boolean(option.disabled);
    const shown = [];
    if (option.colour) {
      const dot = elementFor(`${path}:colour`, 'span', next);
      write(dot, 'css', `flex:none;width:16px;height:16px;border-radius:50%;box-shadow:inset 0 0 0 1px var(--tilo-border,rgba(127,127,127,.35));background:${rgba(option.colour)};`);
      shown.push(dot);
    }
    if (option.icon) shown.push(glyph(`${path}:icon`, option.icon, next));
    if (!iconOnly) {
      const label = elementFor(`${path}:label`, 'span', next);
      write(label, 'css', 'flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:pre;');
      write(label, 'text', option.label);
      shown.push(label);
    }
    if (option.detail) {
      const detail = elementFor(`${path}:detail`, 'span', next);
      write(detail, 'css', 'flex:none;color:var(--tilo-text-dim,#6e6e73);white-space:pre;');
      write(detail, 'text', option.detail);
      shown.push(detail);
    }
    if (option.dot) {
      // Something new behind it: the accent dot at its corner, as painted.
      const dot = elementFor(`${path}:dot`, 'span', next);
      write(dot, '@aria-hidden', 'true');
      write(dot, 'css', 'position:absolute;right:5px;top:5px;width:6px;height:6px;border-radius:50%;background:var(--tilo-accent,#2f6fed);');
      shown.push(dot);
    }
    place(item, shown);
    if (option.value !== undefined && option.value !== null) mark(item, 'value', fallback, option.value);
    else mark(item, 'activate', option.target ?? fallback);
    return item;
  }

  // An icon: its outlines as an inline SVG in its colour.
  const SVG = 'http://www.w3.org/2000/svg';
  function glyph(path, icon, next) {
    const box = elementFor(path, 'span', next);
    write(box, 'css', `display:block;flex:none;width:${icon.size}px;height:${icon.size}px;color:${rgba(icon.color)};`);
    write(box, '@aria-hidden', 'true');
    const key = JSON.stringify([icon.name, icon.paths.length, icon.stroke, icon.size, icon.filled]);
    if (box.tiloGlyph !== key) {
      box.tiloGlyph = key;
      const svg = document.createElementNS(SVG, 'svg');
      svg.setAttribute('viewBox', '0 0 24 24');
      svg.setAttribute('width', String(icon.size));
      svg.setAttribute('height', String(icon.size));
      svg.setAttribute('fill', icon.filled ? 'currentColor' : 'none');
      svg.setAttribute('stroke', 'currentColor');
      // The outline is as wide on screen at every size, as the toolkit
      // strokes it.
      svg.setAttribute('stroke-width', String(icon.stroke * 24 / icon.size));
      svg.setAttribute('stroke-linecap', 'round');
      svg.setAttribute('stroke-linejoin', 'round');
      for (const d of icon.paths) {
        const line = document.createElementNS(SVG, 'path');
        line.setAttribute('d', d);
        svg.append(line);
      }
      box.replaceChildren(svg);
      stats.writes++;
    }
    return box;
  }

  // A widget that is its own paint, as its picture at the screen's density.
  function showPicture(element, node, version, next) {
    const canvas = elementFor(`${node.path}#picture`, 'canvas', next);
    write(canvas, 'css', `position:absolute;left:0;top:0;width:${node.rect[2]}px;height:${node.rect[3]}px;pointer-events:none;`);
    write(canvas, '@aria-hidden', 'true');
    const want = `${version}@${scale}`;
    if (pictures.get(node.path) !== want) {
      pictures.set(node.path, want);
      wanted.set(node.path, canvas);
    }
    return canvas;
  }

  function present(metaText, compositionText) {
    const started = performance.now();
    const meta = JSON.parse(metaText);
    const composition = JSON.parse(compositionText);
    scale = globalThis.devicePixelRatio || 1;
    if (meta.theme) for (const [name, value] of Object.entries(meta.theme)) layer.style.setProperty(name, value);
    layer.style.background = meta.background;
    const next = new Map();
    scrollers = new Map();
    const roots = [render(composition.root, null, next, 0)];
    for (const overlay of composition.layers ?? []) roots.push(render(overlay, null, next, 0));
    place(layer, roots);
    // Scrollers stand where the composition says, unless the person is
    // moving them: their own scroll reaches the widget first.
    for (const {element, offset} of scrollers.values()) {
      if (Math.abs(element.scrollTop - offset) >= 1) {
        const before = element.scrollTop;
        settingScroll.add(element);
        element.scrollTop = offset;
        // A scroll bar clamps what it cannot reach: nothing moved means no
        // `scroll` event will come back to take, and a set left set here
        // would swallow the person's next scroll of this region.
        if (element.scrollTop === before) settingScroll.delete(element);
      }
      if (!element.tiloScroll) {
        element.tiloScroll = true;
        element.addEventListener('scroll', () => {
          if (settingScroll.has(element)) { settingScroll.delete(element); return; }
          onScroll?.(element.dataset.path, element.scrollTop);
        }, {passive: true});
      }
    }
    for (const path of pictures.keys()) if (!next.has(`${path}#picture`)) pictures.delete(path);
    elements = next;
    if (wanted.size && !fetching) { fetching = true; queueMicrotask(fetchPictures); }
    stats.frames++;
    stats.elements = next.size;
    stats.lastMs = performance.now() - started;
    onFrame?.({meta, stats});
  }

  // A press in a document's text puts the caret where the browser laid the
  // text out: the block's text target is told the offset, in its bytes.
  function caretFromPoint(x, y) {
    const position = document.caretPositionFromPoint?.(x, y);
    if (position) return {node: position.offsetNode, offset: position.offset};
    const range = document.caretRangeFromPoint?.(x, y);
    return range ? {node: range.startContainer, offset: range.startOffset} : null;
  }
  layer.addEventListener('pointerdown', event => {
    const item = event.target.closest?.('.tilo-block');
    if (!item || event.button !== 0 || event.target.closest('.tilo-check')) return;
    const point = caretFromPoint(event.clientX, event.clientY);
    if (!point || !item.contains(point.node)) return;
    // The offset of the caret in the block's text: the text of the runs
    // before it, plus where in its own run it fell.
    let units = 0;
    const walker = document.createTreeWalker(item, NodeFilter.SHOW_TEXT);
    for (let text = walker.nextNode(); text; text = walker.nextNode()) {
      if (text === point.node) { units += point.offset; break; }
      units += text.data.length;
    }
    const block = item.dataset.block;
    const full = item.textContent;
    const byte = unitToByte(full, units);
    event.preventDefault();
    event.stopImmediatePropagation();
    semantic?.(item.dataset.editor, `notes.block-text:${block}`, 'selection', '', byte, byte);
  }, {capture: true});

  return {
    present,
    stats,
    // The scroller under the page point `x`, `y` that can still move by `dy`
    // (by any amount for a `dy` of 0), innermost first.
    scrollerAt(x, y, dy) {
      let found;
      for (const {element: node} of scrollers.values()) {
        const r = node.getBoundingClientRect();
        if (x < r.left || x >= r.right || y < r.top || y >= r.bottom) continue;
        const room = dy > 0 ? node.scrollHeight - node.clientHeight - node.scrollTop
          : dy < 0 ? node.scrollTop : node.scrollHeight - node.clientHeight;
        if (room <= 0) continue;
        if (!found || found.contains(node)) found = node;
      }
      return found;
    },
  };
}

// The lift a window or a panel stands at over the shell.
const FLOATING_SHADOW = '0 1px 3px rgba(0,0,0,.07),0 8px 22px rgba(0,0,0,.09)';

// How the rendered composition looks beyond what each widget declares: the
// document's blocks, the caret, the page's own selection. Colours and sizes
// come from the theme's variables the runtime publishes on the layer.
export const VIEW_CSS = `
/* Every size the composition gives is the layout's, which counts padding in. */
.tilo-view,.tilo-view *{box-sizing:border-box;}
/* A row's or a place's controls show while the pointer is over it or focus is in it, as painted. */
.tilo-view [data-hover-controls]:not(:hover):not(:focus-within) [data-widget=HoverToolbarFace]{opacity:0;pointer-events:none;}
.tilo-view [data-hover-controls]:is(:hover,:focus-within) [data-widget=HoverToolbarFace]{transition:opacity .12s ease;}
/* An entry scrolls its words without a bar of its own, as painted. */
.tilo-view textarea{scrollbar-width:none;}
/* A scroller's bar is a thin quiet thumb, not the platform's arrows and trough. */
.tilo-view{scrollbar-width:thin;scrollbar-color:color-mix(in srgb,var(--tilo-text-dim,#6e6e73) 35%,transparent) transparent;}
.tilo-view{font-family:var(--tilo-faces,"Tilo Sans",system-ui,sans-serif);color:var(--tilo-text,#1d1d1f);--tilo-mono:ui-monospace,"SF Mono",Menlo,monospace;font-synthesis:none;-webkit-font-smoothing:antialiased;}
.tilo-view button{all:unset;box-sizing:border-box;display:block;cursor:default;}
.tilo-link{color:var(--tilo-accent,#2f6fed);text-decoration:underline;text-underline-offset:2px;cursor:pointer;}
.tilo-link:hover{color:var(--tilo-accent-hover,var(--tilo-accent,#2f6fed));}
/* A control the person cannot use is dimmed; a box or a switch says so by its own inks. */
.tilo-view [aria-disabled=true]:not(label){opacity:.45;}
.tilo-view [data-widget=WindowNoticeStrip]>[aria-hidden]{border-radius:var(--tilo-radius,8px);box-shadow:0 0 0 1px var(--tilo-border,rgba(127,127,127,.25)),0 6px 20px rgba(0,0,0,.12);}
.tilo-view button:focus-visible{outline:2px solid var(--tilo-accent,#2f6fed);outline-offset:-2px;}
.tilo-view ul,.tilo-view ol{margin:0;padding:0;list-style:none;}
/* A page's own boxes, switches, radios and ranges, set in the toolkit's look: a box that
   fills with the accent and its tick, a switch whose knob slides, a range in the accent. */
.tilo-view input[type=checkbox],.tilo-view input[type=radio]{appearance:none;-webkit-appearance:none;margin:0;flex:none;width:18px;height:18px;border-radius:5px;background:var(--tilo-raised,#f1f2f4);box-shadow:inset 0 0 0 1.5px color-mix(in srgb,var(--tilo-text-dim,#6e6e73) 55%,transparent);display:grid;place-content:center;cursor:pointer;transition:background .15s ease,box-shadow .15s ease;}
.tilo-view input[type=radio]{border-radius:50%;}
.tilo-view input[type=checkbox]:checked,.tilo-view input[type=radio]:checked,.tilo-view input[type=checkbox]:indeterminate{background:var(--tilo-accent,#2f6fed);box-shadow:none;}
.tilo-view input[type=checkbox]:checked::after{content:"";width:9px;height:5px;border:2px solid var(--tilo-on-accent,#fff);border-top:0;border-right:0;transform:translateY(-1px) rotate(-45deg);}
.tilo-view input[type=checkbox]:indeterminate::after{content:"";width:9px;height:2px;background:var(--tilo-on-accent,#fff);border-radius:1px;}
.tilo-view input[type=radio]:checked::after{content:"";width:7px;height:7px;border-radius:50%;background:var(--tilo-on-accent,#fff);}
.tilo-view input[role=switch]{width:36px;height:22px;border-radius:11px;place-content:center start;padding:0 3px;}
.tilo-view input[role=switch]::after{content:"";width:16px;height:16px;border-radius:50%;background:#fff;box-shadow:0 1px 2px rgba(0,0,0,.25);transition:transform .18s ease;}
.tilo-view input[role=switch]:checked::after{width:16px;height:16px;border:0;transform:translateX(14px);}
.tilo-view input:disabled{cursor:default;}
/* A box or switch the person cannot change is grey where it would be the accent, as painted. */
.tilo-view input[type=checkbox]:disabled:checked,.tilo-view input[type=radio]:disabled:checked,.tilo-view input[type=checkbox]:disabled:indeterminate{background:color-mix(in srgb,var(--tilo-text-dim,#6e6e73) 35%,var(--tilo-raised,#e5e7eb));}
.tilo-view input[type=checkbox]:disabled:not(:checked),.tilo-view input[type=radio]:disabled:not(:checked){opacity:.5;}
.tilo-view input[type=checkbox]:focus-visible,.tilo-view input[type=radio]:focus-visible,.tilo-view input[type=range]:focus-visible{outline:2px solid var(--tilo-accent,#2f6fed);outline-offset:2px;}
.tilo-view input[type=range]{flex:1;min-width:0;margin:0;height:20px;appearance:none;-webkit-appearance:none;background:transparent;cursor:pointer;}
/* The track filled in the accent up to the knob, and the knob a white disc on its ring, as painted. */
.tilo-view input[type=range]::-webkit-slider-runnable-track{height:4px;border-radius:2px;background:linear-gradient(to right,var(--tilo-accent,#2f6fed) var(--tilo-along,0%),var(--tilo-raised,#e5e7eb) var(--tilo-along,0%));}
.tilo-view input[type=range]::-moz-range-track{height:4px;border-radius:2px;background:var(--tilo-raised,#e5e7eb);}
.tilo-view input[type=range]::-moz-range-progress{height:4px;border-radius:2px;background:var(--tilo-accent,#2f6fed);}
.tilo-view input[type=range]::-webkit-slider-thumb{-webkit-appearance:none;width:18px;height:18px;margin-top:-7px;border-radius:50%;background:#fff;box-shadow:0 0 0 1.5px color-mix(in srgb,var(--tilo-text-dim,#6e6e73) 45%,transparent),0 1px 2px rgba(0,0,0,.2);}
.tilo-view input[type=range]::-moz-range-thumb{width:18px;height:18px;border:0;border-radius:50%;background:#fff;box-shadow:0 0 0 1.5px color-mix(in srgb,var(--tilo-text-dim,#6e6e73) 45%,transparent),0 1px 2px rgba(0,0,0,.2);}
/* Entries, choosers and strips of choices on the raised well the canvas paints them on. */
.tilo-view [data-tilo-act=step]{width:28px!important;height:28px!important;background:var(--tilo-raised,#f1f2f4)!important;color:var(--tilo-text,#1d1d1f)!important;font-size:15px!important;}
.tilo-view [data-tilo-act=step]::before,.tilo-view [data-tilo-act=step]:not([data-tilo-step-by^="-"])::after{content:"";position:absolute;left:50%;top:50%;width:10px;height:1.75px;border-radius:1px;background:currentColor;transform:translate(-50%,-50%);}
.tilo-view [data-tilo-act=step]:not([data-tilo-step-by^="-"])::after{width:1.75px;height:10px;}
.tilo-view [data-tilo-act=step]:hover{background:color-mix(in srgb,var(--tilo-raised,#f1f2f4) 85%,var(--tilo-text,#1d1d1f) 8%)!important;}
.tilo-view [data-tilo-act=step]:disabled{opacity:.45;}
.tilo-view [data-tilo-act=value][role=spinbutton],.tilo-view input[type=date],.tilo-view input[type=time],.tilo-view input[type=search]{height:28px;border-radius:6px!important;background:var(--tilo-raised,#f1f2f4)!important;padding:0 8px!important;text-align:center;}
.tilo-view input[type=search]{text-align:start;}
.tilo-view [role=combobox]{background:var(--tilo-raised,#f1f2f4);box-shadow:inset 0 0 0 1px var(--tilo-border,rgba(127,127,127,.25));}
.tilo-view [role=combobox]:hover{background:color-mix(in srgb,var(--tilo-raised,#f1f2f4) 85%,var(--tilo-text,#1d1d1f) 8%);}
.tilo-view [role=radiogroup]{background:var(--tilo-raised,#f1f2f4);border-radius:var(--tilo-radius,8px);padding:2px;box-shadow:inset 0 0 0 1px var(--tilo-border,rgba(127,127,127,.25));flex-wrap:nowrap!important;}
.tilo-view [role=radiogroup]>button,.tilo-view [role=tablist]>button,.tilo-view [role=menu]>button,.tilo-view li[role=option]{font-size:13px;font-weight:500;line-height:1.3;}
.tilo-view [role=radiogroup]>button{padding:4px 14px!important;color:var(--tilo-text-dim,#6e6e73);background:transparent!important;}
.tilo-view [role=radiogroup]>button[aria-checked=true]{background:var(--tilo-ground,#fff)!important;color:var(--tilo-text,#1d1d1f);font-weight:600;box-shadow:0 1px 2px rgba(0,0,0,.12);}
.tilo-view [role=tablist]{flex-wrap:nowrap!important;}
.tilo-view [role=tablist]>button{color:var(--tilo-text-dim,#6e6e73);}
.tilo-view [role=tablist]>button[aria-selected=true]{color:var(--tilo-text,#1d1d1f);font-weight:600;background:var(--tilo-raised,rgba(127,127,127,.16));}
/* A control over what its widget paints is only where a press lands: the picture shows it. */
.tilo-view [data-widget=Target]{background:transparent!important;box-shadow:none!important;}
/* What the shell paints as cards, rows and keys, as elements: the lift and the edge
   come from the theme's own variables, the words from each part's own look. */
.tilo-view [data-widget=NotificationBanner]{box-sizing:border-box;padding:12px 14px;background:var(--tilo-raised,rgba(127,127,127,.08));color:var(--tilo-text,#1d1d1f);border-radius:var(--tilo-radius,8px);box-shadow:0 8px 24px rgba(0,0,0,.16),inset 0 0 0 1px var(--tilo-border,rgba(127,127,127,.25));overflow:hidden;}
.tilo-view [data-widget=HungAppCard]{box-sizing:border-box;padding:16px;background:var(--tilo-overlay,#f5f5f7);color:var(--tilo-text,#1d1d1f);border-radius:var(--tilo-radius,8px);box-shadow:0 16px 40px rgba(0,0,0,.24),inset 0 0 0 1px var(--tilo-border,rgba(127,127,127,.25));}
.tilo-view [data-widget=ArrivalHint]{display:flex;align-items:center;justify-content:center;box-sizing:border-box;padding:0 16px;border-radius:9999px;background:var(--tilo-overlay,#f5f5f7);color:var(--tilo-text,#1d1d1f);box-shadow:0 8px 24px rgba(0,0,0,.16),inset 0 0 0 1px var(--tilo-border,rgba(127,127,127,.25));overflow:hidden;}
.tilo-view [data-widget=LaunchpadCard]{box-sizing:border-box;background:var(--tilo-raised,rgba(127,127,127,.08));border-radius:var(--tilo-radius,8px);transition:background .12s ease;}
.tilo-view [data-widget=LaunchpadCard]:hover{background:color-mix(in srgb,var(--tilo-raised,rgba(127,127,127,.08)) 80%,var(--tilo-text,#1d1d1f) 6%);}
.tilo-view [data-widget=PowerMenuRow]{box-shadow:inset 0 0 0 1px var(--tilo-border,rgba(127,127,127,.25));}
.tilo-view [data-widget=PowerMenuRow]:hover{background:color-mix(in srgb,var(--tilo-overlay,#f5f5f7) 80%,var(--tilo-text,#1d1d1f) 6%);}
.tilo-view [data-widget=PowerMenuTitle]{box-sizing:border-box;padding:0 12px;display:flex;align-items:center;}
.tilo-view [data-widget=PasscodeKey]{box-sizing:border-box;background:var(--tilo-raised,rgba(127,127,127,.08));border-radius:var(--tilo-radius,8px);box-shadow:inset 0 0 0 1px var(--tilo-border,rgba(127,127,127,.25));display:flex;align-items:center;justify-content:center;}
.tilo-view li{list-style:none;box-sizing:border-box;}
.tilo-view li:has(>.tilo-words){display:flex;align-items:center;border-radius:var(--tilo-radius,8px);}
.tilo-view li[aria-selected=true]:not([data-widget=NavRow]){background:var(--tilo-raised);}
.tilo-view li[data-widget=NavRow][aria-selected=true]{background:transparent;}
.tilo-view li:hover:not(:has(li:hover)){background:color-mix(in srgb,var(--tilo-raised) 70%,transparent);}
.tilo-words{min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:pre;}
.tilo-view h1,.tilo-view h2,.tilo-view h3,.tilo-view h4,.tilo-view h5,.tilo-view h6,.tilo-view p,.tilo-view blockquote,.tilo-view pre{margin:0;}
.tilo-view hr{border:0;border-top:1px solid var(--tilo-border,rgba(127,127,127,.25));margin:0;align-self:stretch;}
.tilo-view dl,.tilo-view dd{margin:0;}
/* A table's head stands over a rule, and its chosen row on the selection's tint, as painted. */
.tilo-view thead{box-shadow:inset 0 -1px 0 var(--tilo-border,rgba(127,127,127,.25));}
.tilo-view tr[aria-selected=true]{background:color-mix(in srgb,var(--tilo-accent,#2f6fed) 10%,transparent);border-radius:var(--tilo-radius,8px);}
.tilo-view tbody tr:hover:not([aria-selected=true]){background:color-mix(in srgb,var(--tilo-raised) 70%,transparent);border-radius:var(--tilo-radius,8px);}
.tilo-view img{object-fit:cover;overflow:clip;}
.tilo-view img[data-fit=contain]{object-fit:contain;}
.tilo-view img[data-fit=fill]{object-fit:fill;}
.tilo-view progress{accent-color:var(--tilo-accent,#2f6fed);}
/* Four dots orbit, the leading one full and each after it fainter, as painted. */
.tilo-view [data-widget=Spinner]{background:radial-gradient(10% 10% at 50% 15%,var(--tilo-accent,#2f6fed) 90%,transparent 100%),radial-gradient(10% 10% at 85% 50%,color-mix(in srgb,var(--tilo-accent,#2f6fed) 75%,transparent) 90%,transparent 100%),radial-gradient(10% 10% at 50% 85%,color-mix(in srgb,var(--tilo-accent,#2f6fed) 50%,transparent) 90%,transparent 100%),radial-gradient(10% 10% at 15% 50%,color-mix(in srgb,var(--tilo-accent,#2f6fed) 25%,transparent) 90%,transparent 100%);animation:tilo-spin 1.5s linear infinite reverse;}
.tilo-view [data-widget=Skeleton]{background:var(--tilo-raised,rgba(127,127,127,.1));border-radius:6px;overflow:hidden;position:relative;}
.tilo-view [data-widget=Skeleton]::after{content:"";position:absolute;inset:0;background:linear-gradient(90deg,transparent,color-mix(in srgb,var(--tilo-text-dim,#6e6e73) 10%,transparent),transparent);transform:translateX(-100%);animation:tilo-shimmer 1.6s ease-in-out infinite;}
@keyframes tilo-spin{to{transform:rotate(1turn);}}
@keyframes tilo-shimmer{to{transform:translateX(100%);}}
@media (prefers-reduced-motion:reduce){.tilo-view [data-widget=Spinner],.tilo-view [data-widget=Skeleton]::after{animation:none;}}
.tilo-doc .tilo-block{margin:0;padding:0;white-space:pre-wrap;overflow-wrap:anywhere;position:relative;}
.tilo-doc li.tilo-block{list-style:none;padding-left:calc(var(--tilo-list,28px) + var(--tilo-indent,0) * var(--tilo-step,28px));}
.tilo-doc li.tilo-bullet::before{content:"";position:absolute;left:calc(var(--tilo-list,28px) / 2 - 2.5px + var(--tilo-indent,0) * var(--tilo-step,28px));top:calc(0.5lh - 2.5px);width:5px;height:5px;border-radius:50%;background:var(--tilo-text-dim,#6e6e73);}
.tilo-doc li.tilo-numbered::before{content:attr(data-ordinal) ".";position:absolute;left:calc(var(--tilo-indent,0) * var(--tilo-step,28px));width:calc(var(--tilo-list,28px) - 8px);text-align:right;color:var(--tilo-text-dim,#6e6e73);font-variant-numeric:tabular-nums;}
.tilo-doc li.tilo-check .tilo-check{position:absolute;left:calc(var(--tilo-list,28px) / 2 - 7.5px + var(--tilo-indent,0) * var(--tilo-step,28px));top:0.45em;margin:0;width:15px;height:15px;accent-color:var(--tilo-accent,#2f6fed);}
.tilo-doc li.tilo-done{color:var(--tilo-text-dim,#6e6e73);text-decoration:line-through;}
.tilo-doc blockquote.tilo-block{border-left:3px solid var(--tilo-accent,#2f6fed);padding-left:calc(var(--tilo-quote,16px) - 3px);color:var(--tilo-text-dim,#6e6e73);}
.tilo-doc pre.tilo-block{font-family:var(--tilo-mono);background:var(--tilo-raised,rgba(127,127,127,.08));border-radius:6px;padding:10px 12px;}
.tilo-doc hr.tilo-block{border:0;border-top:1px solid var(--tilo-line,rgba(127,127,127,.25));margin:12px 0;padding:0;}
.tilo-doc code{font-family:var(--tilo-mono);font-size:.9em;background:var(--tilo-raised,rgba(127,127,127,.12));border-radius:4px;padding:1px 4px;}
.tilo-doc a{color:var(--tilo-accent,#2f6fed);text-decoration:underline;text-underline-offset:2px;}
.tilo-doc .tilo-mention{color:var(--tilo-accent,#2f6fed);}
.tilo-doc .tilo-selected{background:color-mix(in srgb,var(--tilo-accent,#2f6fed) 28%,transparent);}
.tilo-caret{display:inline-block;width:0;height:1.2em;margin:0 -1px;border-left:2px solid var(--tilo-accent,#2f6fed);vertical-align:text-bottom;animation:tilo-blink 1.06s steps(1) infinite;}
@keyframes tilo-blink{50%{opacity:0;}}
@media (prefers-reduced-motion:reduce){.tilo-caret{animation:none;}}
`;
