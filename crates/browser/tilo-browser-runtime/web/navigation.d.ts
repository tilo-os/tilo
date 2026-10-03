/** Per-mount Tilo projection. All identities are opaque strings, including u64 IDs. */
export interface DesktopRoute { workspace: string; strip: string; group: string; tab: string }
export type WorkspaceActivityView = 'catch-up' | 'explore' | 'replay' | 'tasks';
/** Workspace settings tabs; `permissions` is the one labelled Privacy (every grant in the room). */
export type WorkspaceSettingsTab = 'general' | 'members' | 'agents' | 'permissions' | 'apps';
/** `inSurface`: the runtime draws this view over the desktop itself (the shared history pane), so the page shows none of its own. */
export type WorkspaceActivityState = {workspace: string; view: WorkspaceActivityView; playing: boolean; coverage: 'complete' | 'partial'; inSurface?: boolean} | null;
/** Last authoritative close in this generation; reset to null when a pane opens.
 * Only dismissed permits returning focus to an external opener. Optional on older hosts. */
export type WorkspaceActivityCloseReason = 'dismissed' | 'workspace-changed' | 'resource-opened' | 'access-changed' | 'disposed';
export interface DesktopNavigationSnapshot {
  version: 1;
  generation: string;
  /**
   * Every window. `app` is its app id; `record` the record it shows (null for
   * an app's one package-primary document), which another mount of the page
   * with the same profile, device and room may pass as its `record` to edit
   * the same document live, or `{kind: 'open', app, record}` opens again.
   */
  entries: Array<{route: DesktopRoute; label: string; stripLabel: string; workspaceLabel: string; app?: string | null; record?: string | null}>;
  workspaces: Array<{id: string; label: string; room: string; owner?: string | null; strips: Array<{id: string; label: string}>}>;
  applications?: Array<{id: string; label: string}>;
  activeWorkspace: string;
  activeStrip: string | null;
  active: DesktopRoute | null;
  widthPermille: number | null;
  maximized: boolean;
  capabilities: {select: boolean; width: boolean; maximize: boolean; strips: boolean; commands?: boolean; close?: boolean; open?: boolean; globalSearch?: boolean; workspaceSelect?: boolean; workspaceHistory?: boolean; workspaceReplay?: boolean; notifications?: boolean};
  desktopPan?: {workspace: string; strip: string; offset: number; viewport: number; content: number; max: number} | null;
  workspaceActivity?: WorkspaceActivityState;
  workspaceActivityCloseReason?: WorkspaceActivityCloseReason | null;
  /** Banners on screen, notifications kept in the centre, and whether the centre is open; with `capabilities.notifications`. */
  notifications?: {active: number; retained: number; centerOpen: boolean};
  history?: {target: string} | null;
  /** The workspace settings while they are open over the desktop, and their tab. */
  workspaceSettings?: {workspace: string; tab: WorkspaceSettingsTab} | null;
  scroll: {region: string; offset: number; viewport: number; content: number; max: number} | null;
}
export type DesktopScrollAction = {kind: "line" | "page"; value: -1 | 1} | {kind: "absolute"; value: number} | {kind: "start" | "end"};
export type DesktopNavigationCommand = {generation: string} & (
  {kind: "workspace"; workspace: string} |
  {kind: "workspace-activity"; workspace: string; view: WorkspaceActivityView} |
  {kind: "close-workspace-activity"; workspace: string} |
  /** Open or close the notification centre over the desktop. */
  {kind: "notification-center"; open: boolean} |
  /** The active workspace's settings, over the desktop (closing its Activity). */
  {kind: "workspace-settings"; workspace: string; tab: WorkspaceSettingsTab} |
  {kind: "close-workspace-settings"; workspace: string} |
  {kind: "global-search"; query?: string} |
  {kind: "pan"; workspace: string; strip: string; offset: number} |
  {kind: "history"; route: DesktopRoute; target: string} |
  {kind: "close"; route: DesktopRoute} |
  {kind: "select"; route: DesktopRoute} |
  {kind: "strip"; workspace: string; strip: string} |
  {kind: "width"; route: DesktopRoute; permille: number} |
  {kind: "maximize"; route: DesktopRoute; maximized: boolean} |
  {kind: "scroll"; region: string; action: DesktopScrollAction} |
  {kind: "commands"; query: string} |
  /**
   * An app id or alias, linked first when the core lacks it; opens in the active workspace.
   * With `record` (a local desktop only) the window shows that existing record of the app
   * in the workspace's room, such as another window's or a standalone mount's `record`.
   */
  {kind: "open"; app: string; record?: string}
);
export interface DesktopNavigationV1 {
  readonly version: 1;
  getSnapshot(): DesktopNavigationSnapshot;
  subscribe(listener: (snapshot: DesktopNavigationSnapshot) => void): () => void;
  /** Workspace commands queue local presentation; observe snapshots for rendered pane state.
   * Close resolves after terminal success and authoritative topology removal; open once the
   * command is handed to the topology owner (a local desktop has applied it by then). */
  dispatch(command: DesktopNavigationCommand): Promise<void>;
}
