/**
 * Types for Assistant (`so.tilo.assistant`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Window` is a `Resource` of the app's `assistant.window`, made by its factory with the
 * resource's path (`assistant/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.assistant@5caa27ee491e8bfe";

export interface Agent {
  id: string;
  name: string;
  keeper: string;
  model: string | null;
  mcp_client: string | null;
  bound: Bound[];
  spend_limit_micros: number | null;
  spent: Spent | null;
  state: AgentState;
  created_at: number;
  instruction: string | null;
  sub: Sub | null;
  sub_depth: number | null;
  sub_at_once: number | null;
}

/** One of "active", "retired". */
export type AgentState = "active" | "retired";

export interface Bound {
  node: string;
  kinds: string[];
  answer: string;
}

export interface NewSubAgent {
  name: string;
  job: string;
  role: string | null;
  instruction: string | null;
  kind: string | null;
  model: string | null;
  task: string | null;
  id: string | null;
}

export interface Note {
  id: string;
  by: string;
  text: string;
  at: number;
}

export interface Produced {
  id: string;
  kind: string;
  label: string;
  url: string | null;
  at: ProducedAt | null;
  at_ms: number;
}

export interface ProducedAt {
  app: string;
  document: string;
  resource: string;
}

export interface Spent {
  day: string;
  cost_micros: number;
}

/** One of "working", "needs_person", "done", "failed", "stopped". */
export type Stands = "working" | "needs_person" | "done" | "failed" | "stopped";

export interface Sub {
  started_by: string;
  job: string;
  role: string;
  kind: string;
  depth: number;
  task: string | null;
  conversation: string;
  stands: Stands;
  since: number;
  line: string;
  notes: Note[];
  produced: Produced[];
}

/** The arguments of `create-agent`. */
export interface CreateAgentArgs {
  name: string;
  id?: string;
  model?: string;
  mcp_client?: string;
}

/** The arguments of `ensure-personal-assistant`. */
export interface EnsurePersonalAssistantArgs {
}

/** The arguments of `set-agent-spend-limit`. */
export interface SetAgentSpendLimitArgs {
  agent: string;
  limit_micros?: number;
}

/** The arguments of `record-agent-spend`. */
export interface RecordAgentSpendArgs {
  agent: string;
  day: string;
  cost_micros: number;
}

/** The arguments of `rename-agent`. */
export interface RenameAgentArgs {
  agent: string;
  name: string;
}

/** The arguments of `set-agent-model`. */
export interface SetAgentModelArgs {
  agent: string;
  model?: string;
}

/** The arguments of `set-agent-instruction`. */
export interface SetAgentInstructionArgs {
  agent: string;
  instruction?: string;
}

/** The arguments of `set-agent-bound`. */
export interface SetAgentBoundArgs {
  agent: string;
  bound: Bound[];
}

/** The arguments of `retire-agent`. */
export interface RetireAgentArgs {
  agent: string;
}

/** The arguments of `start-sub-agent`. */
export interface StartSubAgentArgs {
  name: string;
  job: string;
  role?: string;
  instruction?: string;
  kind?: string;
  model?: string;
  task?: string;
  id?: string;
}

/** The arguments of `note-sub-agent`. */
export interface NoteSubAgentArgs {
  agent: string;
  text: string;
}

/** The arguments of `stop-sub-agent`. */
export interface StopSubAgentArgs {
  agent: string;
}

/** The arguments of `sub-agent-stands`. */
export interface SubAgentStandsArgs {
  agent: string;
  stands: Stands;
  line?: string;
}

/** The arguments of `sub-agent-produced`. */
export interface SubAgentProducedArgs {
  agent: string;
  kind: string;
  label: string;
  url?: string;
  at?: ProducedAt;
}

/** The arguments of `set-sub-agent-limits`. */
export interface SetSubAgentLimitsArgs {
  agent: string;
  depth?: number;
  at_once?: number;
}

/** Assistant window — `assistant.window`. */
export interface Window extends tilo.Resource {
  viewMainChat(): Promise<void>;
}

/** The actions `Window` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const WINDOW_ACTIONS = ["viewMainChat"] as const;

/** The `assistant.window` kind at `path`. */
export function assistantWindow(ws: tilo.Workspace, path: string): Window {
  return resourceProxy(ws, path, SURFACE, WINDOW_ACTIONS) as unknown as Window;
}

/** Agents — `tilo.workspace.agent`. */
export interface WorkspaceAgent extends tilo.Resource {
  agents: Array<tilo.Entry<Agent>>;
  createAgent(args: {
    name: string;
    id?: string;
    model?: string;
    mcp_client?: string;
  }): Promise<void>;
  /** Runs `create-agent` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  createAgentMany(items: CreateAgentArgs[]): Promise<tilo.ActionAnswer[]>;
  ensurePersonalAssistant(): Promise<void>;
  /** Runs `ensure-personal-assistant` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  ensurePersonalAssistantMany(items: EnsurePersonalAssistantArgs[]): Promise<tilo.ActionAnswer[]>;
  setAgentSpendLimit(args: {
    agent: string;
    limit_micros?: number;
  }): Promise<void>;
  /** Runs `set-agent-spend-limit` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setAgentSpendLimitMany(items: SetAgentSpendLimitArgs[]): Promise<tilo.ActionAnswer[]>;
  recordAgentSpend(args: {
    agent: string;
    day: string;
    cost_micros: number;
  }): Promise<void>;
  /** Runs `record-agent-spend` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  recordAgentSpendMany(items: RecordAgentSpendArgs[]): Promise<tilo.ActionAnswer[]>;
  renameAgent(args: {
    agent: string;
    name: string;
  }): Promise<void>;
  /** Runs `rename-agent` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  renameAgentMany(items: RenameAgentArgs[]): Promise<tilo.ActionAnswer[]>;
  setAgentModel(args: {
    agent: string;
    model?: string;
  }): Promise<void>;
  /** Runs `set-agent-model` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setAgentModelMany(items: SetAgentModelArgs[]): Promise<tilo.ActionAnswer[]>;
  setAgentInstruction(args: {
    agent: string;
    instruction?: string;
  }): Promise<void>;
  /** Runs `set-agent-instruction` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setAgentInstructionMany(items: SetAgentInstructionArgs[]): Promise<tilo.ActionAnswer[]>;
  setAgentBound(args: {
    agent: string;
    bound: Bound[];
  }): Promise<void>;
  /** Runs `set-agent-bound` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setAgentBoundMany(items: SetAgentBoundArgs[]): Promise<tilo.ActionAnswer[]>;
  retireAgent(args: {
    agent: string;
  }): Promise<void>;
  /** Runs `retire-agent` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  retireAgentMany(items: RetireAgentArgs[]): Promise<tilo.ActionAnswer[]>;
  startSubAgent(args: {
    name: string;
    job: string;
    role?: string;
    instruction?: string;
    kind?: string;
    model?: string;
    task?: string;
    id?: string;
  }): Promise<void>;
  /** Runs `start-sub-agent` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  startSubAgentMany(items: StartSubAgentArgs[]): Promise<tilo.ActionAnswer[]>;
  noteSubAgent(args: {
    agent: string;
    text: string;
  }): Promise<void>;
  /** Runs `note-sub-agent` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  noteSubAgentMany(items: NoteSubAgentArgs[]): Promise<tilo.ActionAnswer[]>;
  stopSubAgent(args: {
    agent: string;
  }): Promise<void>;
  /** Runs `stop-sub-agent` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  stopSubAgentMany(items: StopSubAgentArgs[]): Promise<tilo.ActionAnswer[]>;
  subAgentStands(args: {
    agent: string;
    stands: Stands;
    line?: string;
  }): Promise<void>;
  /** Runs `sub-agent-stands` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  subAgentStandsMany(items: SubAgentStandsArgs[]): Promise<tilo.ActionAnswer[]>;
  subAgentProduced(args: {
    agent: string;
    kind: string;
    label: string;
    url?: string;
    at?: ProducedAt;
  }): Promise<void>;
  /** Runs `sub-agent-produced` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  subAgentProducedMany(items: SubAgentProducedArgs[]): Promise<tilo.ActionAnswer[]>;
  setSubAgentLimits(args: {
    agent: string;
    depth?: number;
    at_once?: number;
  }): Promise<void>;
  /** Runs `set-sub-agent-limits` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setSubAgentLimitsMany(items: SetSubAgentLimitsArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `WorkspaceAgent` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const WORKSPACEAGENT_ACTIONS = ["createAgent", "createAgentMany", "ensurePersonalAssistant", "ensurePersonalAssistantMany", "setAgentSpendLimit", "setAgentSpendLimitMany", "recordAgentSpend", "recordAgentSpendMany", "renameAgent", "renameAgentMany", "setAgentModel", "setAgentModelMany", "setAgentInstruction", "setAgentInstructionMany", "setAgentBound", "setAgentBoundMany", "retireAgent", "retireAgentMany", "startSubAgent", "startSubAgentMany", "noteSubAgent", "noteSubAgentMany", "stopSubAgent", "stopSubAgentMany", "subAgentStands", "subAgentStandsMany", "subAgentProduced", "subAgentProducedMany", "setSubAgentLimits", "setSubAgentLimitsMany"] as const;

/** The `tilo.workspace.agent` kind at `path`. */
export function workspaceAgent(ws: tilo.Workspace, path: string): WorkspaceAgent {
  return resourceProxy(ws, path, SURFACE, WORKSPACEAGENT_ACTIONS) as unknown as WorkspaceAgent;
}

