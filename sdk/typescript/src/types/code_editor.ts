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
 * says where it is.
 */

import { resourceProxy } from "tilo-sdk";
import type * as tilo from "tilo-sdk";

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
  runs: Run[];
  requests: unknown;
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
  newFolder(args?: {
    name?: string;
    folder?: string;
  }): Promise<tilo.Run | void>;
  rename(args: {
    entry: string;
    name: string;
  }): Promise<tilo.Run | void>;
  moveEntry(args: {
    entry: string;
    folder?: string;
    before?: string;
  }): Promise<tilo.Run | void>;
  replaceText(args: {
    file: string;
    text: string;
  }): Promise<tilo.Run | void>;
  replaceRange(args: {
    at: unknown;
    text: string;
  }): Promise<tilo.Run | void>;
  delete(args: {
    entry: string;
  }): Promise<tilo.Run | void>;
  recordEntry(args: {
    id: string;
    kind: EntryKind;
    name: string;
    language: string;
    folder?: string;
  }): Promise<tilo.Run | void>;
  recordText(args: {
    file: string;
    text: string;
  }): Promise<tilo.Run | void>;
  recordTarget(args: {
    target: string;
    device?: string;
  }): Promise<tilo.Run | void>;
  recordRelaySession(args?: {
    session?: string;
  }): Promise<tilo.Run | void>;
  claimPane(args: {
    pane: string;
    device?: string;
  }): Promise<tilo.Run | void>;
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
  recordRun(args: {
    command: string;
    cwd: string;
    started_ms: number;
    ended_ms: number;
    exit: number;
    output_len: number;
    truncated: boolean;
    tool: string;
    undoable: boolean;
    items: unknown[];
    run: string;
    output?: string;
    request?: string;
  }): Promise<tilo.Run | void>;
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
  return resourceProxy(ws, path) as unknown as Project;
}

