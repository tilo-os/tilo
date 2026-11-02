/**
 * Per-mount Tilo projection, mirroring `tilo_desktop_surface::navigation`.
 * All identities are opaque strings, including u64 IDs.
 */
export interface DesktopRoute { workspace: string; strip: string; group: string; tab: string }
export interface DesktopNavigationSnapshot {
  version: 1;
  generation: string;
  /** `app`: the package id the window shows, as links name it; null for a window that shows no app of its own. The Activity app (`so.tilo.activity`) is one such window. */
  entries: Array<{route: DesktopRoute; app: string | null; label: string; stripLabel: string; workspaceLabel: string}>;
  workspaces: Array<{id: string; label: string; workspace: string; owner: string | null; strips: Array<{id: string; label: string}>}>;
  /** What `launch` can open. */
  applications: Array<{id: string; label: string}>;
  activeWorkspace: string;
  activeStrip: string | null;
  active: DesktopRoute | null;
  widthPermille: number | null;
  maximized: boolean;
  capabilities: {
    select: boolean; width: boolean; maximize: boolean; strips: boolean; commands: boolean;
    globalSearch: boolean; workspaceSelect: boolean; workspaceHistory: boolean; workspaceReplay: boolean;
    close: boolean; closeInactive: boolean; launch: boolean; pan: boolean;
    scroll: boolean; history: boolean;
    /** The surface takes sideways wheel input itself; leave it the wheel rather than turn it into `pan`. */
    swipe: boolean;
    notifications: boolean;
  };
  desktopPan: {workspace: string; strip: string; offset: number; viewport: number; content: number; max: number} | null;
  /** Back and forward through the active tab's own history. */
  history: {canBack: boolean; canForward: boolean} | null;
  scroll: {region: string; offset: number; viewport: number; content: number; max: number} | null;
  /** Banners on screen, notifications kept in the centre, and whether the centre is open; with `capabilities.notifications`. */
  notifications?: {active: number; retained: number; centerOpen: boolean};
}
/**
 * A verb of the shown desktop: the effect id its vocabulary publishes and the
 * arguments its caller gives. The desktop performs the verbs it offers itself;
 * one it does not perform is refused, as is an argument naming something gone.
 */
export interface DesktopVerb { effect: string; args?: Record<string, unknown> }
export interface DesktopNavigationV1 {
  readonly version: 1;
  getSnapshot(): DesktopNavigationSnapshot;
  subscribe(listener: (snapshot: DesktopNavigationSnapshot) => void): () => void;
  /** Verbs queue their local presentation; observe snapshots for rendered pane state.
   * `close-tab` resolves after the tab is gone from a published snapshot; every other
   * verb once the runtime admitted it. */
  dispatch(verb: DesktopVerb): Promise<void>;
}
