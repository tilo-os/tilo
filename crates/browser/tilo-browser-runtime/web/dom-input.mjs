// The HTML layer is a mount's hit target (`dom: true`). The browser handles
// what a page handles natively, and everything else goes on to the canvas,
// whose listeners (winit's) hand it to the runtime as before:
// - a wheel over a scroll container that can still move moves it at once;
// - a touch that lands on a scroll container pans it natively, with the
//   browser's momentum and overscroll: the runtime hears the touch begin and
//   end (a tap is still a tap), and a pan the browser takes over ends in a
//   cancel, so the runtime never drags what the browser scrolls;
// - a press is the runtime's, as it was on the canvas: it moves no focus and
//   starts no native selection. While the page selects text natively
//   (`native()`), presses are the page's, and while the person holds such a
//   selection the focus stays where it is, so the hidden text editor does not
//   take the selection away; copying it is the browser's.
// Every other pointer event is re-sent to the canvas as it came, without
// bubbling, so the page's own window-level listeners see it once.

// What winit listens for on the canvas.
const FORWARDED = ['pointerdown', 'pointermove', 'pointerup', 'pointercancel', 'pointerover', 'pointerout', 'contextmenu'];

export function createDomInput({layer, canvas, scrollerAt, native = () => false, view = globalThis}) {
  // Touches that began on a scroll container, by pointer id: their moves are
  // the browser's pan.
  const panning = new Set();
  const listeners = new AbortController();
  const on = (node, type, fn, options = {}) => node.addEventListener(type, fn, {...options, signal: listeners.signal});

  // A native selection being made: from the press that starts it.
  let selecting = false;
  // A selection of the layer's text that the person holds, or is making.
  function holdsSelection() {
    if (selecting) return true;
    const selection = view.getSelection?.();
    if (!selection || selection.isCollapsed || !selection.rangeCount) return false;
    const node = selection.anchorNode;
    return Boolean(node && layer.contains(node));
  }

  function forward(event) {
    const Kind = event.pointerType !== undefined ? view.PointerEvent : view.MouseEvent;
    // An init dictionary reads the event's own properties: position, buttons,
    // modifiers, pointer id and type, pressure.
    const copy = new Kind(event.type, {
      bubbles: false, cancelable: true, composed: false,
      clientX: event.clientX, clientY: event.clientY, screenX: event.screenX, screenY: event.screenY,
      button: event.button, buttons: event.buttons, detail: event.detail,
      ctrlKey: event.ctrlKey, shiftKey: event.shiftKey, altKey: event.altKey, metaKey: event.metaKey,
      pointerId: event.pointerId, pointerType: event.pointerType, isPrimary: event.isPrimary,
      pressure: event.pressure, width: event.width, height: event.height,
      tiltX: event.tiltX, tiltY: event.tiltY,
    });
    canvas.dispatchEvent(copy);
    // The runtime took it the way the canvas would have.
    if (copy.defaultPrevented && event.type === 'contextmenu') event.preventDefault();
  }

  for (const type of FORWARDED) {
    on(layer, type, event => {
      if (native()) return;
      // Crossing from one of the layer's elements to another is no entry or
      // exit of the surface.
      if ((type === 'pointerover' || type === 'pointerout') && event.relatedTarget && layer.contains(event.relatedTarget)) return;
      if (event.pointerType === 'touch') {
        if (type === 'pointerdown' && scrollerAt(event.clientX, event.clientY, 0)) panning.add(event.pointerId);
        if (type === 'pointermove' && panning.has(event.pointerId)) return;
        if (type === 'pointerup' || type === 'pointercancel') panning.delete(event.pointerId);
      }
      forward(event);
    }, {capture: true});
  }
  // A press the runtime takes moves no focus and starts no selection of its
  // own, as on the canvas (winit keeps the focus there): the press's default
  // would take the focus from the runtime's editor to the page.
  on(layer, 'mousedown', event => { if (native()) selecting = event.button === 0; else event.preventDefault(); }, {capture: true});
  on(view, 'mouseup', () => { selecting = false; }, {capture: true});
  // A wheel a scroll container can take moves it in this very event, so the
  // next frame shows the move (the browser's own wheel scroll would wait for
  // this listener, then animate); any other wheel goes to the runtime.
  on(layer, 'wheel', event => {
    if (event.ctrlKey) { forwardWheel(event); return; }
    const dy = event.deltaMode === 1 ? event.deltaY * 40 : event.deltaMode === 2 ? event.deltaY * layer.clientHeight : event.deltaY;
    const scroller = dy && scrollerAt(event.clientX, event.clientY, dy);
    if (scroller) { event.preventDefault(); scroller.scrollTop += dy; return; }
    forwardWheel(event);
  }, {capture: true, passive: false});
  function forwardWheel(event) {
    event.preventDefault();
    canvas.dispatchEvent(new view.WheelEvent('wheel', {
      bubbles: false, cancelable: true,
      clientX: event.clientX, clientY: event.clientY, screenX: event.screenX, screenY: event.screenY,
      deltaX: event.deltaX, deltaY: event.deltaY, deltaZ: event.deltaZ, deltaMode: event.deltaMode,
      ctrlKey: event.ctrlKey, shiftKey: event.shiftKey, altKey: event.altKey, metaKey: event.metaKey,
      buttons: event.buttons,
    }));
  }

  // The runtime sets its cursor on the canvas; the layer over it shows it.
  if (view.MutationObserver) {
    const cursor = () => { layer.style.cursor = canvas.style.cursor; };
    const observer = new view.MutationObserver(cursor);
    observer.observe(canvas, {attributes: true, attributeFilter: ['style']});
    listeners.signal.addEventListener('abort', () => observer.disconnect());
    cursor();
  }

  return {holdsSelection, close: () => listeners.abort()};
}
