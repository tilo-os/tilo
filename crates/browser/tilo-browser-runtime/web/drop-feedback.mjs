/**
 * What the page shows while files from the computer are dragged over the
 * desk and once they are dropped: the place a drop would go, outlined (the
 * window under the pointer, or a bar between the strip's windows where one
 * opens) with what dropping there does, or why nothing there takes files;
 * then, at the place they fell, how far the copy into the workspace is and
 * what it added.
 *
 * Both are page elements over the canvas: drawing them costs the canvas no
 * frame, and moving them while the pointer moves is a style change.
 */
import {dropLabel} from './upload-files.mjs';

/** How long the end of a drop stays at its place. */
const SHOWN_DONE_MS = 6000;
const SHOWN_FAILED_MS = 10000;
/** The gap kept between what the page shows and the page's edges. */
const EDGE = 16;

/** Where a box `width` × `height` hung at the point `(x, y)` stands so it is
 * whole inside a page `page.width` × `page.height`: centred above the point,
 * pushed in from any edge it would cross. */
export function placeNear({x, y}, {width, height}, page) {
  const left = Math.min(Math.max(EDGE, x - width / 2), Math.max(EDGE, page.width - EDGE - width));
  const above = y - height - 12;
  const top = above >= EDGE ? above : Math.min(y + 12, Math.max(EDGE, page.height - EDGE - height));
  return {left: Math.round(left), top: Math.round(top)};
}

/** What the progress at a drop's place says: the copy as it goes, with how
 * far it is, then what it added, or why it did not. */
export function progressText({phase, done, total, text}) {
  if (phase !== 'copying') return text;
  const share = total > 0 ? Math.min(99, Math.floor((done / total) * 100)) : 0;
  return `${text} · ${share}%`;
}

const STYLE = `
.tilo-drop-target{position:fixed;pointer-events:none;z-index:2147483000;box-sizing:border-box;border:2px solid #5b8def;border-radius:10px;background:rgba(91,141,239,.12);transition:left 60ms,top 60ms,width 60ms,height 60ms}
.tilo-drop-target[data-kind=strip]{border-radius:3px;background:#5b8def;border:none}
.tilo-drop-target[data-kind=desk]{border-style:dashed}
.tilo-drop-target[data-kind=refused]{border:none;background:none}
.tilo-drop-note,.tilo-drop-progress{position:fixed;pointer-events:none;z-index:2147483001;box-sizing:border-box;max-width:min(360px,calc(100vw - 32px));padding:7px 12px;border-radius:9px;font:500 13px/1.35 system-ui,sans-serif;color:#fff;background:rgba(24,26,32,.94);box-shadow:0 4px 16px rgba(0,0,0,.28);white-space:normal;overflow-wrap:anywhere}
.tilo-drop-note[data-refused]{background:rgba(120,32,32,.95)}
.tilo-drop-progress[data-phase=failed]{background:rgba(120,32,32,.95)}
.tilo-drop-progress .bar{height:3px;margin-top:6px;border-radius:2px;background:rgba(255,255,255,.22);overflow:hidden}
.tilo-drop-progress .bar>i{display:block;height:100%;width:0;background:#8fb3ff;transition:width 120ms}
.tilo-drop-progress[data-phase=done] .bar,.tilo-drop-progress[data-phase=failed] .bar{display:none}
`;

/** The drop feedback over `canvas`, its elements in `document`. */
export function createDropFeedback({canvas, document = globalThis.document, view = globalThis}) {
  let style = null;
  let outline = null;
  let note = null;
  const ensure = () => {
    if (style) return;
    style = document.createElement('style');
    style.textContent = STYLE;
    document.head.append(style);
    outline = document.createElement('div');
    outline.className = 'tilo-drop-target';
    note = document.createElement('div');
    note.className = 'tilo-drop-note';
    outline.hidden = note.hidden = true;
    document.body.append(outline, note);
  };
  const page = () => ({width: view.innerWidth, height: view.innerHeight});
  /** A place in the canvas's pixels, in the page's. */
  const onPage = target => {
    const rect = canvas.getBoundingClientRect();
    const scaleX = rect.width / (canvas.width || 1), scaleY = rect.height / (canvas.height || 1);
    return {left: rect.left + target.x * scaleX, top: rect.top + target.y * scaleY, width: target.width * scaleX, height: target.height * scaleY};
  };
  const hang = (element, x, y) => {
    const {left, top} = placeNear({x, y}, {width: element.offsetWidth, height: element.offsetHeight}, page());
    element.style.left = `${left}px`;
    element.style.top = `${top}px`;
  };
  return {
    /** Files are dragged over `(clientX, clientY)`, where the desk says
     * `target` is. */
    over(target, clientX, clientY) {
      ensure();
      const label = dropLabel(target);
      const placed = target && target.kind !== 'refused' && Number.isFinite(target.x);
      // A strip with no window to open between is the whole desk, outlined as a window is.
      const kind = target?.kind === 'strip' && target.width > 24 ? 'desk' : target?.kind;
      outline.dataset.kind = kind === 'window' && !target.takes ? 'refused' : kind ?? 'refused';
      outline.hidden = !placed || outline.dataset.kind === 'refused';
      if (!outline.hidden) {
        const at = onPage(target);
        Object.assign(outline.style, {left: `${at.left}px`, top: `${at.top}px`, width: `${at.width}px`, height: `${at.height}px`});
      }
      note.hidden = !label;
      if (note.textContent !== label) note.textContent = label;
      note.toggleAttribute('data-refused', target?.kind === 'refused' || (target?.kind === 'window' && !target.takes));
      if (label) hang(note, clientX, clientY);
    },
    /** The drag left the desk, or ended. */
    leave() {
      if (!style) return;
      outline.hidden = note.hidden = true;
    },
    /** Something was dropped, or chosen, at `(clientX, clientY)`: what
     * follows how the copy goes there, called with `{phase, done, total,
     * text}`. A refusal says why there and goes. */
    progress(clientX, clientY) {
      ensure();
      const box = document.createElement('div');
      box.className = 'tilo-drop-progress';
      box.setAttribute('role', 'status');
      const text = document.createElement('div');
      const bar = document.createElement('div');
      bar.className = 'bar';
      const fill = document.createElement('i');
      bar.append(fill);
      box.append(text, bar);
      document.body.append(box);
      let timer = null;
      return state => {
        box.dataset.phase = state.phase;
        text.textContent = progressText(state);
        fill.style.width = `${state.total > 0 ? Math.min(100, (state.done / state.total) * 100) : 0}%`;
        hang(box, clientX, clientY);
        if (state.phase === 'done' || state.phase === 'failed') {
          view.clearTimeout(timer);
          timer = view.setTimeout(() => box.remove(), state.phase === 'done' ? SHOWN_DONE_MS : SHOWN_FAILED_MS);
        }
      };
    },
  };
}
