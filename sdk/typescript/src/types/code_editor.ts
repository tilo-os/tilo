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
 * longer answers an `Invocation`. A record is the object the wire carries, and an enum whose
 * cases carry fields is an object whose tag the app's schema names. An action whose
 * name is one the interface already has is run with `action("…")`, which the file
 * says where it is. An undoable action that answers nothing also has a `<action>Many`,
which applies it to every argument object of a list in a few all-or-nothing steps,
all of them one invocation group.
 */

import { resourceProxy } from "tilo-sdk";
import type * as tilo from "tilo-sdk";

/** The version of the surface these types were made from: a workspace whose app
 * publishes another refuses their actions rather than run what they no longer
 * describe. */
export const SURFACE = "so.tilo.code-editor@7a02f1a0144d5191";

export interface Command {
  command: string;
  cwd: string;
  environment: string | null;
  actor: string | null;
  for_person: string | null;
  started_ms: number;
  ended_ms: number;
  exit: number;
  output: unknown;
  truncated: boolean;
  tool: string;
  undoable: boolean;
  items: CommandItem[];
}

export interface CommandItem {
  kind: ItemKind;
  text: string;
  path: string | null;
  line: number | null;
  column: number | null;
  code: string | null;
}

export interface CommandRequest {
  command: string;
  requested_by: string | null;
  for_person: string | null;
  pane: string;
  agent: boolean;
}

export interface Entry {
  kind: EntryKind;
  name: string;
  parent: string | null;
  language: string;
}

/** One of "file", "folder". */
export type EntryKind = "file" | "folder";

export interface FinishedCommand {
  command: string;
  cwd: string;
  started_ms: number;
  ended_ms: number;
  exit: number;
  output: unknown | null;
  truncated: boolean;
  tool: string;
  undoable: boolean;
  items: CommandItem[];
  request: string | null;
  id: string;
}

/** One of "error", "warning", "location", "url", "test-passed", "test-failed", "test-ignored", "change". */
export type ItemKind = "error" | "warning" | "location" | "url" | "test-passed" | "test-failed" | "test-ignored" | "change";

export interface Terminal {
  target: string | null;
  relay_session: string | null;
  pane: string | null;
  device: string | null;
  holder: string | null;
  commands: Array<tilo.Entry<Command>>;
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

/** The arguments of `record-command`. */
export interface RecordCommandArgs {
  command: string;
  cwd: string;
  started_ms: number;
  ended_ms: number;
  exit: number;
  truncated: boolean;
  tool: string;
  undoable: boolean;
  items: CommandItem[];
  id: string;
  output?: unknown;
  request?: string;
}

/** Code Editor — `code_editor.project`. */
export interface Project extends tilo.Resource {
  files: Array<tilo.Entry<Entry>>;
  buffers: Record<string, unknown>;
  readonly terminal_panel: Terminal | null;
  newFile(args?: {
    name?: string;
    folder?: string;
    text?: string;
  }): Promise<void>;
  /** Runs `new-file` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  newFileMany(items: NewFileArgs[]): Promise<tilo.ActionAnswer[]>;
  newFolder(args?: {
    name?: string;
    folder?: string;
  }): Promise<void>;
  /** Runs `new-folder` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  newFolderMany(items: NewFolderArgs[]): Promise<tilo.ActionAnswer[]>;
  rename(args: {
    entry: string;
    name: string;
  }): Promise<void>;
  /** Runs `rename` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  renameMany(items: RenameArgs[]): Promise<tilo.ActionAnswer[]>;
  moveEntry(args: {
    entry: string;
    folder?: string;
    before?: string;
  }): Promise<void>;
  /** Runs `move-entry` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  moveEntryMany(items: MoveEntryArgs[]): Promise<tilo.ActionAnswer[]>;
  replaceText(args: {
    file: string;
    text: string;
  }): Promise<void>;
  /** Runs `replace-text` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  replaceTextMany(items: ReplaceTextArgs[]): Promise<tilo.ActionAnswer[]>;
  replaceRange(args: {
    at: unknown;
    text: string;
  }): Promise<void>;
  /** Runs `replace-range` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  replaceRangeMany(items: ReplaceRangeArgs[]): Promise<tilo.ActionAnswer[]>;
  delete(args: {
    entry: string;
  }): Promise<void>;
  /** Runs `delete` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deleteMany(items: DeleteArgs[]): Promise<tilo.ActionAnswer[]>;
  recordEntry(args: {
    id: string;
    kind: EntryKind;
    name: string;
    language: string;
    folder?: string;
  }): Promise<void>;
  /** Runs `record-entry` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  recordEntryMany(items: RecordEntryArgs[]): Promise<tilo.ActionAnswer[]>;
  recordText(args: {
    file: string;
    text: string;
  }): Promise<void>;
  /** Runs `record-text` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  recordTextMany(items: RecordTextArgs[]): Promise<tilo.ActionAnswer[]>;
  recordTarget(args: {
    target: string;
    device?: string;
  }): Promise<void>;
  /** Runs `record-target` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  recordTargetMany(items: RecordTargetArgs[]): Promise<tilo.ActionAnswer[]>;
  recordRelaySession(args?: {
    session?: string;
  }): Promise<void>;
  /** Runs `record-relay-session` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  recordRelaySessionMany(items: RecordRelaySessionArgs[]): Promise<tilo.ActionAnswer[]>;
  claimPane(args: {
    pane: string;
    device?: string;
  }): Promise<void>;
  /** Runs `claim-pane` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  claimPaneMany(items: ClaimPaneArgs[]): Promise<tilo.ActionAnswer[]>;
  run(args: {
    command: string;
  }): Promise<unknown>;
  rerun(args: {
    run: string;
  }): Promise<unknown>;
  rerunTest(args: {
    run: string;
    test: string;
  }): Promise<unknown>;
  stop(): Promise<void>;
  /** Runs `stop` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  stopMany(items: StopArgs[]): Promise<tilo.ActionAnswer[]>;
  recordCommand(args: {
    command: string;
    cwd: string;
    started_ms: number;
    ended_ms: number;
    exit: number;
    truncated: boolean;
    tool: string;
    undoable: boolean;
    items: CommandItem[];
    id: string;
    output?: unknown;
    request?: string;
  }): Promise<void>;
  /** Runs `record-command` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  recordCommandMany(items: RecordCommandArgs[]): Promise<tilo.ActionAnswer[]>;
  showSymbol(args: {
    symbol: string;
  }): Promise<void>;
  showResult(args: {
    result: string;
  }): Promise<void>;
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

/** The actions `Project` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PROJECT_ACTIONS = ["newFile", "newFileMany", "newFolder", "newFolderMany", "rename", "renameMany", "moveEntry", "moveEntryMany", "replaceText", "replaceTextMany", "replaceRange", "replaceRangeMany", "delete", "deleteMany", "recordEntry", "recordEntryMany", "recordText", "recordTextMany", "recordTarget", "recordTargetMany", "recordRelaySession", "recordRelaySessionMany", "claimPane", "claimPaneMany", "run", "rerun", "rerunTest", "stop", "stopMany", "recordCommand", "recordCommandMany", "showSymbol", "showResult", "reveal", "find", "searchProject", "goToLine", "toggleBottomPanel", "nextTheme", "goToProblems", "goToSearch", "goToTerminal", "toggleSidebar", "toggleLanguageServers", "toggleFormatOnSave"] as const;

/** The `code_editor.project` kind at `path`. */
export function project(ws: tilo.Workspace, path: string): Project {
  return resourceProxy(ws, path, SURFACE, PROJECT_ACTIONS) as unknown as Project;
}

