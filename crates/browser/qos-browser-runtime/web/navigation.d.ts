/** Per-mount qOS projection. All identities are opaque strings, including u64 IDs. */
export interface DesktopRoute { workspace: string; strip: string; group: string; tab: string }
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
  capabilities: {select: boolean; width: boolean; maximize: boolean; strips: boolean; commands?: boolean; globalSearch?: boolean; close?: boolean};
  desktopPan?: {workspace: string; strip: string; offset: number; viewport: number; content: number; max: number} | null;
  history?: {target: string} | null;
  scroll: {region: string; offset: number; viewport: number; content: number; max: number} | null;
}
export type DesktopScrollAction = {kind: "line" | "page"; value: -1 | 1} | {kind: "absolute"; value: number} | {kind: "start" | "end"};
export type DesktopNavigationCommand = {generation: string} & (
  {kind: "global-search"} |
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
  dispatch(command: DesktopNavigationCommand): Promise<void>;
}
