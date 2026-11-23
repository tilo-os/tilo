/**
 * Types for Chat (`so.tilo.chat`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Conversations` is a `Resource` of the app's `tl.so.tilo.chat.Conversations`, made by its factory with the
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
export const SURFACE = "so.tilo.chat@d21a7a397512be3f";

/** The arguments of `delete_channel`. */
export interface DeleteChannelArgs {
  ch: TextChannel;
}

/** The arguments of `delete_category`. */
export interface DeleteCategoryArgs {
  c: Category;
}

/** The arguments of `move_channel`. */
export interface MoveChannelArgs {
  ch: TextChannel | VoiceChannel;
  category?: Category;
  at?: number;
}

/** The arguments of `move_category`. */
export interface MoveCategoryArgs {
  c: Category;
  at?: number;
}

/** The arguments of `delete_channel`. */
export interface CategoryDeleteChannelArgs {
  ch: TextChannel;
}

/** The arguments of `delete`. */
export interface DeleteArgs {
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

/** The arguments of `decide`. */
export interface DecideArgs {
  decision: string;
}

/** The arguments of `delete`. */
export interface ThreadDeleteArgs {
  m: Message;
}

/** The arguments of `delete`. */
export interface DirectDeleteArgs {
  m: Message;
}

/** The arguments of `correct_skill`. */
export interface CorrectSkillArgs {
  name?: string;
  work?: string;
  how?: string;
  form?: string;
  model?: string;
}

/** The arguments of `forget_skill`. */
export interface ForgetSkillArgs {
}

/** The arguments of `keep_example`. */
export interface KeepExampleArgs {
  text: string;
  accepted: boolean;
  note?: string;
}

/** The arguments of `drop_example`. */
export interface DropExampleArgs {
  example: Example;
}

/** Conversations — `tl.so.tilo.chat.Conversations`. */
export interface Conversations extends tilo.Resource {
  "5kzdptyg": Array<tilo.Entry<Category | Direct | TextChannel | VoiceChannel>>;  // channels
  n2xgh234: Array<tilo.Entry<Skill>>;  // skills
  /** Adds a text channel to the sidebar, about `topic` when it is given. */
  new_channel(args?: {
    name?: string;
    topic?: string;
  }): Promise<TextChannel>;
  /** Adds a category to the sidebar. */
  new_category(args?: {
    name?: string;
  }): Promise<Category>;
  /** Adds a voice channel to the sidebar. */
  new_voice_channel(args?: {
    name?: string;
  }): Promise<VoiceChannel>;
  /** Starts a skill, called `name`. What it is for and how its work is done are written on its page, or by an agent told to share the guidance with the workspace. */
  new_skill(args?: {
    name?: string;
  }): Promise<Skill>;
  /** Files a problem report, what someone ran into while using Tilo, in the workspace's #feedback channel, which the first report opens. The people who read the reports there answer, react and start a thread under one as under any message. The channel is made under one key on every device, so reports filed apart open one. */
  report_problem(args: {
    body: string;
  }): Promise<Message>;
  /** Opens the direct message between the invoker and `who`, in the sidebar, or the one already open between the two of them: two people have one conversation, not a new empty one each time. With yourself it is your notes to self. */
  message_person(args: {
    who: string;
  }): Promise<Direct>;
  /** Writes `body` to `who`, a person or an agent, in the one direct conversation with them: the one already open, else a new one in the sidebar. The message is sent as every message is. */
  write_to(args: {
    who: string;
    body: string;
  }): Promise<Message>;
  /** Deletes a channel and what was said in it, from the sidebar or the category it is in. */
  delete_channel(args: {
    ch: TextChannel;
  }): Promise<void>;
  /** Runs `delete_channel` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  delete_channelMany(items: DeleteChannelArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Deletes a category that holds no channel. */
  delete_category(args: {
    c: Category;
  }): Promise<void>;
  /** Runs `delete_category` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  delete_categoryMany(items: DeleteCategoryArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Moves a channel to a place in the sidebar: under `category`, or outside any, at `at` among the channels listed there (the end when it is negative). A move keeps the channel and what was written in it. */
  move_channel(args: {
    ch: TextChannel | VoiceChannel;
    category?: Category;
    at?: number;
  }): Promise<void>;
  /** Runs `move_channel` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  move_channelMany(items: MoveChannelArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Moves a category to a place in the sidebar, at `at` among the categories and channels listed there (the end when it is negative). Its own channels move with it. */
  move_category(args: {
    c: Category;
    at?: number;
  }): Promise<void>;
  /** Runs `move_category` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  move_categoryMany(items: MoveCategoryArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Conversations` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CONVERSATIONS_ACTIONS = ["new_channel", "new_category", "new_voice_channel", "new_skill", "report_problem", "message_person", "write_to", "delete_channel", "delete_channelMany", "delete_category", "delete_categoryMany", "move_channel", "move_channelMany", "move_category", "move_categoryMany"] as const;

/** The `tl.so.tilo.chat.Conversations` kind at `path`. */
export function conversations(ws: tilo.Workspace, path: string): Conversations {
  return resourceProxy(ws, path, SURFACE, CONVERSATIONS_ACTIONS) as unknown as Conversations;
}

/** Category — `tl.so.tilo.chat.Category`. */
export interface Category extends tilo.Resource {
  hbhrds5c: string;  // name
  hvqgmwrk: Array<tilo.Entry<TextChannel | VoiceChannel>>;  // channels
  /** Adds a text channel to a category, about `topic` when it is given. */
  add_channel(args?: {
    name?: string;
    topic?: string;
  }): Promise<TextChannel>;
  /** Adds a voice channel to a category. */
  add_voice_channel(args?: {
    name?: string;
  }): Promise<VoiceChannel>;
  /** Deletes a channel and what was said in it, from the sidebar or the category it is in. */
  delete_channel(args: {
    ch: TextChannel;
  }): Promise<void>;
  /** Runs `delete_channel` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  delete_channelMany(items: CategoryDeleteChannelArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Category` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CATEGORY_ACTIONS = ["add_channel", "add_voice_channel", "delete_channel", "delete_channelMany"] as const;

/** The `tl.so.tilo.chat.Category` kind at `path`. */
export function category(ws: tilo.Workspace, path: string): Category {
  return resourceProxy(ws, path, SURFACE, CATEGORY_ACTIONS) as unknown as Category;
}

/** VoiceChannel — `tl.so.tilo.chat.VoiceChannel`. */
export interface VoiceChannel extends tilo.Resource {
  olqjezxu: string;  // name
}

/** The actions `VoiceChannel` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const VOICECHANNEL_ACTIONS = [] as const;

/** The `tl.so.tilo.chat.VoiceChannel` kind at `path`. */
export function voiceChannel(ws: tilo.Workspace, path: string): VoiceChannel {
  return resourceProxy(ws, path, SURFACE, VOICECHANNEL_ACTIONS) as unknown as VoiceChannel;
}

/** TextChannel — `tl.so.tilo.chat.TextChannel`. */
export interface TextChannel extends tilo.Resource {
  x3haugpo: string;  // name
  u6b35mgm: string;  // topic
  ksuxpopi: Array<tilo.Entry<Message>>;  // messages
  ztou5nin: Array<tilo.Entry<Thread>>;  // threads
  kgihivmr: Array<tilo.Entry<Report>>;  // reports
  /** Posts a message in a channel, a thread or a direct conversation as the invoker, answering another when `reply_to` names one. History and the catch-up say what a person did in its own words, counting the messages a step of them holds. */
  send(args: {
    body: string;
    reply_to?: Message;
    attachments?: unknown[];
  }): Promise<Message>;
  /** Sends a view as a message: its title as what the message says, its definition for anyone who reads it to add to their own views. */
  send_view(args: {
    title: string;
    definition: string;
  }): Promise<Message>;
  /** Posts a message written on another service, as it was written there: who wrote it, as that service names them, where and when. A bridge relays another service's conversation this way; the message's history names the bridge, and the message says whom it relays. */
  relay(args: {
    body: string;
    by: string;
    written_in: string;
    written?: number;
    attachments?: unknown[];
    reply_to?: Message;
  }): Promise<Message>;
  /** Deletes a message from the channel or thread it is in. Its author, or anyone the channel's permissions allow, may: the platform undoes it rather than asking first, and the removal is a version of the message, so who removed it stays answerable. Its reports stay, saying it was deleted. */
  delete(args: {
    m: Message;
  }): Promise<void>;
  /** Runs `delete` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deleteMany(items: DeleteArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Reports a message of a conversation to the workspace's moderators, saying why when the reporter does. Who reported it is the report's history's, so moderators see who asked; the conversation keeps it, with who wrote the message and what it said, after the message is deleted. */
  report(args: {
    m: Message;
    reason?: string;
  }): Promise<Report>;
  /** Starts a thread from a message of a channel or a direct conversation, named after what the message says. */
  start_thread(args: {
    m: Message;
    name?: string;
  }): Promise<Thread>;
}

/** The actions `TextChannel` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TEXTCHANNEL_ACTIONS = ["send", "send_view", "relay", "delete", "deleteMany", "report", "start_thread"] as const;

/** The `tl.so.tilo.chat.TextChannel` kind at `path`. */
export function textChannel(ws: tilo.Workspace, path: string): TextChannel {
  return resourceProxy(ws, path, SURFACE, TEXTCHANNEL_ACTIONS) as unknown as TextChannel;
}

/** Message — `tl.so.tilo.chat.Message`. */
export interface Message extends tilo.Resource {
  xfrqdy3y: string;  // body
  "4uwxpnw2": Message | null;  // reply_to
  kxekobh6: Thread | null;  // thread
  mwily5kw: Array<tilo.Entry<Part>>;  // parts
  "4zr7hti6": boolean;  // pinned
  ofad24rc: Record<string, string[]>;  // reactions
  dh6346no: string;  // sent_view
  tz5yw5dt: string;  // by
  "4da5gcom": string;  // written_in
  mmlcexsa: number;  // written
  jcgolgdf: string;  // reacted
  n7d27huf: Array<tilo.Entry<Attached>>;  // attachments
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

/** The `tl.so.tilo.chat.Message` kind at `path`. */
export function message(ws: tilo.Workspace, path: string): Message {
  return resourceProxy(ws, path, SURFACE, MESSAGE_ACTIONS) as unknown as Message;
}

/** Report — `tl.so.tilo.chat.Report`. */
export interface Report extends tilo.Resource {
  jqcabz2h: Message | null;  // message
  e3zqz33a: string | null;  // person
  adip7bw4: string;  // said
  bmfbgqt6: string;  // reason
  "3oner4zg": string;  // decision
  /** Settles a report: what a moderator, or the moderation agent, decided about it. */
  decide(args: {
    decision: string;
  }): Promise<void>;
  /** Runs `decide` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  decideMany(items: DecideArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Report` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const REPORT_ACTIONS = ["decide", "decideMany"] as const;

/** The `tl.so.tilo.chat.Report` kind at `path`. */
export function report(ws: tilo.Workspace, path: string): Report {
  return resourceProxy(ws, path, SURFACE, REPORT_ACTIONS) as unknown as Report;
}

/** Thread — `tl.so.tilo.chat.Thread`. */
export interface Thread extends tilo.Resource {
  xuvfifa5: string;  // name
  ijy454g6: Array<tilo.Entry<Message>>;  // messages
  "2ki7h2z5": boolean;  // archived
  ep7fvntj: Array<tilo.Entry<Report>>;  // reports
  /** Posts a message in a channel, a thread or a direct conversation as the invoker, answering another when `reply_to` names one. History and the catch-up say what a person did in its own words, counting the messages a step of them holds. */
  send(args: {
    body: string;
    reply_to?: Message;
    attachments?: unknown[];
  }): Promise<Message>;
  /** Sends a view as a message: its title as what the message says, its definition for anyone who reads it to add to their own views. */
  send_view(args: {
    title: string;
    definition: string;
  }): Promise<Message>;
  /** Posts a message written on another service, as it was written there: who wrote it, as that service names them, where and when. A bridge relays another service's conversation this way; the message's history names the bridge, and the message says whom it relays. */
  relay(args: {
    body: string;
    by: string;
    written_in: string;
    written?: number;
    attachments?: unknown[];
    reply_to?: Message;
  }): Promise<Message>;
  /** Deletes a message from the channel or thread it is in. Its author, or anyone the channel's permissions allow, may: the platform undoes it rather than asking first, and the removal is a version of the message, so who removed it stays answerable. Its reports stay, saying it was deleted. */
  delete(args: {
    m: Message;
  }): Promise<void>;
  /** Runs `delete` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deleteMany(items: ThreadDeleteArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Reports a message of a conversation to the workspace's moderators, saying why when the reporter does. Who reported it is the report's history's, so moderators see who asked; the conversation keeps it, with who wrote the message and what it said, after the message is deleted. */
  report(args: {
    m: Message;
    reason?: string;
  }): Promise<Report>;
}

/** The actions `Thread` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const THREAD_ACTIONS = ["send", "send_view", "relay", "delete", "deleteMany", "report"] as const;

/** The `tl.so.tilo.chat.Thread` kind at `path`. */
export function thread(ws: tilo.Workspace, path: string): Thread {
  return resourceProxy(ws, path, SURFACE, THREAD_ACTIONS) as unknown as Thread;
}

/** Direct — `tl.so.tilo.chat.Direct`. */
export interface Direct extends tilo.Resource {
  "5stv6ro7": string;  // name
  wph25aa5: string[];  // people
  ycadyqh5: string;  // person
  y65whtrn: Array<tilo.Entry<Message>>;  // messages
  "3p73kpuw": Array<tilo.Entry<Thread>>;  // threads
  doiukw5i: Array<tilo.Entry<Report>>;  // reports
  /** Posts a message in a channel, a thread or a direct conversation as the invoker, answering another when `reply_to` names one. History and the catch-up say what a person did in its own words, counting the messages a step of them holds. */
  send(args: {
    body: string;
    reply_to?: Message;
    attachments?: unknown[];
  }): Promise<Message>;
  /** Sends a view as a message: its title as what the message says, its definition for anyone who reads it to add to their own views. */
  send_view(args: {
    title: string;
    definition: string;
  }): Promise<Message>;
  /** Posts a message written on another service, as it was written there: who wrote it, as that service names them, where and when. A bridge relays another service's conversation this way; the message's history names the bridge, and the message says whom it relays. */
  relay(args: {
    body: string;
    by: string;
    written_in: string;
    written?: number;
    attachments?: unknown[];
    reply_to?: Message;
  }): Promise<Message>;
  /** Deletes a message from the channel or thread it is in. Its author, or anyone the channel's permissions allow, may: the platform undoes it rather than asking first, and the removal is a version of the message, so who removed it stays answerable. Its reports stay, saying it was deleted. */
  delete(args: {
    m: Message;
  }): Promise<void>;
  /** Runs `delete` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deleteMany(items: DirectDeleteArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Reports a message of a conversation to the workspace's moderators, saying why when the reporter does. Who reported it is the report's history's, so moderators see who asked; the conversation keeps it, with who wrote the message and what it said, after the message is deleted. */
  report(args: {
    m: Message;
    reason?: string;
  }): Promise<Report>;
  /** Starts a thread from a message of a channel or a direct conversation, named after what the message says. */
  start_thread(args: {
    m: Message;
    name?: string;
  }): Promise<Thread>;
}

/** The actions `Direct` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DIRECT_ACTIONS = ["send", "send_view", "relay", "delete", "deleteMany", "report", "start_thread"] as const;

/** The `tl.so.tilo.chat.Direct` kind at `path`. */
export function direct(ws: tilo.Workspace, path: string): Direct {
  return resourceProxy(ws, path, SURFACE, DIRECT_ACTIONS) as unknown as Direct;
}

/** Attached — `tl.so.tilo.chat.Attached`. */
export interface Attached extends tilo.Resource {
  qsdc77jc: string;  // name
  h5lqkkjt: string;  // media
  g7nx7a7g: number;  // size
  wu2n7dpm: string;  // blob
}

/** The actions `Attached` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ATTACHED_ACTIONS = [] as const;

/** The `tl.so.tilo.chat.Attached` kind at `path`. */
export function attached(ws: tilo.Workspace, path: string): Attached {
  return resourceProxy(ws, path, SURFACE, ATTACHED_ACTIONS) as unknown as Attached;
}

/** Part — `tl.so.tilo.chat.Part`. */
export interface Part extends tilo.Resource {
  zevnbs3i: string;  // kind
  "3hkzkqvr": string;  // body
}

/** The actions `Part` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PART_ACTIONS = [] as const;

/** The `tl.so.tilo.chat.Part` kind at `path`. */
export function part(ws: tilo.Workspace, path: string): Part {
  return resourceProxy(ws, path, SURFACE, PART_ACTIONS) as unknown as Part;
}

/** Skill — `tl.so.tilo.chat.Skill`. */
export interface Skill extends tilo.Resource {
  gfk6pm4a: string;  // name
  gnfldsvz: string;  // work
  bn3w5doj: string;  // how
  ijpa4i7d: string;  // form
  "3wlkwi6k": string;  // model
  bxx5rykt: Array<tilo.Entry<Example>>;  // examples
  /** Corrects a skill: only what is given changes, and what is left out stays as it is. */
  correct_skill(args?: {
    name?: string;
    work?: string;
    how?: string;
    form?: string;
    model?: string;
  }): Promise<void>;
  /** Runs `correct_skill` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  correct_skillMany(items: CorrectSkillArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Forgets a skill: no later turn reads it. */
  forget_skill(): Promise<void>;
  /** Runs `forget_skill` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  forget_skillMany(items: ForgetSkillArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Keeps an example of a skill: a result the person accepted (more like this) or rejected (less like this), with what they said about it. The same result said again is that example, moved newest, and a skill keeps its newest twelve. */
  keep_example(args: {
    text: string;
    accepted: boolean;
    note?: string;
  }): Promise<void>;
  /** Runs `keep_example` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  keep_exampleMany(items: KeepExampleArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes an example out of a skill. */
  drop_example(args: {
    example: Example;
  }): Promise<void>;
  /** Runs `drop_example` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  drop_exampleMany(items: DropExampleArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Skill` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SKILL_ACTIONS = ["correct_skill", "correct_skillMany", "forget_skill", "forget_skillMany", "keep_example", "keep_exampleMany", "drop_example", "drop_exampleMany"] as const;

/** The `tl.so.tilo.chat.Skill` kind at `path`. */
export function skill(ws: tilo.Workspace, path: string): Skill {
  return resourceProxy(ws, path, SURFACE, SKILL_ACTIONS) as unknown as Skill;
}

/** Example — `tl.so.tilo.chat.Example`. */
export interface Example extends tilo.Resource {
  fspydtu6: string;  // text
  "2u2ngevv": boolean;  // accepted
  agcun6lt: string;  // note
  "3ccutu3r": number;  // at
}

/** The actions `Example` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const EXAMPLE_ACTIONS = [] as const;

/** The `tl.so.tilo.chat.Example` kind at `path`. */
export function example(ws: tilo.Workspace, path: string): Example {
  return resourceProxy(ws, path, SURFACE, EXAMPLE_ACTIONS) as unknown as Example;
}

/** Chat — `tl.so.tilo.chat`. */
export interface Chat extends tilo.Resource {
}

/** The actions `Chat` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CHAT_ACTIONS = [] as const;

/** The `tl.so.tilo.chat` kind at `path`. */
export function chat(ws: tilo.Workspace, path: string): Chat {
  return resourceProxy(ws, path, SURFACE, CHAT_ACTIONS) as unknown as Chat;
}

