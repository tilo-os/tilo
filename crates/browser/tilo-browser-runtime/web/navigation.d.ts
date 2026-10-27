/**
 * Per-mount Tilo projection, mirroring `tilo_desktop_surface::navigation`.
 * All identities are opaque strings, including u64 IDs.
 */
export interface DesktopRoute { workspace: string; strip: string; group: string; tab: string }
export type WorkspaceActivityView = 'catch-up' | 'explore' | 'replay';
export interface DesktopNavigationSnapshot {
  version: 1;
  generation: string;
  /** `app`: the package id the window shows, as links name it; null for a window that shows no app of its own. The Activity app (`so.tilo.activity`) is one such window. */
  entries: Array<{route: DesktopRoute; app: string | null; label: string; stripLabel: string; workspaceLabel: string}>;
  workspaces: Array<{id: string; label: string; room: string; owner: string | null; strips: Array<{id: string; label: string}>}>;
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
export type DesktopScrollAction = {kind: "line" | "page"; value: -1 | 1} | {kind: "absolute"; value: number} | {kind: "start" | "end"};
export type DesktopNavigationCommand = {generation: string} & (
  {kind: "workspace"; workspace: string} |
  /** Open the Activity app at `view`, or bring its window forward; `author`: show only what this person did, as History names them. */
  {kind: "workspace-activity"; workspace: string; view: WorkspaceActivityView; author?: string} |
  /** Close the Activity app's windows in the workspace. */
  {kind: "close-workspace-activity"; workspace: string} |
  /** Open or close the notification centre over the desktop. */
  {kind: "notification-center"; open: boolean} |
  {kind: "global-search"} |
  {kind: "pan"; workspace: string; strip: string; offset: number} |
  {kind: "history"; route: DesktopRoute; target: "back" | "forward"} |
  {kind: "close"; route: DesktopRoute} |
  {kind: "select"; route: DesktopRoute} |
  {kind: "strip"; workspace: string; strip: string} |
  {kind: "width"; route: DesktopRoute; permille: number} |
  {kind: "maximize"; route: DesktopRoute; maximized: boolean} |
  {kind: "scroll"; region: string; action: DesktopScrollAction} |
  {kind: "commands"; query: string} |
  /** Open an app of the snapshot's `applications` in a new window beside the focused one. */
  {kind: "launch"; app: string} |
  /** Show an app as following a link to it does: focus its window on the shown strip, else open one. */
  {kind: "open"; app: string}
);
export interface DesktopNavigationV1 {
  readonly version: 1;
  getSnapshot(): DesktopNavigationSnapshot;
  subscribe(listener: (snapshot: DesktopNavigationSnapshot) => void): () => void;
  /** Workspace commands queue local presentation; observe snapshots for rendered pane state.
   * Close resolves after terminal success and authoritative topology removal; every other
   * command once the runtime admitted it. */
  dispatch(command: DesktopNavigationCommand): Promise<void>;
}
