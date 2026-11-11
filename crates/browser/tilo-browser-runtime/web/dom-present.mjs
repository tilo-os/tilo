// Shows a mount's frames as HTML: the runtime hands each frame's scene over as
// DOM operations (tilo_cpu_raster::dom) and this builds them into elements —
// boxes, SVG paths, clips, shadows and real text set in the toolkit's own font
// files.
//
// Elements keep their identity by key, never by position in the frame:
// - each widget's draws arrive between a key and its end ("k" … "ke"), and
//   the widget gets one element, kept from frame to frame by that key and
//   placed at the widget's origin;
// - everything else is named by its kind and place among its kind inside the
//   innermost widget or clip, so an op inserted in one widget renumbers
//   nothing outside it, and only its own kind inside;
// - an element is reused only for an op of the same kind, so a box that was
//   a clip never keeps a clip's properties.
// Only properties that changed are written: an unchanged frame writes nothing,
// and a widget that moved is one position write.
//
// A widget that scrolls (the frame lists it in `meta.scrolls`) shows the clip
// of its viewport as a native scroll container: its content is placed where
// it lies in the scrolled content, the container is scrolled to the widget's
// offset, and wheel and scroll bar moves are the browser's own. Where the
// person scrolls goes back to the widget (`onScroll`), which then draws what
// came into view.
//
// The painted elements are hidden from assistive technology one by one; the
// semantic mirror's elements are placed inside the element of the widget they
// describe (`host`), so the page holds one tree for both.
const SVG = 'http://www.w3.org/2000/svg';

export function createDomPresenter(layer, {onFrame, onScroll} = {}) {
  // Each registered family's ascent and descent, in ems.
  const fonts = new Map();
  // The elements of the last frame, by key.
  let previous = new Map();
  // Each widget's element, by its key as the frame named it.
  let widgets = new Map();
  // The scroll containers of the last frame, by widget key.
  let scrollers = new Map();
  // Semantic elements living inside a widget's element, by widget key.
  const hosted = new Map();
  const stats = {frames: 0, lastMs: 0, elements: 0, writes: 0, created: 0};

  function registerFonts(list) {
    for (const [family, bytes, index, ascent, descent] of list) {
      if (fonts.has(family)) continue;
      fonts.set(family, {ascent, descent});
      // A collection's later faces would need their tables split out; the
      // file goes in as it is, which shows its first face.
      void index;
      const face = new FontFace(family, bytes);
      document.fonts.add(face);
      face.load().catch(error => console.warn('dom presenter: font', family, error));
    }
  }

  // The images frames named, by id: each a canvas holding its pixels, which
  // every element showing it draws from.
  const images = new Map();
  function registerImages(list) {
    for (const [image, width, height, rgba] of list) {
      const source = document.createElement('canvas');
      source.width = width; source.height = height;
      source.getContext('2d').putImageData(new ImageData(rgba, width, height), 0, 0);
      source.tiloUsed = stats.frames;
      images.set(image, source);
    }
  }

  function element(key, tag, svg) {
    let node = previous.get(key);
    if (!node || node.tiloTag !== tag) {
      node = svg ? document.createElementNS(SVG, tag) : document.createElement(tag);
      node.tiloTag = tag;
      node.tiloProps = {};
      node.style.position = 'absolute';
      stats.created++;
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

  // A painted leaf is a picture of the interface; what it means is the
  // semantic element's to say.
  function leaf(node) {
    if (node.tiloLeaf) return;
    node.tiloLeaf = true;
    node.setAttribute('aria-hidden', 'true');
  }

  function scrollContainer(node, key) {
    if (node.tiloScroll === key) return;
    node.tiloScroll = key;
    node.addEventListener('scroll', () => {
      const offset = node.scrollTop;
      // The frame placed it there; nothing to report.
      if (Math.abs(offset - (node.tiloShown ?? 0)) < 0.5) return;
      node.tiloShown = offset;
      onScroll?.(key, offset);
    }, {passive: true});
  }

  function present(metaJson, opsJson, newFonts, newImages = []) {
    const started = performance.now();
    const meta = JSON.parse(metaJson);
    const ops = JSON.parse(opsJson);
    registerFonts(newFonts);
    registerImages(newImages);
    if (meta.theme) for (const [name, value] of Object.entries(meta.theme)) layer.style.setProperty(name, value);
    const regions = new Map((meta.scrolls ?? []).map(scroll => [scroll.key, scroll]));
    const next = new Map();
    const nextWidgets = new Map();
    const nextScrollers = new Map();
    const seen = new Map();
    stats.writes = 0;
    stats.created = 0;
    // Each open widget, clip or group: its element, the origin its children
    // are placed from, the scope that names them and the children it gathers.
    const root = {node: layer, origin: [0, 0], scope: '', counts: {}, children: [], kind: 'root'};
    const stack = [root];
    const close = frame => sync(frame.node, frame.children);
    for (const op of ops) {
      const top = stack[stack.length - 1];
      const kind = op[0];
      if (kind === 'ke') {
        // Close the innermost widget, and anything left open inside it.
        const at = stack.findLastIndex(frame => frame.kind === 'k');
        if (at > 0) while (stack.length > at) close(stack.pop());
        continue;
      }
      if (kind === 'e') {
        const at = stack.findLastIndex(frame => frame.kind === 'c');
        if (at > 0) while (stack.length > at) close(stack.pop());
        continue;
      }
      if (kind === 'k') {
        const [, given, x, y] = op;
        // The same widget drawn twice in one frame gets two elements.
        const n = seen.get(given) ?? 0;
        seen.set(given, n + 1);
        const key = n ? `${given}#${n}` : given;
        const node = element(`k:${key}`, 'div');
        place(node, x, y, undefined, undefined, top.origin);
        node.tiloRect = [x, y, 0, 0];
        next.set(`k:${key}`, node);
        if (!n) nextWidgets.set(given, node);
        top.children.push(node);
        stack.push({node, origin: [x, y], scope: `k:${key}`, counts: {}, children: [], kind: 'k', widget: given});
        continue;
      }
      // Named by kind and place among its kind, so an op of one kind
      // inserted renumbers no other kind's.
      const nth = top.counts[kind] ?? 0;
      top.counts[kind] = nth + 1;
      const id = `${top.scope}/${kind}${nth}`;
      let node;
      if (kind === 'r') {
        const [, x, y, w, h, radius, fill, stroke, width] = op;
        node = element(id, 'div');
        place(node, x, y, w, h, top.origin);
        set(node, 'box-sizing', 'border-box');
        set(node, 'background', fill || 'transparent');
        set(node, 'border-radius', radius ? px(radius) : '0');
        set(node, 'border', stroke ? `${px(width)} solid ${stroke}` : 'none');
        // Only text takes the pointer, so a drag that starts on it selects
        // it rather than the box behind it.
        set(node, 'pointer-events', 'none');
      } else if (kind === 'i') {
        const [, x, y, w, h, image, a, b, c, d, e, f, alpha] = op;
        node = element(id, 'div');
        place(node, x, y, w, h, top.origin);
        set(node, 'overflow', 'hidden');
        set(node, 'pointer-events', 'none');
        const source = images.get(image);
        if (source) source.tiloUsed = stats.frames;
        set(node, 'background', source ? 'none' : 'repeating-linear-gradient(45deg,#8882 0 6px,#8884 6px 12px)');
        let picture = node.firstChild;
        if (source) {
          // The image at its own size, drawn once, placed by its matrix.
          if (!picture) { picture = document.createElement('canvas'); picture.tiloProps = {}; picture.style.position = 'absolute'; picture.style.left = '0'; picture.style.top = '0'; picture.style.transformOrigin = '0 0'; node.append(picture); }
          if (picture.tiloImage !== image) {
            picture.tiloImage = image;
            picture.width = source.width; picture.height = source.height;
            picture.getContext('2d').drawImage(source, 0, 0);
          }
          set(picture, 'transform', `matrix(${a},${b},${c},${d},${e - x},${f - y})`);
          set(picture, 'opacity', String(alpha ?? 1));
        } else if (picture) picture.remove();
      } else if (kind === 'p') {
        const [, x, y, w, h, d, fill, stroke, width, evenOdd] = op;
        node = element(id, 'svg', true);
        set(node, 'overflow', 'visible');
        set(node, 'pointer-events', 'none');
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
      } else if (kind === 't') {
        const [, text, x, y, size, family, variations, color] = op;
        node = element(id, 'span');
        set(node, 'white-space', 'pre');
        set(node, 'pointer-events', 'auto');
        // The runtime's cursor, which the layer shows, not the text's own.
        set(node, 'cursor', 'inherit');
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
      } else if (kind === 's') {
        const [, x, y, w, h, radius, blur, color] = op;
        node = element(id, 'div');
        place(node, x, y, w, h, top.origin);
        set(node, 'border-radius', px(radius));
        set(node, 'box-shadow', `0 0 ${px(blur * 2)} ${color}`);
        set(node, 'pointer-events', 'none');
      } else if (kind === 'c' || kind === 'cp' || kind === 'g') {
        node = element(id, 'div');
        let origin = top.origin;
        if (kind === 'g') {
          // A group is transparent: what it holds takes the pointer, not it.
          set(node, 'pointer-events', 'none');
          set(node, 'left', '0px'); set(node, 'top', '0px');
          set(node, 'opacity', String(op[1]));
        } else {
          const [, x, y, w, h, extra, opacity] = op;
          place(node, x, y, w, h, top.origin);
          origin = [x, y];
          set(node, 'opacity', String(opacity));
          if (kind === 'c') set(node, 'border-radius', extra ? px(extra) : '0');
          else set(node, 'clip-path', `path("${extra}")`);
          // The viewport of the widget this clip is drawn in, when that
          // widget scrolls: a native scroll container.
          const widget = stack.findLast(frame => frame.kind === 'k')?.widget;
          const region = kind === 'c' && widget !== undefined && !nextScrollers.has(widget) ? regions.get(widget) : undefined;
          if (region && Math.abs(h - region.viewport) < 1.5) {
            nextScrollers.set(widget, node);
            scrollContainer(node, widget);
            set(node, 'overflow-x', 'hidden');
            set(node, 'overflow-y', 'auto');
            set(node, 'overscroll-behavior', 'contain');
            set(node, 'scrollbar-width', 'none');
            set(node, 'pointer-events', 'auto');
            // A touch pans it as a page's would; the layer around it takes
            // no gesture of its own.
            set(node, 'touch-action', 'pan-x pan-y');
            // Children stand where they lie in the content, which the
            // container then scrolls by the widget's offset.
            origin = [x, y - region.offset];
            node.tiloRegion = region;
          } else {
            set(node, 'overflow', 'hidden');
            set(node, 'pointer-events', 'none');
            set(node, 'touch-action', 'auto');
            node.tiloRegion = undefined;
          }
        }
        next.set(id, node);
        top.children.push(node);
        stack.push({node, origin, scope: id, counts: {}, children: [], kind: 'c'});
        continue;
      }
      if (node) { leaf(node); next.set(id, node); top.children.push(node); }
    }
    while (stack.length > 1) close(stack.pop());
    close(root);
    // Each scroll container: as tall as its content, and scrolled where the
    // widget is unless the person scrolled it on since.
    for (const [key, node] of nextScrollers) {
      const region = node.tiloRegion;
      let extent = node.tiloExtent;
      if (!extent) {
        extent = document.createElement('div');
        extent.style.cssText = 'position:absolute;left:0;width:1px;height:1px;pointer-events:none;visibility:hidden';
        extent.tiloForeign = true;
        node.tiloExtent = extent;
      }
      if (extent.parentNode !== node) node.prepend(extent);
      extent.style.top = px(Math.max(region.content, region.viewport) - 1);
      const asked = region.offset;
      if (node.tiloAsked !== asked) {
        node.tiloAsked = asked;
        if (Math.abs(node.scrollTop - asked) >= 0.5 && Math.abs((node.tiloShown ?? 0) - asked) >= 0.5) {
          node.tiloShown = asked;
          node.scrollTop = asked;
        }
      }
    }
    // An image no frame showed for a while goes; the runtime hands it over
    // again, under a new id, if it comes back.
    for (const [image, source] of images) if (stats.frames - (source.tiloUsed ?? stats.frames) >= 600) images.delete(image);
    previous = next;
    widgets = nextWidgets;
    scrollers = nextScrollers;
    rehost();
    set(layer, 'background', meta.background);
    stats.frames++;
    stats.elements = next.size;
    stats.scrollers = scrollers.size;
    stats.lastMs = performance.now() - started;
    stats.meta = meta;
    stats.ops = opsJson;
    onFrame?.(stats);
  }

  // Make `parent`'s children exactly `children`, in order, moving as few as
  // it can. Elements others placed here (`tiloForeign`: a semantic element,
  // a scroll extent) stay, ahead of the painted ones.
  function sync(parent, children) {
    let at = parent.firstChild;
    while (at?.tiloForeign) at = at.nextSibling;
    for (const child of children) {
      if (at === child) { at = at.nextSibling; continue; }
      parent.insertBefore(child, at);
    }
    while (at) { const following = at.nextSibling; if (!at.tiloForeign) at.remove(); at = following; }
  }

  // Every hosted semantic element in its widget's element of this frame.
  function rehost() {
    for (const [key, entry] of hosted) {
      if (entry.element.tiloState?.active === false) { hosted.delete(key); continue; }
      const home = widgets.get(key);
      if (!home) continue;
      if (entry.element.parentNode !== home) home.prepend(entry.element);
    }
  }

  layer.tiloProps = {};
  return {
    present,
    stats,
    // What the semantic mirror asks (`semanticBridge`'s `host`): the element
    // of the widget whose draws carried `key`, to hold the semantic element
    // that describes it (undefined when the last frame did not draw it);
    // `adopt` keeps that element there as frames replace the widget's
    // element, `release` lets it go.
    host: {
      home: key => widgets.get(key),
      adopt(key, element) {
        if (!element.tiloForeign) {
          element.tiloForeign = true;
          // In the painted layer it must stay unseen, as the mirror's own
          // layer keeps it: no pointer lands on it (the painted layer is the
          // page's hit target, and the pointer is the runtime's), and it
          // stays out of a selection of the painted text.
          element.style.opacity = '0';
          element.style.pointerEvents = 'none';
          if (element.localName !== 'textarea' && element.localName !== 'input') element.style.userSelect = 'none';
        }
        hosted.set(key, {element});
      },
      release(key) { hosted.delete(key); },
    },
    // The scroll container under the page point `x`, `y` that can still move
    // by `dy` (by any amount for a `dy` of 0), innermost first.
    scrollerAt(x, y, dy) {
      let found;
      for (const node of scrollers.values()) {
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
