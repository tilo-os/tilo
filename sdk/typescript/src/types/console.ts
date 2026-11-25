/**
 * Types for Console (`so.tilo.console`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.json`; write
 * it again rather than editing it.
 *
 * `Notice` is a `Resource` of the app's `tl.tilo.host.Notice`, made by its factory with the
 * resource's path (`console/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.console@c5730a1c98f095e9";

/** The arguments of `stop_agent`. */
export interface StopAgentArgs {
}

/** The arguments of `take_over_agent`. */
export interface TakeOverAgentArgs {
}

/** The arguments of `undo_agent_session`. */
export interface UndoAgentSessionArgs {
}

/** The arguments of `let_agent_go_on`. */
export interface LetAgentGoOnArgs {
}

/** The arguments of `publish`. */
export interface PublishArgs {
}

/** The arguments of `stop_client`. */
export interface StopClientArgs {
  client: string;
}

/** The arguments of `take_over_client`. */
export interface TakeOverClientArgs {
  client: string;
}

/** The arguments of `undo_client_session`. */
export interface UndoClientSessionArgs {
  client: string;
}

/** The arguments of `let_client_go_on`. */
export interface LetClientGoOnArgs {
  client: string;
}

/** The arguments of `open_connect`. */
export interface OpenConnectArgs {
}

/** Notice — `tl.tilo.host.Notice`. */
export interface Notice extends tilo.Resource {
  notice_to: string;  // to
  notice_title: string;  // title
  notice_body: string;  // body
  notice_opens: unknown;  // opens
  notice_seen: boolean;  // seen
  notice_device: string;  // device
  notice_actions: string[];  // actions
}

/** The actions `Notice` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const NOTICE_ACTIONS = [] as const;

/** The `tl.tilo.host.Notice` kind at `path`. */
export function notice(ws: tilo.Workspace, path: string): Notice {
  return resourceProxy(ws, path, SURFACE, NOTICE_ACTIONS) as unknown as Notice;
}

/** SessionLink — `tl.so.tilo.console.SessionLink`. */
export interface SessionLink extends tilo.Resource {
  acxv3wxd: string;  // run
  "2pulq7rs": string;  // instruction
}

/** The actions `SessionLink` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SESSIONLINK_ACTIONS = [] as const;

/** The `tl.so.tilo.console.SessionLink` kind at `path`. */
export function sessionLink(ws: tilo.Workspace, path: string): SessionLink {
  return resourceProxy(ws, path, SURFACE, SESSIONLINK_ACTIONS) as unknown as SessionLink;
}

/** AgentCard — `tl.so.tilo.console.AgentCard`. */
export interface AgentCard extends tilo.Resource {
  mpwn6fmy: string;  // client
  /** Stops the client's working session, keeping its grant. */
  stop_agent(): Promise<void>;
  /** Runs `stop_agent` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  stop_agentMany(items: StopAgentArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Stops the client's session and opens what it changed last, or the workspace it worked in. */
  take_over_agent(): Promise<void>;
  /** Runs `take_over_agent` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  take_over_agentMany(items: TakeOverAgentArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes back every change of the client's newest session, when the history says it can. */
  undo_agent_session(): Promise<void>;
  /** Runs `undo_agent_session` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  undo_agent_sessionMany(items: UndoAgentSessionArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes back the grant the client came in by. */
  revoke_agent(): Promise<void>;
  /** Lets a stopped client's calls through again. */
  let_agent_go_on(): Promise<void>;
  /** Runs `let_agent_go_on` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  let_agent_go_onMany(items: LetAgentGoOnArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `AgentCard` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const AGENTCARD_ACTIONS = ["stop_agent", "stop_agentMany", "take_over_agent", "take_over_agentMany", "undo_agent_session", "undo_agent_sessionMany", "revoke_agent", "let_agent_go_on", "let_agent_go_onMany"] as const;

/** The `tl.so.tilo.console.AgentCard` kind at `path`. */
export function agentCard(ws: tilo.Workspace, path: string): AgentCard {
  return resourceProxy(ws, path, SURFACE, AGENTCARD_ACTIONS) as unknown as AgentCard;
}

/** Window — `tl.so.tilo.console.Window`. */
export interface Window extends tilo.Resource {
  e6xn3nds: string;  // place
  ujzhryea: Array<tilo.Entry<SessionLink>>;  // sessions
  hadfiy76: Array<tilo.Entry<AgentCard>>;  // cards
  /** Gives each of the newest sessions the history lists, and each client that acted here, a resource of its own, so each can be linked to, found and opened outside this window. An insert under a key a resource already stands under leaves that resource as it stands, so a reading that answers again changes nothing twice. */
  publish(): Promise<void>;
  /** Runs `publish` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  publishMany(items: PublishArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The resource of the session `run`, made now when the window has not published it yet: every way of opening a session goes through it, so the list, a link, search and another app open the same window. */
  session_link(args: {
    run: string;
  }): Promise<SessionLink>;
  /** The card of `client`, made now when the window has not published it yet: every way of opening a client's card goes through it. */
  agent_card(args: {
    client: string;
  }): Promise<AgentCard>;
  /** Stops the client's working session, keeping its grant: the card's own Stop, by the client a card, a palette or an agent names. */
  stop_client(args: {
    client: string;
  }): Promise<void>;
  /** Runs `stop_client` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  stop_clientMany(items: StopClientArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Stops the client's session and opens what it changed last: the card's own Take over, by the client a card, a palette or an agent names. */
  take_over_client(args: {
    client: string;
  }): Promise<void>;
  /** Runs `take_over_client` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  take_over_clientMany(items: TakeOverClientArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes back every change of the client's newest session: the card's own Undo session, by the client a card, a palette or an agent names. */
  undo_client_session(args: {
    client: string;
  }): Promise<void>;
  /** Runs `undo_client_session` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  undo_client_sessionMany(items: UndoClientSessionArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes back the grant the client came in by: the card's own Revoke, by the client a card, a palette or an agent names. */
  revoke_client(args: {
    client: string;
  }): Promise<void>;
  /** Lets a stopped client's calls through again: the card's own Let it go on, by the client a card, a palette or an agent names. */
  let_client_go_on(args: {
    client: string;
  }): Promise<void>;
  /** Runs `let_client_go_on` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  let_client_go_onMany(items: LetClientGoOnArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shows how an agent or another computer connects as this person: the window opens at Connect. */
  open_connect(): Promise<void>;
  /** Runs `open_connect` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  open_connectMany(items: OpenConnectArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Window` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const WINDOW_ACTIONS = ["publish", "publishMany", "session_link", "agent_card", "stop_client", "stop_clientMany", "take_over_client", "take_over_clientMany", "undo_client_session", "undo_client_sessionMany", "revoke_client", "let_client_go_on", "let_client_go_onMany", "open_connect", "open_connectMany"] as const;

/** The `tl.so.tilo.console.Window` kind at `path`. */
export function consoleWindow(ws: tilo.Workspace, path: string): Window {
  return resourceProxy(ws, path, SURFACE, WINDOW_ACTIONS) as unknown as Window;
}

/** Console — `tl.so.tilo.console`. */
export interface Console extends tilo.Resource {
}

/** The actions `Console` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CONSOLE_ACTIONS = [] as const;

/** The `tl.so.tilo.console` kind at `path`. */
export function console(ws: tilo.Workspace, path: string): Console {
  return resourceProxy(ws, path, SURFACE, CONSOLE_ACTIONS) as unknown as Console;
}

