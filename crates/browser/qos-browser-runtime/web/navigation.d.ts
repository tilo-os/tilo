/** Per-mount qOS projection. All identities are opaque strings, including u64 IDs. */
export interface DesktopRoute { workspace: string; strip: string; group: string; tab: string }
export type WorkspaceActivityView = 'catch-up' | 'explore' | 'replay' | 'tasks';
export type WorkspaceActivityState = {workspace: string; view: WorkspaceActivityView; playing: boolean; coverage: 'complete' | 'partial'} | null;
/** Last authoritative close in this generation; reset to null when a pane opens.
 * Only dismissed permits returning focus to an external opener. Optional on older hosts. */
export type WorkspaceActivityCloseReason = 'dismissed' | 'workspace-changed' | 'resource-opened' | 'access-changed' | 'disposed';
export interface DesktopNavigationSnapshot {
  version: 1;
  generation: string;
  entries: Array<{route: DesktopRoute; label: string; stripLabel: string; workspaceLabel: string}>;
  workspaces: Array<{id: string; label: string; room: string; owner?: string | null; strips: Array<{id: string; label: string}>}>;
  applications?: Array<{id: string; label: string}>;
  activeWorkspace: string;
  activeStrip: string | null;
  active: DesktopRoute | null;
  widthPermille: number | null;
  maximized: boolean;
  capabilities: {select: boolean; width: boolean; maximize: boolean; strips: boolean; commands?: boolean; close?: boolean; globalSearch?: boolean; workspaceSelect?: boolean; workspaceHistory?: boolean; workspaceReplay?: boolean};
  desktopPan?: {workspace: string; strip: string; offset: number; viewport: number; content: number; max: number} | null;
  workspaceActivity?: WorkspaceActivityState;
  workspaceActivityCloseReason?: WorkspaceActivityCloseReason | null;
  history?: {target: string} | null;
  scroll: {region: string; offset: number; viewport: number; content: number; max: number} | null;
}
export type DesktopScrollAction = {kind: "line" | "page"; value: -1 | 1} | {kind: "absolute"; value: number} | {kind: "start" | "end"};
export type DesktopNavigationCommand = {generation: string} & (
  {kind: "workspace"; workspace: string} |
  {kind: "workspace-activity"; workspace: string; view: WorkspaceActivityView} |
  {kind: "close-workspace-activity"; workspace: string} |
  {kind: "global-search"; query?: string} |
  {kind: "pan"; workspace: string; strip: string; offset: number} |
  {kind: "history"; route: DesktopRoute; target: string} |
  {kind: "close"; route: DesktopRoute} |
  {kind: "select"; route: DesktopRoute} |
  {kind: "strip"; workspace: string; strip: string} |
  {kind: "width"; route: DesktopRoute; permille: number} |
  {kind: "maximize"; route: DesktopRoute; maximized: boolean} |
  {kind: "scroll"; region: string; action: DesktopScrollAction} |
  {kind: "commands"; query: string}
);
export interface DesktopNavigationV1 {
  readonly version: 1;
  getSnapshot(): DesktopNavigationSnapshot;
  subscribe(listener: (snapshot: DesktopNavigationSnapshot) => void): () => void;
  /** Workspace commands queue local presentation; observe snapshots for rendered pane state.
   * Close resolves after terminal success and authoritative topology removal. */
  dispatch(command: DesktopNavigationCommand): Promise<void>;
}
