/**
 * Types for Messages (`so.tilo.messages`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.json`; write
 * it again rather than editing it.
 *
 * `Messages` is a `Resource` of the app's `tilo.messages`, made by its factory with the
 * resource's path (`messages/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.messages@839f8a04619dec24";

export interface Conversation {
  with: string;
  messages: Array<tilo.Entry<Message>>;
}

export interface Message {
  from: string;
  to: string;
  body: string;
  date: string;
  at_ms: number;
  outgoing: boolean;
  queued: boolean;
}

/** Messages — `tilo.messages`. */
export interface Messages extends tilo.Resource {
  readonly conversations: Array<tilo.Entry<Conversation>>;
  deliverMessage(args: {
    from: string;
    body: string;
    to?: string;
    date?: string;
    conversation?: string;
  }): Promise<unknown>;
  sendMessage(args: {
    to: string;
    body: string;
    conversation?: string;
  }): Promise<unknown>;
  reply(args: {
    conversation: string;
    body: string;
  }): Promise<unknown>;
  listGroups(args: {
    service: string;
    account: string;
  }): Promise<void>;
  readGroupMembership(args: {
    service: string;
    account: string;
    group: string;
  }): Promise<void>;
  readGroupHistory(args: {
    service: string;
    account: string;
    group: string;
  }): Promise<void>;
  readHistory(args: {
    choice: string;
  }): Promise<void>;
  readOlderHistory(args: {
    choice: string;
  }): Promise<void>;
  searchMessages(args: {
    query: string;
  }): Promise<void>;
  exportContent(args?: {
    format?: string;
    conversation?: string;
  }): Promise<void>;
}

/** The actions `Messages` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const MESSAGES_ACTIONS = ["deliverMessage", "sendMessage", "reply", "listGroups", "readGroupMembership", "readGroupHistory", "readHistory", "readOlderHistory", "searchMessages", "exportContent"] as const;

/** The `tilo.messages` kind at `path`. */
export function messages(ws: tilo.Workspace, path: string): Messages {
  return resourceProxy(ws, path, SURFACE, MESSAGES_ACTIONS) as unknown as Messages;
}

