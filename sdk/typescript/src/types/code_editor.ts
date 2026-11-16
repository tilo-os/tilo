/**
 * Types for Code Editor (`so.tilo.code-editor`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Project` is a `Resource` of the app's `code_editor.project`, made by its factory with the
 * resource's path (`code-editor/<resource>`): its attributes are the app's fields
 * (read-only ones `readonly`) and its methods are the app's actions, named in camel
 * case and taking the arguments the wire takes them under. An action that takes
 * longer answers a `Run`. A record is the object the wire carries, and an enum whose
 * cases carry fields is an object whose tag the app's schema names. An action whose
 * name is one the interface already has is run with `action("…")`, which the file
 * says where it is. An undoable action that answers nothing also has a `<action>Many`,
which applies it to every argument object of a list in a few all-or-nothing steps,
all of them one run.
 */

import { resourceProxy } from "tilo-sdk";
import type * as tilo from "tilo-sdk";

/** The version of the surface these types were made from: a workspace whose app
 * publishes another refuses their actions rather than run what they no longer
 * describe. */
export const SURFACE = "so.tilo.code-editor@a337d60d96a31d9c";

export interface Entry {
  kind: EntryKind;
  name: string;
  parent: string | null;
  language: string;
}

/** One of "file", "folder". */
export type EntryKind = "file" | "folder";

/** One of "error", "warning", "location", "url", "test-passed", "test-failed", "test-ignored", "change". */
export type ItemKind = "error" | "warning" | "location" | "url" | "test-passed" | "test-failed" | "test-ignored" | "change";

export interface Run {
  command: string;
  cwd: string;
  environment: string | null;
  actor: string;
  for_person: string | null;
  started_ms: number;
  ended_ms: number;
  exit: number;
  output: unknown;
  truncated: boolean;
  tool: string;
  undoable: boolean;
  items: RunItem[];
}

export interface RunItem {
  kind: ItemKind;
  text: string;
  path: string | null;
  line: number | null;
  column: number | null;
  code: string | null;
}

export interface RunRequest {
  command: string;
  requested_by: string;
  for_person: string | null;
  pane: string;
  agent: boolean;
}

export interface Terminal {
  target: string | null;
  relay_session: string | null;
  pane: string | null;
  device: string | null;
  holder: string | null;
  runs: Run[];
  requests: unknown;
}

/** The arguments of `new-file`. */
export interface NewFileArgs {
  name?: string;
  folder?: string;
  text?: string;
}

/** The arguments of `new-folder`. */
export interface NewFolderArgs {
  name?: string;
  folder?: string;
}

/** The arguments of `rename`. */
export interface RenameArgs {
  entry: string;
  name: string;
}

/** The arguments of `move-entry`. */
export interface MoveEntryArgs {
  entry: string;
  folder?: string;
  before?: string;
}

/** The arguments of `replace-text`. */
export interface ReplaceTextArgs {
  file: string;
  text: string;
}

/** The arguments of `replace-range`. */
export interface ReplaceRangeArgs {
  at: unknown;
  text: string;
}

/** The arguments of `delete`. */
export interface DeleteArgs {
  entry: string;
}

/** The arguments of `record-entry`. */
export interface RecordEntryArgs {
  id: string;
  kind: EntryKind;
  name: string;
  language: string;
  folder?: string;
}

/** The arguments of `record-text`. */
export interface RecordTextArgs {
  file: string;
  text: string;
}

/** The arguments of `record-target`. */
export interface RecordTargetArgs {
  target: string;
  device?: string;
}

/** The arguments of `record-relay-session`. */
export interface RecordRelaySessionArgs {
  session?: string;
}

/** The arguments of `claim-pane`. */
export interface ClaimPaneArgs {
  pane: string;
  device?: string;
}

/** The arguments of `stop`. */
export interface StopArgs {
}

/** The arguments of `record-run`. */
export interface RecordRunArgs {
  command: string;
  cwd: string;
  started_ms: number;
  ended_ms: number;
  exit: number;
  truncated: boolean;
  tool: string;
  undoable: boolean;
  items: unknown[];
  run: string;
  output?: unknown;
  request?: string;
}

/** Code Editor — `code_editor.project`. */
export interface Project extends tilo.Resource {
  files: Entry[];
  buffers: Record<string, unknown>;
  readonly terminal_panel: Terminal | null;
  newFile(args?: {
    name?: string;
    folder?: string;
    text?: string;
  }): Promise<tilo.Run | void>;
  /** Runs `new-file` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  newFileMany(items: NewFileArgs[]): Promise<tilo.ActionAnswer[]>;
  newFolder(args?: {
    name?: string;
    folder?: string;
  }): Promise<tilo.Run | void>;
  /** Runs `new-folder` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  newFolderMany(items: NewFolderArgs[]): Promise<tilo.ActionAnswer[]>;
  rename(args: {
    entry: string;
    name: string;
  }): Promise<tilo.Run | void>;
  /** Runs `rename` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  renameMany(items: RenameArgs[]): Promise<tilo.ActionAnswer[]>;
  moveEntry(args: {
    entry: string;
    folder?: string;
    before?: string;
  }): Promise<tilo.Run | void>;
  /** Runs `move-entry` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  moveEntryMany(items: MoveEntryArgs[]): Promise<tilo.ActionAnswer[]>;
  replaceText(args: {
    file: string;
    text: string;
  }): Promise<tilo.Run | void>;
  /** Runs `replace-text` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  replaceTextMany(items: ReplaceTextArgs[]): Promise<tilo.ActionAnswer[]>;
  replaceRange(args: {
    at: unknown;
    text: string;
  }): Promise<tilo.Run | void>;
  /** Runs `replace-range` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  replaceRangeMany(items: ReplaceRangeArgs[]): Promise<tilo.ActionAnswer[]>;
  delete(args: {
    entry: string;
  }): Promise<tilo.Run | void>;
  /** Runs `delete` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  deleteMany(items: DeleteArgs[]): Promise<tilo.ActionAnswer[]>;
  recordEntry(args: {
    id: string;
    kind: EntryKind;
    name: string;
    language: string;
    folder?: string;
  }): Promise<tilo.Run | void>;
  /** Runs `record-entry` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  recordEntryMany(items: RecordEntryArgs[]): Promise<tilo.ActionAnswer[]>;
  recordText(args: {
    file: string;
    text: string;
  }): Promise<tilo.Run | void>;
  /** Runs `record-text` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  recordTextMany(items: RecordTextArgs[]): Promise<tilo.ActionAnswer[]>;
  recordTarget(args: {
    target: string;
    device?: string;
  }): Promise<tilo.Run | void>;
  /** Runs `record-target` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  recordTargetMany(items: RecordTargetArgs[]): Promise<tilo.ActionAnswer[]>;
  recordRelaySession(args?: {
    session?: string;
  }): Promise<tilo.Run | void>;
  /** Runs `record-relay-session` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  recordRelaySessionMany(items: RecordRelaySessionArgs[]): Promise<tilo.ActionAnswer[]>;
  claimPane(args: {
    pane: string;
    device?: string;
  }): Promise<tilo.Run | void>;
  /** Runs `claim-pane` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  claimPaneMany(items: ClaimPaneArgs[]): Promise<tilo.ActionAnswer[]>;
  run(args: {
    command: string;
  }): Promise<unknown | tilo.Run>;
  rerun(args: {
    run: string;
  }): Promise<unknown | tilo.Run>;
  rerunTest(args: {
    run: string;
    test: string;
  }): Promise<unknown | tilo.Run>;
  stop(): Promise<tilo.Run | void>;
  /** Runs `stop` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  stopMany(items: StopArgs[]): Promise<tilo.ActionAnswer[]>;
  recordRun(args: {
    command: string;
    cwd: string;
    started_ms: number;
    ended_ms: number;
    exit: number;
    truncated: boolean;
    tool: string;
    undoable: boolean;
    items: unknown[];
    run: string;
    output?: unknown;
    request?: string;
  }): Promise<tilo.Run | void>;
  /** Runs `record-run` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  recordRunMany(items: RecordRunArgs[]): Promise<tilo.ActionAnswer[]>;
  reveal(args: {
    line: number;
    column: number;
    results: boolean;
    file?: string;
    path?: unknown;
  }): Promise<void>;
  find(): Promise<void>;
  searchProject(): Promise<void>;
  goToLine(args?: {
    value?: string;
  }): Promise<void>;
  toggleBottomPanel(): Promise<void>;
  nextTheme(): Promise<void>;
  goToProblems(): Promise<void>;
  goToSearch(): Promise<void>;
  goToTerminal(): Promise<void>;
  toggleSidebar(): Promise<void>;
  toggleLanguageServers(): Promise<void>;
  toggleFormatOnSave(): Promise<void>;
}

/** The `code_editor.project` kind at `path`. */
export function project(ws: tilo.Workspace, path: string): Project {
  return resourceProxy(ws, path, SURFACE) as unknown as Project;
}

