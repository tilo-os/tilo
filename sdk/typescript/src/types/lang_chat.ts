/**
 * Types for Chat (`so.tilo.lang.chat`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Conversations` is a `Resource` of the app's `tl.so.tilo.lang.chat.Conversations`, made by its factory with the
 * resource's path (`chat/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.lang.chat@73cc1693087f02f6";

/** The arguments of `delete`. */
export interface DeleteArgs {
  to: Direct | TextChannel | Thread;
  m: Message;
}

/** The arguments of `pin`. */
export interface PinArgs {
  pinned?: boolean;
}

/** The arguments of `react`. */
export interface ReactArgs {
  emoji: string;
}

/** The arguments of `delete`. */
export interface ThreadDeleteArgs {
  to: Direct | TextChannel | Thread;
  m: Message;
}

/** The arguments of `delete`. */
export interface DirectDeleteArgs {
  to: Direct | TextChannel | Thread;
  m: Message;
}

/** The arguments of `delete`. */
export interface ChatDeleteArgs {
  to: Direct | TextChannel | Thread;
  m: Message;
}

/** Conversations — `tl.so.tilo.lang.chat.Conversations`. */
export interface Conversations extends tilo.Resource {
  "5kzdptyg": Array<tilo.Entry<Category | Direct | TextChannel>>;  // channels
  /** Adds a text channel to the sidebar. */
  new_channel(args?: {
    name?: string;
  }): Promise<TextChannel>;
  /** Adds a category to the sidebar. */
  new_category(args?: {
    name?: string;
  }): Promise<Category>;
  /** Opens the direct message with `who`, in the sidebar, or the one already open with them: a person has one conversation, not a new empty one each time. */
  message_person(args: {
    who: string;
  }): Promise<Direct>;
}

/** The actions `Conversations` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CONVERSATIONS_ACTIONS = ["new_channel", "new_category", "message_person"] as const;

/** The `tl.so.tilo.lang.chat.Conversations` kind at `path`. */
export function conversations(ws: tilo.Workspace, path: string): Conversations {
  return resourceProxy(ws, path, SURFACE, CONVERSATIONS_ACTIONS) as unknown as Conversations;
}

/** Category — `tl.so.tilo.lang.chat.Category`. */
export interface Category extends tilo.Resource {
  hbhrds5c: string;  // name
  hvqgmwrk: Array<tilo.Entry<TextChannel>>;  // channels
  /** Adds a text channel to a category. */
  add_channel(args?: {
    name?: string;
  }): Promise<TextChannel>;
}

/** The actions `Category` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CATEGORY_ACTIONS = ["add_channel"] as const;

/** The `tl.so.tilo.lang.chat.Category` kind at `path`. */
export function category(ws: tilo.Workspace, path: string): Category {
  return resourceProxy(ws, path, SURFACE, CATEGORY_ACTIONS) as unknown as Category;
}

/** TextChannel — `tl.so.tilo.lang.chat.TextChannel`. */
export interface TextChannel extends tilo.Resource {
  x3haugpo: string;  // name
  u6b35mgm: string;  // topic
  ksuxpopi: Array<tilo.Entry<Message>>;  // messages
  ztou5nin: Array<tilo.Entry<Thread>>;  // threads
  /** Posts a message in a channel, a thread or a direct conversation as the invoker, answering another when `reply_to` names one. History and the catch-up say what a person did in its own words, counting the messages a step of them holds. */
  send(args: {
    to: Direct | TextChannel | Thread;
    body: string;
    reply_to?: Message;
  }): Promise<Message>;
  /** Sends a view as a message: its title as what the message says, its definition for anyone who reads it to add to their own views. */
  send_view(args: {
    to: Direct | TextChannel | Thread;
    title: string;
    definition: string;
  }): Promise<Message>;
  /** Deletes a message from the channel or thread it is in. Its author, or anyone the channel's permissions allow, may: the platform undoes it rather than asking first, and the removal is a version of the message, so who removed it stays answerable. */
  delete(args: {
    to: Direct | TextChannel | Thread;
    m: Message;
  }): Promise<void>;
  /** Runs `delete` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deleteMany(items: DeleteArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Starts a thread from a message of a channel or a direct conversation, named after what the message says. */
  start_thread(args: {
    c: Direct | TextChannel;
    m: Message;
    name?: string;
  }): Promise<Thread>;
}

/** The actions `TextChannel` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TEXTCHANNEL_ACTIONS = ["send", "send_view", "delete", "deleteMany", "start_thread"] as const;

/** The `tl.so.tilo.lang.chat.TextChannel` kind at `path`. */
export function textChannel(ws: tilo.Workspace, path: string): TextChannel {
  return resourceProxy(ws, path, SURFACE, TEXTCHANNEL_ACTIONS) as unknown as TextChannel;
}

/** Message — `tl.so.tilo.lang.chat.Message`. */
export interface Message extends tilo.Resource {
  xfrqdy3y: string;  // body
  "4uwxpnw2": Message | null;  // reply_to
  kxekobh6: Thread | null;  // thread
  mwily5kw: Array<tilo.Entry<Part>>;  // parts
  "4zr7hti6": boolean;  // pinned
  ofad24rc: Record<string, string[]>;  // reactions
  dh6346no: string;  // sent_view
  tz5yw5dt: string;  // by
  mmlcexsa: number;  // written
  jcgolgdf: string;  // reacted
  // the app's `edit` action is the SDK's own: run it with `action("edit")`.
  /** Pins a message to its channel, or unpins it. */
  pin(args?: {
    pinned?: boolean;
  }): Promise<void>;
  /** Runs `pin` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  pinMany(items: PinArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Adds your reaction with an emoji to a message, or takes it back when you had reacted with it. */
  react(args: {
    emoji: string;
  }): Promise<void>;
  /** Runs `react` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  reactMany(items: ReactArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Message` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const MESSAGE_ACTIONS = ["pin", "pinMany", "react", "reactMany"] as const;

/** The `tl.so.tilo.lang.chat.Message` kind at `path`. */
export function message(ws: tilo.Workspace, path: string): Message {
  return resourceProxy(ws, path, SURFACE, MESSAGE_ACTIONS) as unknown as Message;
}

/** Thread — `tl.so.tilo.lang.chat.Thread`. */
export interface Thread extends tilo.Resource {
  xuvfifa5: string;  // name
  ijy454g6: Array<tilo.Entry<Message>>;  // messages
  "2ki7h2z5": boolean;  // archived
  /** Posts a message in a channel, a thread or a direct conversation as the invoker, answering another when `reply_to` names one. History and the catch-up say what a person did in its own words, counting the messages a step of them holds. */
  send(args: {
    to: Direct | TextChannel | Thread;
    body: string;
    reply_to?: Message;
  }): Promise<Message>;
  /** Sends a view as a message: its title as what the message says, its definition for anyone who reads it to add to their own views. */
  send_view(args: {
    to: Direct | TextChannel | Thread;
    title: string;
    definition: string;
  }): Promise<Message>;
  /** Deletes a message from the channel or thread it is in. Its author, or anyone the channel's permissions allow, may: the platform undoes it rather than asking first, and the removal is a version of the message, so who removed it stays answerable. */
  delete(args: {
    to: Direct | TextChannel | Thread;
    m: Message;
  }): Promise<void>;
  /** Runs `delete` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deleteMany(items: ThreadDeleteArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Thread` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const THREAD_ACTIONS = ["send", "send_view", "delete", "deleteMany"] as const;

/** The `tl.so.tilo.lang.chat.Thread` kind at `path`. */
export function thread(ws: tilo.Workspace, path: string): Thread {
  return resourceProxy(ws, path, SURFACE, THREAD_ACTIONS) as unknown as Thread;
}

/** Direct — `tl.so.tilo.lang.chat.Direct`. */
export interface Direct extends tilo.Resource {
  "5stv6ro7": string;  // name
  ycadyqh5: string;  // person
  y65whtrn: Array<tilo.Entry<Message>>;  // messages
  "3p73kpuw": Array<tilo.Entry<Thread>>;  // threads
  /** Posts a message in a channel, a thread or a direct conversation as the invoker, answering another when `reply_to` names one. History and the catch-up say what a person did in its own words, counting the messages a step of them holds. */
  send(args: {
    to: Direct | TextChannel | Thread;
    body: string;
    reply_to?: Message;
  }): Promise<Message>;
  /** Sends a view as a message: its title as what the message says, its definition for anyone who reads it to add to their own views. */
  send_view(args: {
    to: Direct | TextChannel | Thread;
    title: string;
    definition: string;
  }): Promise<Message>;
  /** Deletes a message from the channel or thread it is in. Its author, or anyone the channel's permissions allow, may: the platform undoes it rather than asking first, and the removal is a version of the message, so who removed it stays answerable. */
  delete(args: {
    to: Direct | TextChannel | Thread;
    m: Message;
  }): Promise<void>;
  /** Runs `delete` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deleteMany(items: DirectDeleteArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Starts a thread from a message of a channel or a direct conversation, named after what the message says. */
  start_thread(args: {
    c: Direct | TextChannel;
    m: Message;
    name?: string;
  }): Promise<Thread>;
}

/** The actions `Direct` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DIRECT_ACTIONS = ["send", "send_view", "delete", "deleteMany", "start_thread"] as const;

/** The `tl.so.tilo.lang.chat.Direct` kind at `path`. */
export function direct(ws: tilo.Workspace, path: string): Direct {
  return resourceProxy(ws, path, SURFACE, DIRECT_ACTIONS) as unknown as Direct;
}

/** Part — `tl.so.tilo.lang.chat.Part`. */
export interface Part extends tilo.Resource {
  zevnbs3i: string;  // kind
  "3hkzkqvr": string;  // body
}

/** The actions `Part` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PART_ACTIONS = [] as const;

/** The `tl.so.tilo.lang.chat.Part` kind at `path`. */
export function part(ws: tilo.Workspace, path: string): Part {
  return resourceProxy(ws, path, SURFACE, PART_ACTIONS) as unknown as Part;
}

/** Chat — `tl.so.tilo.lang.chat`. */
export interface Chat extends tilo.Resource {
  /** Posts a message in a channel, a thread or a direct conversation as the invoker, answering another when `reply_to` names one. History and the catch-up say what a person did in its own words, counting the messages a step of them holds. */
  send(args: {
    to: Direct | TextChannel | Thread;
    body: string;
    reply_to?: Message;
  }): Promise<Message>;
  /** Sends a view as a message: its title as what the message says, its definition for anyone who reads it to add to their own views. */
  send_view(args: {
    to: Direct | TextChannel | Thread;
    title: string;
    definition: string;
  }): Promise<Message>;
  /** Deletes a message from the channel or thread it is in. Its author, or anyone the channel's permissions allow, may: the platform undoes it rather than asking first, and the removal is a version of the message, so who removed it stays answerable. */
  delete(args: {
    to: Direct | TextChannel | Thread;
    m: Message;
  }): Promise<void>;
  /** Runs `delete` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deleteMany(items: ChatDeleteArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Starts a thread from a message of a channel or a direct conversation, named after what the message says. */
  start_thread(args: {
    c: Direct | TextChannel;
    m: Message;
    name?: string;
  }): Promise<Thread>;
}

/** The actions `Chat` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CHAT_ACTIONS = ["send", "send_view", "delete", "deleteMany", "start_thread"] as const;

/** The `tl.so.tilo.lang.chat` kind at `path`. */
export function chat(ws: tilo.Workspace, path: string): Chat {
  return resourceProxy(ws, path, SURFACE, CHAT_ACTIONS) as unknown as Chat;
}

