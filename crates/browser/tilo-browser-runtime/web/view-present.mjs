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
    if (node.own?.kind === 'text') css += element.localName === 'li' ? textStyle(node.own) : textCss(node.own);
    else if (node.own?.kind === 'face') css += faceCss(node.own);
    else if (node.own?.kind === 'field') css += textStyle(node.own.look) + 'margin:0;padding:0;border:0;outline:none;background:transparent;font-family:inherit;';
    write(element, 'css', css);
    // What it is, for assistive technology and for the page's own find,
    // selection and focus.
    const semantics = node.semantics;
    write(element, '@role', role && ARIA_ROLES[role.name] && element.localName !== ARIA_ROLES[role.name] ? ARIA_ROLES[role.name] : null);
    const visibleText = node.own?.kind === 'text' || node.own?.kind === 'field' || node.own?.kind === 'blocks' || Boolean(node.own?.label);
    write(element, '@aria-label', !visibleText && semantics?.name ? semantics.name : null);
    write(element, '@aria-selected', semantics?.state?.selected ? 'true' : null);
    write(element, '@aria-pressed', role?.name === 'ToggleButton' ? String(Boolean(semantics?.state?.checked)) : null);
    write(element, '@aria-expanded', semantics?.state?.expanded === undefined || semantics?.state?.expanded === null ? null : String(semantics.state.expanded));
    write(element, '@aria-disabled', semantics?.state?.disabled ? 'true' : null);
    write(element, '@data-widget', node.widget || null);
    // A notice is read out as it arrives, without moving the keyboard.
    write(element, '@aria-live', semantics?.live ? String(semantics.live).toLowerCase() : null);
    if (element.localName === 'button') write(element, '@type', 'button');
    if (element.localName === 'button') write(element, '@tabindex', '-1');

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

    const scrollTop = scroller ? node.scroll[0] : 0;
    const context = {flows, scroller, rect: node.rect};
    for (const child of node.children ?? []) {
      const [, , cw, ch] = child.rect;
      // Nothing to show of a child with no room (a closed menu, an empty
      // door), and nothing of it to reach.
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
    if (!own || own.kind === 'blocks' || own.kind === 'painted' || own.kind === 'face') place(element, children);
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

  function textCss(look) {
    const clamp = look.max_lines ? (look.max_lines === 1
      ? 'white-space:pre;overflow:hidden;text-overflow:ellipsis;'
      : `display:-webkit-box;-webkit-box-orient:vertical;-webkit-line-clamp:${look.max_lines};overflow:hidden;white-space:pre-wrap;`)
      : 'white-space:pre-wrap;';
    return textStyle(look) + clamp + 'margin:0;overflow-wrap:anywhere;';
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
      write(item, 'css', `--tilo-indent:${block.level};`);
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
        settingScroll.add(element);
        element.scrollTop = offset;
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

// How the rendered composition looks beyond what each widget declares: the
// document's blocks, the caret, the page's own selection. Colours and sizes
// come from the theme's variables the runtime publishes on the layer.
export const VIEW_CSS = `
.tilo-view{font-family:var(--tilo-faces,"Tilo Sans",system-ui,sans-serif);color:var(--tilo-text,#1d1d1f);--tilo-mono:ui-monospace,"SF Mono",Menlo,monospace;font-synthesis:none;-webkit-font-smoothing:antialiased;}
.tilo-view button{all:unset;box-sizing:border-box;display:block;cursor:default;}
.tilo-view [aria-disabled=true]{opacity:.45;}
.tilo-view [data-widget=WindowNoticeStrip]>[aria-hidden]{border-radius:var(--tilo-radius,8px);box-shadow:0 0 0 1px var(--tilo-border,rgba(127,127,127,.25)),0 6px 20px rgba(0,0,0,.12);}
.tilo-view button:focus-visible{outline:2px solid var(--tilo-accent,#2f6fed);outline-offset:-2px;}
.tilo-view ul{margin:0;padding:0;list-style:none;}
.tilo-view li{list-style:none;box-sizing:border-box;}
.tilo-view li:has(>.tilo-words){display:flex;align-items:center;padding:0 12px;border-radius:var(--tilo-radius,8px);}
.tilo-view li[aria-selected=true]{background:var(--tilo-raised);}
.tilo-view li:hover{background:color-mix(in srgb,var(--tilo-raised) 70%,transparent);}
.tilo-words{min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:pre;}
.tilo-view h1,.tilo-view h2,.tilo-view h3,.tilo-view h4,.tilo-view p,.tilo-view blockquote,.tilo-view pre{margin:0;}
.tilo-doc{font-size:16px;line-height:1.6;}
.tilo-doc .tilo-block{margin:0;padding:3px 0;white-space:pre-wrap;overflow-wrap:anywhere;position:relative;}
.tilo-doc .tilo-soft{padding-top:0;}
.tilo-doc h1.tilo-block{font-size:30px;line-height:1.25;font-weight:700;padding:14px 0 6px;letter-spacing:-0.01em;}
.tilo-doc h2.tilo-block{font-size:23px;line-height:1.3;font-weight:650;padding:12px 0 4px;}
.tilo-doc h3.tilo-block{font-size:19px;line-height:1.35;font-weight:600;padding:10px 0 2px;}
.tilo-doc li.tilo-block{list-style:none;padding-left:calc(var(--tilo-list,28px) + var(--tilo-indent,0) * var(--tilo-step,28px));}
.tilo-doc li.tilo-bullet::before{content:"";position:absolute;left:calc(var(--tilo-list,28px) / 2 - 2.5px + var(--tilo-indent,0) * var(--tilo-step,28px));top:calc(3px + 0.8em - 2.5px);width:5px;height:5px;border-radius:50%;background:var(--tilo-text-dim,#6e6e73);}
.tilo-doc li.tilo-numbered::before{content:attr(data-ordinal) ".";position:absolute;left:calc(var(--tilo-indent,0) * var(--tilo-step,28px));width:calc(var(--tilo-list,28px) - 8px);text-align:right;color:var(--tilo-text-dim,#6e6e73);font-variant-numeric:tabular-nums;}
.tilo-doc li.tilo-check .tilo-check{position:absolute;left:calc(var(--tilo-list,28px) / 2 - 7.5px + var(--tilo-indent,0) * var(--tilo-step,28px));top:0.45em;margin:0;width:15px;height:15px;accent-color:var(--tilo-accent,#2f6fed);}
.tilo-doc li.tilo-done{color:var(--tilo-text-dim,#6e6e73);text-decoration:line-through;}
.tilo-doc blockquote.tilo-block{border-left:3px solid var(--tilo-accent,#2f6fed);padding-left:calc(var(--tilo-quote,16px) - 3px);color:var(--tilo-text-dim,#6e6e73);}
.tilo-doc pre.tilo-block{font-family:var(--tilo-mono);font-size:14px;background:var(--tilo-raised,rgba(127,127,127,.08));border-radius:6px;padding:10px 12px;}
.tilo-doc hr.tilo-block{border:0;border-top:1px solid var(--tilo-line,rgba(127,127,127,.25));margin:12px 0;padding:0;}
.tilo-doc code{font-family:var(--tilo-mono);font-size:.9em;background:var(--tilo-raised,rgba(127,127,127,.12));border-radius:4px;padding:1px 4px;}
.tilo-doc a{color:var(--tilo-accent,#2f6fed);text-decoration:underline;text-underline-offset:2px;}
.tilo-doc .tilo-mention{color:var(--tilo-accent,#2f6fed);}
.tilo-doc .tilo-selected{background:color-mix(in srgb,var(--tilo-accent,#2f6fed) 28%,transparent);}
.tilo-caret{display:inline-block;width:0;height:1.2em;margin:0 -1px;border-left:2px solid var(--tilo-accent,#2f6fed);vertical-align:text-bottom;animation:tilo-blink 1.06s steps(1) infinite;}
@keyframes tilo-blink{50%{opacity:0;}}
@media (prefers-reduced-motion:reduce){.tilo-caret{animation:none;}}
`;
