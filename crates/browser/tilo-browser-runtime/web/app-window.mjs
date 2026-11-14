/**
 * Whether the page runs installed as an app window or in a browser tab. A
 * tab's browser keeps Ctrl+W, Ctrl+T, Ctrl+N and Ctrl+Tab for its own tabs;
 * an app window hands them to the page, where the desk answers them as any
 * app does and the shortcut guide shows them (`set_app_window`).
 */

/** The display modes an installed web app opens in. */
export const APP_WINDOW_QUERY = '(display-mode: standalone), (display-mode: window-controls-overlay), (display-mode: minimal-ui)';

/** Tell `runtime` whether the page is an app window now and whenever the
 * browser moves it between a window and a tab. Returns a stop function. */
export function followDisplayMode({runtime, window: win = globalThis}) {
  const query = win.matchMedia?.(APP_WINDOW_QUERY);
  if (!query || typeof runtime?.set_app_window !== 'function') return () => {};
  const tell = () => runtime.set_app_window(Boolean(query.matches));
  tell();
  query.addEventListener?.('change', tell);
  return () => query.removeEventListener?.('change', tell);
}
