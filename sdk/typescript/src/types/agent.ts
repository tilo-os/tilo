/**
 * Types for Agents (`so.tilo.agent`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Ask` is a `Resource` of the app's `tl.so.tilo.agent.Ask`, made by its factory with the
 * resource's path (`agent/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.agent@4925ad3921e038bc";

/** Where an ask stands. The workspace's agent engine moves it on, except that its asker withdraws it and a person stops a turn that is working. One of "waiting", "working", "done", "failed", "stopped", "withdrawn", "cannot_read". */
export type AskState = "waiting" | "working" | "done" | "failed" | "stopped" | "withdrawn" | "cannot_read";

/** The arguments of `withdraw`. */
export interface WithdrawArgs {
}

/** The arguments of `reopen`. */
export interface ReopenArgs {
}

/** The arguments of `retry`. */
export interface RetryArgs {
}

/** The arguments of `stop`. */
export interface StopArgs {
}

/** The arguments of `say`. */
export interface SayArgs {
  more: string;
}

/** The arguments of `think`. */
export interface ThinkArgs {
  more: string;
}

/** The arguments of `step`. */
export interface StepArgs {
  called: string;
  answered: string;
  failed?: boolean;
}

/** The arguments of `show_view`. */
export interface ShowViewArgs {
  title: string;
  definition: string;
}

/** The arguments of `show`. */
export interface ShowArgs {
  card: string;
}

/** The arguments of `finish`. */
export interface FinishArgs {
  state: AskState;
  provider?: string;
  model?: string;
  cost?: number;
}

/** The arguments of `supersede`. */
export interface SupersedeArgs {
}

/** Ask — `tl.so.tilo.agent.Ask`. */
export interface Ask extends tilo.Resource {
  mcsi6vp5: string;  // agent
  "7j5pok54": string | null;  // said
  hw6dxbyx: string;  // mention
  wtm364ne: AskState;  // state
  /** Takes an ask back before a device took its turn. Only its asker may. */
  withdraw(): Promise<void>;
  /** Runs `withdraw` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  withdrawMany(items: WithdrawArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Asks again what was withdrawn: the mention that made it is back, as redo puts it back. Only its asker may. */
  reopen(): Promise<void>;
  /** Runs `reopen` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  reopenMany(items: ReopenArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Asks again for an answer that failed: the same ask, its next attempt, so the reply written now stands beside the one that failed. Only its asker may. */
  retry(): Promise<void>;
  /** Runs `retry` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  retryMany(items: RetryArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Stops a turn that is working; what the agent did until then stays. */
  stop(): Promise<void>;
  /** Runs `stop` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  stopMany(items: StopArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The agent starts its reply to `a`, the ask's next attempt: the one after its newest reply, so a reply that failed and was asked again stands beside it. */
  answer(): Promise<Reply>;
}

/** The actions `Ask` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ASK_ACTIONS = ["withdraw", "withdrawMany", "reopen", "reopenMany", "retry", "retryMany", "stop", "stopMany", "answer"] as const;

/** The `tl.so.tilo.agent.Ask` kind at `path`. */
export function ask(ws: tilo.Workspace, path: string): Ask {
  return resourceProxy(ws, path, SURFACE, ASK_ACTIONS) as unknown as Ask;
}

/** Step — `tl.so.tilo.agent.Step`. */
export interface Step extends tilo.Resource {
  ippal7k2: string;  // called
  cyu3le2j: string;  // answered
  cma7vgpx: boolean;  // failed
}

/** The actions `Step` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const STEP_ACTIONS = [] as const;

/** The `tl.so.tilo.agent.Step` kind at `path`. */
export function step(ws: tilo.Workspace, path: string): Step {
  return resourceProxy(ws, path, SURFACE, STEP_ACTIONS) as unknown as Step;
}

/** SentView — `tl.so.tilo.agent.SentView`. */
export interface SentView extends tilo.Resource {
  gdhl4pzx: string;  // title
  ytw765gn: string;  // definition
}

/** The actions `SentView` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SENTVIEW_ACTIONS = [] as const;

/** The `tl.so.tilo.agent.SentView` kind at `path`. */
export function sentView(ws: tilo.Workspace, path: string): SentView {
  return resourceProxy(ws, path, SURFACE, SENTVIEW_ACTIONS) as unknown as SentView;
}

/** Shown — `tl.so.tilo.agent.Shown`. */
export interface Shown extends tilo.Resource {
  mx5wcfl2: string;  // card
}

/** The actions `Shown` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SHOWN_ACTIONS = [] as const;

/** The `tl.so.tilo.agent.Shown` kind at `path`. */
export function shown(ws: tilo.Workspace, path: string): Shown {
  return resourceProxy(ws, path, SURFACE, SHOWN_ACTIONS) as unknown as Shown;
}

/** Reply — `tl.so.tilo.agent.Reply`. */
export interface Reply extends tilo.Resource {
  gjd462ep: Ask;  // ask
  oka345cb: string;  // body
  czzg7mhh: string;  // thinking
  hzlmnzzw: AskState;  // state
  tnkzsmes: number;  // attempt
  e36wbuu7: Array<tilo.Entry<Step>>;  // steps
  bnlsdtf6: Array<tilo.Entry<SentView>>;  // views
  mwwrjl52: Array<tilo.Entry<Shown>>;  // shown
  vbzsd5lu: string;  // provider
  "5xeeayip": string;  // model
  "4xymfj5v": number;  // cost
  eyh6zmbf: boolean;  // superseded
  /** The agent writes `more` at the end of its answer. */
  say(args: {
    more: string;
  }): Promise<void>;
  /** Runs `say` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  sayMany(items: SayArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The agent's model thought `more` on the way, as its provider shows it: at the end of the reply's thinking, beside its answer. */
  think(args: {
    more: string;
  }): Promise<void>;
  /** Runs `think` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  thinkMany(items: ThinkArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The agent called `called` on the way, and it answered `answered`. */
  step(args: {
    called: string;
    answered: string;
    failed?: boolean;
  }): Promise<void>;
  /** Runs `step` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  stepMany(items: StepArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The agent answers with a view: `definition`, called `title`. */
  show_view(args: {
    title: string;
    definition: string;
  }): Promise<void>;
  /** Runs `show_view` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  show_viewMany(items: ShowViewArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The agent shows the thing `card` projects in its answer, live. */
  show(args: {
    card: string;
  }): Promise<void>;
  /** Runs `show` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  showMany(items: ShowArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The agent's turn ended as `state`: done, failed, or stopped when a person stopped it. `model` of `provider` answered it, where a model did, its calls costing `cost` millionths of the workspace's currency; a turn that ended before its model answered says nothing of one. */
  finish(args: {
    state: AskState;
    provider?: string;
    model?: string;
    cost?: number;
  }): Promise<void>;
  /** Runs `finish` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  finishMany(items: FinishArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Another device's answer to the same ask stands instead of `r`. */
  supersede(): Promise<void>;
  /** Runs `supersede` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  supersedeMany(items: SupersedeArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Reply` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const REPLY_ACTIONS = ["say", "sayMany", "think", "thinkMany", "step", "stepMany", "show_view", "show_viewMany", "show", "showMany", "finish", "finishMany", "supersede", "supersedeMany"] as const;

/** The `tl.so.tilo.agent.Reply` kind at `path`. */
export function reply(ws: tilo.Workspace, path: string): Reply {
  return resourceProxy(ws, path, SURFACE, REPLY_ACTIONS) as unknown as Reply;
}

/** Summary — `tl.so.tilo.agent.Summary`. */
export interface Summary extends tilo.Resource {
  fbk2rxka: string;  // agent
  wl4g23ke: string;  // text
  hefqltbc: string;  // through
  cs27n4h5: string;  // through_at
}

/** The actions `Summary` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SUMMARY_ACTIONS = [] as const;

/** The `tl.so.tilo.agent.Summary` kind at `path`. */
export function summary(ws: tilo.Workspace, path: string): Summary {
  return resourceProxy(ws, path, SURFACE, SUMMARY_ACTIONS) as unknown as Summary;
}

/** Agents — `tl.so.tilo.agent.Agents`. */
export interface Agents extends tilo.Resource {
  mzzoxssv: Array<tilo.Entry<Ask>>;  // asks
  "353i7jsi": Array<tilo.Entry<Reply>>;  // replies
  srbtof5j: Array<tilo.Entry<Summary>>;  // summaries
  /** Asks `agent` about the resource the ask is made from (`.made_from(r)`). Whoever makes it decides first whether the agent may read that resource, and asks with nothing said when it may not: the ask then says so, and holds nothing of the resource. An ask made from a resource is named by it and the mention: every device that asks it so makes the same ask, and asking again is that ask. */
  ask(args: {
    agent: string;
    said: string;
    mention?: string;
  }): Promise<Ask>;
}

/** The actions `Agents` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const AGENTS_ACTIONS = ["ask"] as const;

/** The `tl.so.tilo.agent.Agents` kind at `path`. */
export function agents(ws: tilo.Workspace, path: string): Agents {
  return resourceProxy(ws, path, SURFACE, AGENTS_ACTIONS) as unknown as Agents;
}

/** Agents — `tl.so.tilo.agent`. */
export interface Agent extends tilo.Resource {
  /** The agent keeps what the conversation this is made from said before the part its turn was shown: `text` says it in the agent's words, reaching `through`, the item it last took in. One per conversation and agent: what it kept before is what it says now. */
  keep_summary(args: {
    agent: string;
    text: string;
    through: string;
    through_at?: string;
  }): Promise<Summary>;
}

/** The actions `Agent` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const AGENT_ACTIONS = ["keep_summary"] as const;

/** The `tl.so.tilo.agent` kind at `path`. */
export function agent(ws: tilo.Workspace, path: string): Agent {
  return resourceProxy(ws, path, SURFACE, AGENT_ACTIONS) as unknown as Agent;
}

