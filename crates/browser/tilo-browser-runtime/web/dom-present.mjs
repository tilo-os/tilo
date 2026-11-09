// Shows a mount's frames as HTML: the runtime hands each frame's scene over as
// DOM operations (tilo_cpu_raster::dom) and this builds them into elements —
// boxes, SVG paths, clips, shadows and real text set in the toolkit's own font
// files. Elements are kept from frame to frame by position and only the
// attributes that changed are written, so an unchanged frame writes nothing.
const SVG = 'http://www.w3.org/2000/svg';

export function createDomPresenter(layer, {onFrame} = {}) {
  // Each registered family's ascent and descent, in ems.
  const fonts = new Map();
  // The elements of the last frame, by the path of op indices that reached them.
  let previous = new Map();
  const stats = {frames: 0, lastMs: 0, elements: 0, writes: 0};

  function registerFonts(list) {
    for (const [family, bytes, index, ascent, descent] of list) {
      if (fonts.has(family)) continue;
      fonts.set(family, {ascent, descent});
      // A collection's later faces would need the subtable split out; the
      // prototype registers the file as is, which shows its first face.
      void index;
      const face = new FontFace(family, bytes);
      document.fonts.add(face);
      face.load().catch(error => console.warn('dom presenter: font', family, error));
    }
  }

  function element(key, tag, svg) {
    let node = previous.get(key);
    if (!node || node.tiloTag !== tag) {
      node = svg ? document.createElementNS(SVG, tag) : document.createElement(tag);
      node.tiloTag = tag;
      node.tiloProps = {};
    }
    return node;
  }

  function set(node, prop, value) {
    if (node.tiloProps[prop] === value) return;
    node.tiloProps[prop] = value;
    stats.writes++;
    if (prop === 'text') node.textContent = value;
    else if (prop.startsWith('@')) node.setAttribute(prop.slice(1), value);
    else node.style.setProperty(prop, value);
  }

  const px = v => `${Math.round(v * 100) / 100}px`;

  function place(node, x, y, w, h, origin) {
    set(node, 'left', px(x - origin[0]));
    set(node, 'top', px(y - origin[1]));
    if (w !== undefined) { set(node, 'width', px(w)); set(node, 'height', px(h)); }
  }

  function present(metaJson, opsJson, newFonts) {
    const started = performance.now();
    const meta = JSON.parse(metaJson);
    const ops = JSON.parse(opsJson);
    registerFonts(newFonts);
    const next = new Map();
    stats.writes = 0;
    // Each open clip or group: its element, its origin and the children it gathers.
    const stack = [{node: layer, origin: [0, 0], children: []}];
    let i = 0;
    for (const op of ops) {
      const top = stack[stack.length - 1];
      const key = `${stack.length}:${i++}`;
      const kind = op[0];
      if (kind === 'e') {
        if (stack.length > 1) {
          const done = stack.pop();
          sync(done.node, done.children);
        }
        continue;
      }
      let node;
      if (kind === 'r') {
        const [, x, y, w, h, radius, fill, stroke, width] = op;
        node = element(key, 'div');
        set(node, 'position', 'absolute');
        place(node, x, y, w, h, top.origin);
        set(node, 'box-sizing', 'border-box');
        set(node, 'background', fill || 'transparent');
        set(node, 'border-radius', radius ? px(radius) : '0');
        set(node, 'border', stroke ? `${px(width)} solid ${stroke}` : 'none');
      } else if (kind === 'p' || kind === 'i') {
        const [, x, y, w, h, d, fill, stroke, width, evenOdd] = op;
        if (kind === 'i') {
          node = element(key, 'div');
          set(node, 'position', 'absolute');
          place(node, x, y, w, h, top.origin);
          set(node, 'background', 'repeating-linear-gradient(45deg,#8882 0 6px,#8884 6px 12px)');
        } else {
          node = element(key, 'svg', true);
          set(node, 'position', 'absolute');
          set(node, 'overflow', 'visible');
          place(node, x, y, Math.max(w, 1), Math.max(h, 1), top.origin);
          set(node, '@width', String(Math.max(Math.ceil(w), 1)));
          set(node, '@height', String(Math.max(Math.ceil(h), 1)));
          let path = node.firstChild;
          if (!path) { path = document.createElementNS(SVG, 'path'); path.tiloProps = {}; node.append(path); }
          set(path, '@d', d);
          set(path, '@fill', fill || 'none');
          set(path, '@fill-rule', evenOdd ? 'evenodd' : 'nonzero');
          set(path, '@stroke', stroke || 'none');
          set(path, '@stroke-width', String(width));
        }
      } else if (kind === 't') {
        const [, text, x, y, size, family, variations, color, advance] = op;
        node = element(key, 'span');
        set(node, 'position', 'absolute');
        set(node, 'white-space', 'pre');
        set(node, 'font-family', `"${family}"`);
        set(node, 'font-size', px(size));
        set(node, 'line-height', '1');
        set(node, 'font-variation-settings', variations || 'normal');
        set(node, 'color', color);
        // A span's box starts at its top; the op names the baseline. With a
        // line height of 1 em the browser centres the font's ascent plus
        // descent in that em, so the baseline lies this far below the top.
        const metrics = fonts.get(family) ?? {ascent: 0.8, descent: 0.2};
        const baseline = size * ((1 - metrics.ascent - metrics.descent) / 2 + metrics.ascent);
        place(node, x, y - baseline, undefined, undefined, top.origin);
        set(node, 'text', text);
        node.tiloAdvance = advance;
      } else if (kind === 's') {
        const [, x, y, w, h, radius, blur, color] = op;
        node = element(key, 'div');
        set(node, 'position', 'absolute');
        place(node, x, y, w, h, top.origin);
        set(node, 'border-radius', px(radius));
        set(node, 'box-shadow', `0 0 ${px(blur * 2)} ${color}`);
      } else if (kind === 'c' || kind === 'cp' || kind === 'g') {
        node = element(key, 'div');
        set(node, 'position', 'absolute');
        let origin = top.origin;
        if (kind === 'g') {
          set(node, 'left', '0px'); set(node, 'top', '0px');
          set(node, 'opacity', String(op[1]));
        } else {
          const [, x, y, w, h, extra, opacity] = op;
          place(node, x, y, w, h, top.origin);
          origin = [x, y];
          set(node, 'overflow', 'hidden');
          if (kind === 'c') set(node, 'border-radius', extra ? px(extra) : '0');
          else set(node, 'clip-path', `path("${extra}")`);
          set(node, 'opacity', String(opacity));
        }
        next.set(key, node);
        top.children.push(node);
        stack.push({node, origin, children: []});
        continue;
      }
      if (node) { next.set(key, node); top.children.push(node); }
    }
    while (stack.length > 1) { const done = stack.pop(); sync(done.node, done.children); }
    sync(layer, stack[0].children);
    previous = next;
    set(layer, 'background', meta.background);
    stats.frames++;
    stats.elements = next.size;
    stats.lastMs = performance.now() - started;
    stats.meta = meta;
    onFrame?.(stats);
  }

  // Make `parent`'s children exactly `children`, in order, moving as few as it can.
  function sync(parent, children) {
    let at = parent.firstChild;
    for (const child of children) {
      if (at === child) { at = at.nextSibling; continue; }
      parent.insertBefore(child, at);
    }
    while (at) { const next = at.nextSibling; at.remove(); at = next; }
  }

  layer.tiloProps = {};
  return {present, stats};
}
