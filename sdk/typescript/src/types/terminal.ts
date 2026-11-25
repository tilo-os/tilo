/**
 * Types for Terminal (`so.tilo.terminal`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.json`; write
 * it again rather than editing it.
 *
 * `Session` is a `Resource` of the app's `terminal.session`, made by its factory with the
 * resource's path (`terminal/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.terminal@465901023d890f67";

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

export interface Ran {
  command: string;
  cwd: string;
  environment: string | null;
  exit: number;
  ended_ms: number;
  actor: unknown | null;
  session: string;
  run: string;
}

export interface Remembered {
  command: string;
  cwd: string;
  environment: string | null;
  exit: number;
  ended_ms: number;
  actor: string | null;
  session: string;
  run: string;
}

export interface MachineSecret {
  variable: string;
  item: string;
}

/** One of "Read", "Write". */
export type Access = "Read" | "Write";

export interface CapOverride {
  grantee: string;
  app: string | null;
  scope: Scope;
  effect: Effect;
  access: Access;
  id: string;
  lifetime: string;
  granted_by: string | null;
}

/** One of "camera", "microphone", "screen", "input", "system-audio", "printer", "host-shell", "system-info", "processes", "process-commands", "io-uring", "gpu", "clipboard", "desktop-control". */
export type DeviceCap = "camera" | "microphone" | "screen" | "input" | "system-audio" | "printer" | "host-shell" | "system-info" | "processes" | "process-commands" | "io-uring" | "gpu" | "clipboard" | "desktop-control";

/** One of "Grant", "Restrict", "WorkspaceFileAccess", "WorkspaceFileBoundary". */
export type Effect = "Grant" | "Restrict" | "WorkspaceFileAccess" | "WorkspaceFileBoundary";

export interface FileTask {
  device: string;
  action: unknown;
}

/** One of "Persistent", "Ephemeral". */
export type HomePolicy = "Persistent" | "Ephemeral";

export interface RuntimeRef {
  release: string;
  images: Record<string, string>;
}

/** One of FsPath, Device, NetHost, FsResource, NetCalls: an object whose case the app tags it with. */
export type Scope = Record<string, unknown>;

export interface ToolRef {
  packages: string[];
  images: Record<string, string>;
}

export interface WriteFiles {
  device: string;
  paths: string[];
  no_clobber: boolean | null;
  text: string;
  base64: boolean | null;
  content: string | null;
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

/** The arguments of `remember`. */
export interface RememberArgs {
  command: string;
  cwd: string;
  exit: number;
  ended_ms: number;
  session: string;
  run: string;
  environment?: string;
  actor?: unknown;
}

/** The arguments of `import-run`. */
export interface ImportRunArgs {
  command: string;
  cwd: string;
  exit: number;
  ended_ms: number;
  session: string;
  run: string;
  environment?: string;
  actor?: unknown;
}

/** The arguments of `add-secret`. */
export interface AddSecretArgs {
  variable: string;
  item: unknown;
}

/** The arguments of `remove-secret`. */
export interface RemoveSecretArgs {
  variable: string;
}

/** The arguments of `add-tool`. */
export interface AddToolArgs {
  packages: string[];
  arch: string;
  image: string;
}

/** The arguments of `remove-tool`. */
export interface RemoveToolArgs {
  packages: string[];
}

/** The arguments of `grant-dir`. */
export interface GrantDirArgs {
  path: string;
  write: boolean;
}

/** The arguments of `set-home`. */
export interface SetHomeArgs {
  path: string;
}

/** The arguments of `allow-host`. */
export interface AllowHostArgs {
  host: string;
}

/** The arguments of `revoke-grant`. */
export interface RevokeGrantArgs {
  id: string;
}

/** The arguments of `set-runtime`. */
export interface SetRuntimeArgs {
  runtime: RuntimeRef;
  tools: ToolRef[];
}

/** The arguments of `make`. */
export interface MakeArgs {
  name: string;
  runtime: RuntimeRef;
  workspace: string;
}

/** The arguments of `rename`. */
export interface RenameArgs {
  name: string;
}

/** Terminal — `terminal.session`. */
export interface Session extends tilo.Resource {
  readonly target: string | null;
  readonly relay_session: string | null;
  readonly pane: string | null;
  readonly device: string | null;
  readonly holder: string | null;
  readonly commands: Array<tilo.Entry<Command>>;
  run(args: {
    command: string;
  }): Promise<unknown>;
  rerun(args: {
    command: string;
  }): Promise<unknown>;
  rerunTest(args: {
    command: string;
    test: string;
  }): Promise<unknown>;
  stop(): Promise<void>;
  /** Runs `stop` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  stopMany(items: StopArgs[]): Promise<tilo.ActionAnswer[]>;
  openTerminal(args?: {
    cols?: number;
    rows?: number;
    attach?: string;
  }): Promise<unknown>;
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
  inspectSession(): Promise<void>;
  readScreen(): Promise<void>;
  inspectEditor(): Promise<void>;
  copyVisible(): Promise<void>;
  copySelection(): Promise<void>;
  selectAll(): Promise<void>;
  historyTop(): Promise<void>;
  historyLive(): Promise<void>;
  openLink(args: {
    url: string;
  }): Promise<void>;
}

/** The actions `Session` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SESSION_ACTIONS = ["run", "rerun", "rerunTest", "stop", "stopMany", "openTerminal", "recordCommand", "recordCommandMany", "recordTarget", "recordTargetMany", "recordRelaySession", "recordRelaySessionMany", "claimPane", "claimPaneMany", "inspectSession", "readScreen", "inspectEditor", "copyVisible", "copySelection", "selectAll", "historyTop", "historyLive", "openLink"] as const;

/** The `terminal.session` kind at `path`. */
export function session(ws: tilo.Workspace, path: string): Session {
  return resourceProxy(ws, path, SURFACE, SESSION_ACTIONS) as unknown as Session;
}

/** Command history — `terminal.history`. */
export interface History extends tilo.Resource {
  readonly remembered: Array<tilo.Entry<Remembered>>;
  remember(args: {
    command: string;
    cwd: string;
    exit: number;
    ended_ms: number;
    session: string;
    run: string;
    environment?: string;
    actor?: unknown;
  }): Promise<void>;
  /** Runs `remember` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  rememberMany(items: RememberArgs[]): Promise<tilo.ActionAnswer[]>;
  importRun(args: {
    command: string;
    cwd: string;
    exit: number;
    ended_ms: number;
    session: string;
    run: string;
    environment?: string;
    actor?: unknown;
  }): Promise<void>;
  /** Runs `import-run` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  importRunMany(items: ImportRunArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `History` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const HISTORY_ACTIONS = ["remember", "rememberMany", "importRun", "importRunMany"] as const;

/** The `terminal.history` kind at `path`. */
export function terminalHistory(ws: tilo.Workspace, path: string): History {
  return resourceProxy(ws, path, SURFACE, HISTORY_ACTIONS) as unknown as History;
}

/** Machine secrets — `terminal.machine-secrets`. */
export interface MachineSecrets extends tilo.Resource {
  secrets: MachineSecret[];
  addSecret(args: {
    variable: string;
    item: unknown;
  }): Promise<void>;
  /** Runs `add-secret` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  addSecretMany(items: AddSecretArgs[]): Promise<tilo.ActionAnswer[]>;
  removeSecret(args: {
    variable: string;
  }): Promise<void>;
  /** Runs `remove-secret` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  removeSecretMany(items: RemoveSecretArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `MachineSecrets` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const MACHINESECRETS_ACTIONS = ["addSecret", "addSecretMany", "removeSecret", "removeSecretMany"] as const;

/** The `terminal.machine-secrets` kind at `path`. */
export function machineSecrets(ws: tilo.Workspace, path: string): MachineSecrets {
  return resourceProxy(ws, path, SURFACE, MACHINESECRETS_ACTIONS) as unknown as MachineSecrets;
}

/** Environment — `environment`. */
export interface Environment extends tilo.Resource {
  name: string;
  runtime: RuntimeRef;
  tools: ToolRef[];
  grants: CapOverride[];
  home: HomePolicy;
  home_dir: string;
  // the app's `workspace` field is the SDK's own `workspace`.
  addTool(args: {
    packages: string[];
    arch: string;
    image: string;
  }): Promise<void>;
  /** Runs `add-tool` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  addToolMany(items: AddToolArgs[]): Promise<tilo.ActionAnswer[]>;
  removeTool(args: {
    packages: string[];
  }): Promise<void>;
  /** Runs `remove-tool` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  removeToolMany(items: RemoveToolArgs[]): Promise<tilo.ActionAnswer[]>;
  grantDir(args: {
    path: string;
    write: boolean;
  }): Promise<void>;
  /** Runs `grant-dir` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  grantDirMany(items: GrantDirArgs[]): Promise<tilo.ActionAnswer[]>;
  setHome(args: {
    path: string;
  }): Promise<void>;
  /** Runs `set-home` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setHomeMany(items: SetHomeArgs[]): Promise<tilo.ActionAnswer[]>;
  allowHost(args: {
    host: string;
  }): Promise<void>;
  /** Runs `allow-host` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  allowHostMany(items: AllowHostArgs[]): Promise<tilo.ActionAnswer[]>;
  revokeGrant(args: {
    id: string;
  }): Promise<void>;
  /** Runs `revoke-grant` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  revokeGrantMany(items: RevokeGrantArgs[]): Promise<tilo.ActionAnswer[]>;
  setRuntime(args: {
    runtime: RuntimeRef;
    tools: ToolRef[];
  }): Promise<void>;
  /** Runs `set-runtime` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setRuntimeMany(items: SetRuntimeArgs[]): Promise<tilo.ActionAnswer[]>;
  make(args: {
    name: string;
    runtime: RuntimeRef;
    workspace: string;
  }): Promise<void>;
  /** Runs `make` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  makeMany(items: MakeArgs[]): Promise<tilo.ActionAnswer[]>;
  rename(args: {
    name: string;
  }): Promise<void>;
  /** Runs `rename` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  renameMany(items: RenameArgs[]): Promise<tilo.ActionAnswer[]>;
  copy(args: {
    device: string;
    sources: string[];
    destination: string;
    recursive?: boolean;
    if_exists?: string;
  }): Promise<unknown>;
  move(args: {
    device: string;
    sources: string[];
    destination: string;
    no_clobber?: boolean;
  }): Promise<unknown>;
  // the app's `remove` action is the SDK's own: run it with `action("remove")`.
  makeDir(args: {
    device: string;
    dirs: string[];
    parents?: boolean;
  }): Promise<unknown>;
  removeDir(args: {
    device: string;
    dirs: string[];
    parents?: boolean;
  }): Promise<unknown>;
  link(args: {
    device: string;
    target: string;
    path: string;
    symbolic?: boolean;
    force?: boolean;
  }): Promise<unknown>;
  truncate(args: {
    device: string;
    paths: string[];
    size: number;
    create?: boolean;
  }): Promise<unknown>;
  touch(args: {
    device: string;
    paths: string[];
  }): Promise<unknown>;
  append(args: {
    device: string;
    paths: string[];
    text: string;
    content?: string;
  }): Promise<unknown>;
  write(args: {
    device: string;
    paths: string[];
    no_clobber?: boolean;
    text?: string;
    base64?: boolean;
    content?: string;
  }): Promise<unknown>;
  restore(args: {
    device: string;
    kept: string;
  }): Promise<unknown>;
  revert(args: {
    entry: string;
  }): Promise<unknown>;
}

/** The actions `Environment` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ENVIRONMENT_ACTIONS = ["addTool", "addToolMany", "removeTool", "removeToolMany", "grantDir", "grantDirMany", "setHome", "setHomeMany", "allowHost", "allowHostMany", "revokeGrant", "revokeGrantMany", "setRuntime", "setRuntimeMany", "make", "makeMany", "rename", "renameMany", "copy", "move", "makeDir", "removeDir", "link", "truncate", "touch", "append", "write", "restore", "revert"] as const;

/** The `environment` kind at `path`. */
export function environment(ws: tilo.Workspace, path: string): Environment {
  return resourceProxy(ws, path, SURFACE, ENVIRONMENT_ACTIONS) as unknown as Environment;
}

