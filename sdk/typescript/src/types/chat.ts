/**
 * Types for Chat (`so.tilo.chat`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.json`; write
 * it again rather than editing it.
 *
 * `Notice` is a `Resource` of the app's `tl.tilo.host.Notice`, made by its factory with the
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
export const SURFACE = "so.tilo.chat@d33fddee8d636e96";

/** One emoji of the workspace. */
export interface Emoji {
  image: unknown;  // Its picture
  added_by: string;  // Who added it: they may remove it again
  added_at: number;  // When, in milliseconds since the Unix epoch
}

/** One decision, with the reasoning that led to it. */
export interface Decision {
  id: string;  // Id
  person: string;  // Person
  reports: string[];  // The reports it settles
  outcome: Outcome;  // Outcome
  until_ms: number | null;  // When a restriction lapses
  reasoning: string;  // The model's reasoning, or the Moderator's, in their words
  model: string;  // The model that judged, empty for a person's own decision
  reverted: Reverted[];  // What it took back
  by: string;  // Who decided: the agent or the Moderator
  at_ms: number;  // At ms
  overturned_by: string | null;  // Overturned by
  overturn_note: string | null;  // Overturn note
  overturned_at_ms: number | null;  // Overturned at ms
}

/** What a decision does to the person. One of "no-action", "restrict", "ban". */
export type Outcome = "no-action" | "restrict" | "ban";

/** One report of a person, optionally pointing at what they did. */
export interface Report {
  id: string;  // Id
  reporter: string;  // Who reported; the moderation agent for its own heuristics
  person: string;  // Whom the report is about
  app: string | null;  // The app, resource and item reported, when the report is about content
  record: string | null;  // Record
  item: string | null;  // Item
  reason: string;  // Reason
  at_ms: number;  // At ms
  decision: string | null;  // The decision that settled it
}

/** A resource whose changes by the person a decision took back, and the run that did it, so overturning can take the take-back back. */
export interface Reverted {
  app: string;  // App
  record: string;  // Record
  run: string;  // The history run the moderation agent's undo ran in
  changes: number;  // How many of the person's changes it took back
}

/** What was decided about a person, and why. */
export interface Ruling {
  outcome: Outcome;
  until_ms: number | null;
  reasoning: string;
  model: string;
  reverted: Reverted[];
}

/** The arguments of `delete_channel`. */
export interface DeleteChannelArgs {
  ch: TextChannel | VoiceChannel;
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

/** The arguments of `bridge`. */
export interface BridgeArgs {
  service: string;
  outside_id?: string;
}

/** The arguments of `unbridge`. */
export interface UnbridgeArgs {
}

/** The arguments of `delete_channel`. */
export interface CategoryDeleteChannelArgs {
  ch: TextChannel | VoiceChannel;
}

/** The arguments of `rename_category`. */
export interface RenameCategoryArgs {
  name: string;
}

/** The arguments of `bridge`. */
export interface VoiceChannelBridgeArgs {
  service: string;
  outside_id?: string;
}

/** The arguments of `unbridge`. */
export interface VoiceChannelUnbridgeArgs {
}

/** The arguments of `rename_voice_channel`. */
export interface RenameVoiceChannelArgs {
  name: string;
}

/** The arguments of `set_hearing`. */
export interface SetHearingArgs {
  who: string;
  percent: number;
}

/** The arguments of `silence`. */
export interface SilenceArgs {
  who: string;
  on?: boolean;
}

/** The arguments of `bridge`. */
export interface TextChannelBridgeArgs {
  service: string;
  outside_id?: string;
}

/** The arguments of `unbridge`. */
export interface TextChannelUnbridgeArgs {
}

/** The arguments of `summarize`. */
export interface SummarizeArgs {
  summary: string;
}

/** The arguments of `rename_channel`. */
export interface RenameChannelArgs {
  name: string;
}

/** The arguments of `set_topic`. */
export interface SetTopicArgs {
  topic: string;
}

/** The arguments of `archive_channel`. */
export interface ArchiveChannelArgs {
  archived?: boolean;
}

/** The arguments of `delete`. */
export interface DeleteArgs {
  m: Message;
}

/** The arguments of `open_channel`. */
export interface OpenChannelArgs {
}

/** The arguments of `edit-block`. */
export interface EditBlockArgs {
  block: string;
  text: string;
}

/** The arguments of `insert-block`. */
export interface InsertBlockArgs {
  block: string;
  text: string;
}

/** The arguments of `delete-block`. */
export interface DeleteBlockArgs {
  block: string;
}

/** The arguments of `set_share_access`. */
export interface SetShareAccessArgs {
  can_edit: boolean;
}

/** The arguments of `stop_sharing`. */
export interface StopSharingArgs {
}

/** The arguments of `relay_reaction`. */
export interface RelayReactionArgs {
  emoji: string;
  persona: string;
  by: string;
  given?: boolean;
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

/** The arguments of `summarize`. */
export interface ThreadSummarizeArgs {
  summary: string;
}

/** The arguments of `archive_thread`. */
export interface ArchiveThreadArgs {
  archived?: boolean;
}

/** The arguments of `delete`. */
export interface ThreadDeleteArgs {
  m: Message;
}

/** The arguments of `open_thread`. */
export interface OpenThreadArgs {
}

/** The arguments of `bridge`. */
export interface DirectBridgeArgs {
  service: string;
  outside_id?: string;
}

/** The arguments of `unbridge`. */
export interface DirectUnbridgeArgs {
}

/** The arguments of `summarize`. */
export interface DirectSummarizeArgs {
  summary: string;
}

/** The arguments of `delete`. */
export interface DirectDeleteArgs {
  m: Message;
}

/** The arguments of `open_direct`. */
export interface OpenDirectArgs {
}

/** The arguments of `edit-block`. */
export interface DraftEditBlockArgs {
  block: string;
  text: string;
}

/** The arguments of `insert-block`. */
export interface DraftInsertBlockArgs {
  block: string;
  text: string;
}

/** The arguments of `delete-block`. */
export interface DraftDeleteBlockArgs {
  block: string;
}

/** The arguments of `attach`. */
export interface AttachArgs {
  files: unknown[];
}

/** The arguments of `detach`. */
export interface DetachArgs {
  file: unknown;
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

/** The arguments of `add-emoji`. */
export interface AddEmojiArgs {
  name: string;
  image: string;
}

/** The arguments of `remove-emoji`. */
export interface RemoveEmojiArgs {
  name: string;
}

/** The arguments of `rename-emoji`. */
export interface RenameEmojiArgs {
  name: string;
  to: string;
}

/** The arguments of `rename-any-emoji`. */
export interface RenameAnyEmojiArgs {
  name: string;
  to: string;
}

/** The arguments of `remove-any-emoji`. */
export interface RemoveAnyEmojiArgs {
  name: string;
}

/** The arguments of `report-message`. */
export interface ReportMessageArgs {
  message: string;
  author: unknown;
  reason: string;
}

/** The arguments of `report-person`. */
export interface ReportPersonArgs {
  person: unknown;
  reason: string;
}

/** The arguments of `decide-reports`. */
export interface DecideReportsArgs {
  id: string;
  person: unknown;
  reports: string[];
  ruling: Ruling;
}

/** The arguments of `overturn-decision`. */
export interface OverturnDecisionArgs {
  decision: string;
  note: string;
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

/** Conversations — `tl.so.tilo.chat.Conversations`. */
export interface Conversations extends tilo.Resource {
  "5kzdptyg": Array<tilo.Entry<Category | Direct | TextChannel | VoiceChannel>>;  // channels
  n2xgh234: Array<tilo.Entry<Skill>>;  // skills
  "64vv225r": Array<tilo.Entry<Draft>>;  // drafts
  "7spkrvju": Array<tilo.Entry<ChatReport>>;  // reports
  /** Adds a text channel to the sidebar, under `category` when it is given and about `topic` when that is. */
  new_channel(args?: {
    name?: string;
    topic?: string;
    category?: Category;
  }): Promise<TextChannel>;
  /** Adds a category to the sidebar. */
  new_category(args?: {
    name?: string;
  }): Promise<Category>;
  /** Adds a voice channel to the sidebar, under `category` when it is given. */
  new_voice_channel(args?: {
    name?: string;
    category?: Category;
  }): Promise<VoiceChannel>;
  /** Starts a skill, called `name`. What it is for and how its work is done are written on its page, or by an agent told to share the guidance with the workspace. */
  new_skill(args?: {
    name?: string;
  }): Promise<Skill>;
  /** Files a problem report, what someone ran into while using Tilo, in the workspace's #feedback channel, which the first report opens. The people who read the reports there answer, react and start a thread under one as under any message. The channel is made under one key on every device, so reports filed apart open one. A report carries a recording of what happened where one was kept (Capture's last half minute, or any recording the person chose): it rides on the message as its file, and the report's row offers to replay it. What else it carries (a picture of the window it is about) rides after it. */
  report_problem(args: {
    body: string;
    recording?: unknown;
    attachments?: unknown[];
  }): Promise<Message>;
  /** Opens the direct message between the invoker and `who`, in the sidebar, or the one already open between the two of them: two people have one conversation, not a new empty one each time. With yourself it is your notes to self. A person is someone of the workspace: one who is not in it is refused, since no one but its members reads what is written here. */
  message_person(args: {
    who: string;
  }): Promise<Direct>;
  /** The direct conversation between the invoker and `who`, none while the two of them have none: what an agent, a script or another app reads to find it, opening and making nothing. With yourself it is your notes to self. */
  conversation_with(args: {
    who: string;
  }): Promise<Direct | null>;
  /** Writes `body` to `who`, a person or an agent, in the one direct conversation with them: the one already open, else a new one in the sidebar. The message is sent as every message is. */
  write_to(args: {
    who: string;
    body: string;
  }): Promise<Message>;
  /** Deletes a channel and what was said in it, from the sidebar or the category it is in. */
  delete_channel(args: {
    ch: TextChannel | VoiceChannel;
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
  /** Reports a person to the workspace's moderators, for how they behave here rather than for one message: what the reporter says is the whole of it, and the moderators decide it as any other report. */
  report_person(args: {
    who: string;
    reason: string;
  }): Promise<ChatReport>;
  /** Bans a person from the workspace: they leave it and stay out until a moderator lifts the ban. The workspace's own ban decides who may, its owners and moderators; Chat keeps the ban on the moderators' Reports page as a decided report, which overturning lifts. */
  ban(args: {
    who: string;
    reason: string;
  }): Promise<ChatReport>;
  /** Asks the assistant to start a sub-agent for `job`, in the direct conversation it answers in: the record's own `start-sub-agent` is the assistant's to call there, and the sub-agent reports in that conversation, where the person follows it. None where the workspace keeps no conversation with the assistant yet. */
  ask_for_sub_agent(args: {
    agent: unknown;
    job?: string;
  }): Promise<Message | null>;
}

/** The actions `Conversations` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CONVERSATIONS_ACTIONS = ["new_channel", "new_category", "new_voice_channel", "new_skill", "report_problem", "message_person", "conversation_with", "write_to", "delete_channel", "delete_channelMany", "delete_category", "delete_categoryMany", "move_channel", "move_channelMany", "move_category", "move_categoryMany", "report_person", "ban", "ask_for_sub_agent"] as const;

/** The `tl.so.tilo.chat.Conversations` kind at `path`. */
export function conversations(ws: tilo.Workspace, path: string): Conversations {
  return resourceProxy(ws, path, SURFACE, CONVERSATIONS_ACTIONS) as unknown as Conversations;
}

/** Category — `tl.so.tilo.chat.Category`. */
export interface Category extends tilo.Resource {
  hbhrds5c: string;  // name
  hvqgmwrk: Array<tilo.Entry<TextChannel | VoiceChannel>>;  // channels
  "5pfbdqqj": string;  // service
  vvs4ecqz: string;  // outside_id
  /** How one kind of work is done in this workspace: what a person writes down once, and what an agent reads before it starts that work. Carries a conversation to an outside service: its name as people read it ('Discord') and the id it has there. The bridge's own actions keep what joins the two sides; this is what every view reads to show the place a conversation is also said in. */
  bridge(args: {
    service: string;
    outside_id?: string;
  }): Promise<void>;
  /** Runs `bridge` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  bridgeMany(items: BridgeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Keeps a conversation here: no service carries it any more. */
  unbridge(): Promise<void>;
  /** Runs `unbridge` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  unbridgeMany(items: UnbridgeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Deletes a channel and what was said in it, from the sidebar or the category it is in. */
  delete_channel(args: {
    ch: TextChannel | VoiceChannel;
  }): Promise<void>;
  /** Runs `delete_channel` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  delete_channelMany(items: CategoryDeleteChannelArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Renames a category. Its channels stay in it. */
  rename_category(args: {
    name: string;
  }): Promise<void>;
  /** Runs `rename_category` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  rename_categoryMany(items: RenameCategoryArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Category` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CATEGORY_ACTIONS = ["bridge", "bridgeMany", "unbridge", "unbridgeMany", "delete_channel", "delete_channelMany", "rename_category", "rename_categoryMany"] as const;

/** The `tl.so.tilo.chat.Category` kind at `path`. */
export function category(ws: tilo.Workspace, path: string): Category {
  return resourceProxy(ws, path, SURFACE, CATEGORY_ACTIONS) as unknown as Category;
}

/** VoiceChannel — `tl.so.tilo.chat.VoiceChannel`. */
export interface VoiceChannel extends tilo.Resource {
  olqjezxu: string;  // name
  enzjdbyp: string;  // service
  nvzanb5n: string;  // outside_id
  /** How one kind of work is done in this workspace: what a person writes down once, and what an agent reads before it starts that work. Carries a conversation to an outside service: its name as people read it ('Discord') and the id it has there. The bridge's own actions keep what joins the two sides; this is what every view reads to show the place a conversation is also said in. */
  bridge(args: {
    service: string;
    outside_id?: string;
  }): Promise<void>;
  /** Runs `bridge` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  bridgeMany(items: VoiceChannelBridgeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Keeps a conversation here: no service carries it any more. */
  unbridge(): Promise<void>;
  /** Runs `unbridge` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  unbridgeMany(items: VoiceChannelUnbridgeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Renames a voice channel. A call in it is untouched. */
  rename_voice_channel(args: {
    name: string;
  }): Promise<void>;
  /** Runs `rename_voice_channel` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  rename_voice_channelMany(items: RenameVoiceChannelArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `VoiceChannel` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const VOICECHANNEL_ACTIONS = ["bridge", "bridgeMany", "unbridge", "unbridgeMany", "rename_voice_channel", "rename_voice_channelMany"] as const;

/** The `tl.so.tilo.chat.VoiceChannel` kind at `path`. */
export function voiceChannel(ws: tilo.Workspace, path: string): VoiceChannel {
  return resourceProxy(ws, path, SURFACE, VOICECHANNEL_ACTIONS) as unknown as VoiceChannel;
}

/** Hearing — `tl.so.tilo.chat.Hearing`. */
export interface Hearing extends tilo.Resource {
  ua6lm2xh: Array<tilo.Entry<Heard>>;  // people
  /** Plays `who` in calls on this device at `percent` of as they arrive, and hears them again if they were silenced. Only this device plays them so; nobody else hears a difference. */
  set_hearing(args: {
    who: string;
    percent: number;
  }): Promise<void>;
  /** Runs `set_hearing` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_hearingMany(items: SetHearingArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Silences `who` in calls on this device, or hears them again at the level they had. Only this person stops hearing them; everyone else in the call still does. */
  silence(args: {
    who: string;
    on?: boolean;
  }): Promise<void>;
  /** Runs `silence` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  silenceMany(items: SilenceArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Hearing` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const HEARING_ACTIONS = ["set_hearing", "set_hearingMany", "silence", "silenceMany"] as const;

/** The `tl.so.tilo.chat.Hearing` kind at `path`. */
export function hearing(ws: tilo.Workspace, path: string): Hearing {
  return resourceProxy(ws, path, SURFACE, HEARING_ACTIONS) as unknown as Hearing;
}

/** Heard — `tl.so.tilo.chat.Heard`. */
export interface Heard extends tilo.Resource {
  a2np4gvd: string;  // who
  u4nfj5zx: number;  // percent
  xtnm4w32: boolean;  // silenced
}

/** The actions `Heard` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const HEARD_ACTIONS = [] as const;

/** The `tl.so.tilo.chat.Heard` kind at `path`. */
export function heard(ws: tilo.Workspace, path: string): Heard {
  return resourceProxy(ws, path, SURFACE, HEARD_ACTIONS) as unknown as Heard;
}

/** TextChannel — `tl.so.tilo.chat.TextChannel`. */
export interface TextChannel extends tilo.Resource {
  x3haugpo: string;  // name
  u6b35mgm: string;  // topic
  fld77g72: string;  // summary
  ksuxpopi: Array<tilo.Entry<Message>>;  // messages
  ztou5nin: Array<tilo.Entry<Thread>>;  // threads
  kgihivmr: Array<tilo.Entry<ChatReport>>;  // reports
  byf6kyop: boolean;  // archived
  eqr5qd3v: string;  // service
  elvv3ebj: string;  // outside_id
  /** How one kind of work is done in this workspace: what a person writes down once, and what an agent reads before it starts that work. Carries a conversation to an outside service: its name as people read it ('Discord') and the id it has there. The bridge's own actions keep what joins the two sides; this is what every view reads to show the place a conversation is also said in. */
  bridge(args: {
    service: string;
    outside_id?: string;
  }): Promise<void>;
  /** Runs `bridge` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  bridgeMany(items: TextChannelBridgeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Keeps a conversation here: no service carries it any more. */
  unbridge(): Promise<void>;
  /** Runs `unbridge` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  unbridgeMany(items: TextChannelUnbridgeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Posts a message in a channel, a thread or a direct conversation as the invoker, answering another when `reply_to` names one. History and the catch-up say what a person did in its own words, counting the messages a step of them holds. A message in a direct conversation is news to the other people in it: each is told once, on every device of theirs, who wrote and what, and choosing the notice opens the conversation. A composer names the person's `draft`: what it holds is the message, and the draft is emptied in the same change, so the press is one step to undo. What is sent without a draft is the `body` given, as a program writing a message says it. A reply an agent's `turn` writes begins empty and takes the model's words as it writes them. */
  send(args?: {
    body?: string;
    reply_to?: Message;
    attachments?: unknown[];
    turn?: string;
    draft?: Draft;
  }): Promise<Message>;
  /** Writes down what has been said in a conversation so far, replacing what was written before: what an agent's next turn there reads in place of the messages it covers. */
  summarize(args: {
    summary: string;
  }): Promise<void>;
  /** Runs `summarize` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  summarizeMany(items: SummarizeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sends a view as a message: its title as what the message says, its definition for anyone who reads it to add to their own views. */
  send_view(args: {
    title: string;
    definition: string;
  }): Promise<Message>;
  /** Shares one of the person's windows live into a conversation: the shell shares the window with the workspace, as its own window bar does, and the message refers to that share, so the conversation shows the window live where the message stands. Its sharer chooses whether the members viewing it may control it. */
  share_window(args: {
    window: string;
    can_edit?: boolean;
  }): Promise<Message>;
  /** Posts a message written on another service, as it was written there: who wrote it, as that service names them, where and when. A bridge relays another service's conversation this way; the message's history names the bridge, and the message says whom it relays. */
  relay(args: {
    body: string;
    by: string;
    written_in: string;
    written?: number;
    attachments?: unknown[];
    reply_to?: Message;
    avatar?: string;
    service?: string;
    outside_emoji?: OutsideEmoji[];
    member?: string;
  }): Promise<Message>;
  /** Renames a text channel. What was said in it stays. */
  rename_channel(args: {
    name: string;
  }): Promise<void>;
  /** Runs `rename_channel` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  rename_channelMany(items: RenameChannelArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Says what a channel is for, in its topic. */
  set_topic(args: {
    topic: string;
  }): Promise<void>;
  /** Runs `set_topic` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_topicMany(items: SetTopicArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sets a channel aside, out of the sidebar, keeping everything said in it, or brings it back. */
  archive_channel(args?: {
    archived?: boolean;
  }): Promise<void>;
  /** Runs `archive_channel` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  archive_channelMany(items: ArchiveChannelArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Asks the agent that wrote `m` to carry on where its answer stopped: a reply to it from the person, naming the agent, which the agent's next turn reads with the answer before it. An answer that ran out of room, was stopped or broke off is finished this way, not written again from its start. */
  continue_answer(args: {
    m: Message;
  }): Promise<Message>;
  /** Deletes a message from the channel or thread it is in. Its author, or anyone the channel's permissions allow, may: the platform undoes it rather than asking first, and the removal is a version of the message, so who removed it stays answerable. Its reports stay, saying it was deleted, as far as the one deleting reads them. */
  delete(args: {
    m: Message;
  }): Promise<void>;
  /** Runs `delete` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deleteMany(items: DeleteArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Reports a message of a conversation to the workspace's moderators, saying why when the reporter does. Who reported it is the report's history's, so moderators see who asked; the conversation keeps it, with who wrote the message and what it said, after the message is deleted. */
  report(args: {
    m: Message;
    reason?: string;
  }): Promise<ChatReport>;
  /** A conversation as a Markdown file, which the system's one Export… lists and saves where the person says: every message in order, each under the name it shows with and the time it was written. */
  export_markdown(): Promise<unknown>;
  /** Starts a thread from a message of a channel or a direct conversation, named after what the message says. */
  start_thread(args: {
    m: Message;
    name?: string;
  }): Promise<Thread>;
  /** Opening a channel makes the draft its person writes in, when they have none: a press away and back finds what they typed, on every device of theirs, and nobody else reads it. */
  open_channel(): Promise<void>;
  /** Runs `open_channel` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  open_channelMany(items: OpenChannelArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `TextChannel` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TEXTCHANNEL_ACTIONS = ["bridge", "bridgeMany", "unbridge", "unbridgeMany", "send", "summarize", "summarizeMany", "send_view", "share_window", "relay", "rename_channel", "rename_channelMany", "set_topic", "set_topicMany", "archive_channel", "archive_channelMany", "continue_answer", "delete", "deleteMany", "report", "export_markdown", "start_thread", "open_channel", "open_channelMany"] as const;

/** The `tl.so.tilo.chat.TextChannel` kind at `path`. */
export function textChannel(ws: tilo.Workspace, path: string): TextChannel {
  return resourceProxy(ws, path, SURFACE, TEXTCHANNEL_ACTIONS) as unknown as TextChannel;
}

/** OutsideAuthor — `tl.so.tilo.chat.OutsideAuthor`. */
export interface OutsideAuthor extends tilo.Resource {
  "5fxndhc2": string;  // name
  zkszdahn: string;  // avatar
  yycajap4: string;  // service
  ew25p5fb: string | null;  // member
}

/** The actions `OutsideAuthor` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const OUTSIDEAUTHOR_ACTIONS = [] as const;

/** The `tl.so.tilo.chat.OutsideAuthor` kind at `path`. */
export function outsideAuthor(ws: tilo.Workspace, path: string): OutsideAuthor {
  return resourceProxy(ws, path, SURFACE, OUTSIDEAUTHOR_ACTIONS) as unknown as OutsideAuthor;
}

/** LiveShare — `tl.so.tilo.chat.LiveShare`. */
export interface LiveShare extends tilo.Resource {
  noscllor: string;  // window
  n4s3rmdn: string;  // workspace
  k535m2ok: string;  // title
  ntg5vyds: string;  // app_name
  dzpk5b7g: string;  // icon
  a4wmd33j: boolean;  // can_edit
  jfgns7zp: boolean;  // ended
}

/** The actions `LiveShare` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LIVESHARE_ACTIONS = [] as const;

/** The `tl.so.tilo.chat.LiveShare` kind at `path`. */
export function liveShare(ws: tilo.Workspace, path: string): LiveShare {
  return resourceProxy(ws, path, SURFACE, LIVESHARE_ACTIONS) as unknown as LiveShare;
}

/** OutsideEmoji — `tl.so.tilo.chat.OutsideEmoji`. */
export interface OutsideEmoji extends tilo.Resource {
  ldtylgfu: string;  // name
  jjutyqgx: string;  // picture
}

/** The actions `OutsideEmoji` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const OUTSIDEEMOJI_ACTIONS = [] as const;

/** The `tl.so.tilo.chat.OutsideEmoji` kind at `path`. */
export function outsideEmoji(ws: tilo.Workspace, path: string): OutsideEmoji {
  return resourceProxy(ws, path, SURFACE, OUTSIDEEMOJI_ACTIONS) as unknown as OutsideEmoji;
}

/** Message — `tl.so.tilo.chat.Message`. */
export interface Message extends tilo.Resource {
  xfrqdy3y: string;  // body
  "4uwxpnw2": Message | null;  // reply_to
  kxekobh6: Thread | null;  // thread
  mwily5kw: Array<tilo.Entry<Part>>;  // parts
  "4zr7hti6": boolean;  // pinned
  hj4axvbu: string;  // turn
  t2kepujk: string;  // reasoning
  ofad24rc: Record<string, string[]>;  // reactions
  dh6346no: string;  // sent_view
  "4upqznm4": LiveShare | null;  // live_share
  wqxsenb3: OutsideAuthor | null;  // outside
  bbu6225x: Array<tilo.Entry<OutsideEmoji>>;  // outside_emoji
  tz5yw5dt: string;  // by
  "4da5gcom": string;  // written_in
  mmlcexsa: number;  // written
  jcgolgdf: string;  // reacted
  "6luxdz2x": Array<tilo.Entry<RelayedReaction>>;  // relayed_reactions
  n7d27huf: Array<tilo.Entry<Attached>>;  // attachments
  /** Set one paragraph of a rich-text field to what a Markdown text says. */
  editBlock(args: {
    block: string;
    text: string;
  }): Promise<void>;
  /** Runs `edit-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  editBlockMany(items: EditBlockArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Put a paragraph before the one the address names. */
  insertBlock(args: {
    block: string;
    text: string;
  }): Promise<void>;
  /** Runs `insert-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  insertBlockMany(items: InsertBlockArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Remove the paragraph the address names, and the paragraphs it holds. */
  deleteBlock(args: {
    block: string;
  }): Promise<void>;
  /** Runs `delete-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deleteBlockMany(items: DeleteBlockArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Lets the members viewing a window shared into a conversation control it, or only view it: the shell's share changes, and the message says so. */
  set_share_access(args: {
    can_edit: boolean;
  }): Promise<void>;
  /** Runs `set_share_access` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_share_accessMany(items: SetShareAccessArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Stops sharing a window shared into a conversation: the shell's share ends, and the message says the window is no longer shared in its place. */
  stop_sharing(): Promise<void>;
  /** Runs `stop_sharing` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  stop_sharingMany(items: StopSharingArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Relays a reaction someone gave a message where it was written, as that place names them: the counterpart of `relay` for a reaction. A bridge carrying another service's conversation here makes these, so a reaction made there shows under the person's name rather than the bridge's; `given` false takes it back. The reaction's history names the bridge, and the message says whom it relays. */
  relay_reaction(args: {
    emoji: string;
    persona: string;
    by: string;
    given?: boolean;
  }): Promise<void>;
  /** Runs `relay_reaction` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  relay_reactionMany(items: RelayReactionArgs[]): Promise<tilo.ActionAnswer[]>;
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
  /** Copies what a message says, as plain words, to the clipboard. */
  copy_text(): Promise<void>;
}

/** The actions `Message` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const MESSAGE_ACTIONS = ["editBlock", "editBlockMany", "insertBlock", "insertBlockMany", "deleteBlock", "deleteBlockMany", "set_share_access", "set_share_accessMany", "stop_sharing", "stop_sharingMany", "relay_reaction", "relay_reactionMany", "pin", "pinMany", "react", "reactMany", "copy_text"] as const;

/** The `tl.so.tilo.chat.Message` kind at `path`. */
export function message(ws: tilo.Workspace, path: string): Message {
  return resourceProxy(ws, path, SURFACE, MESSAGE_ACTIONS) as unknown as Message;
}

/** Report — `tl.so.tilo.chat.Report`. */
export interface ChatReport extends tilo.Resource {
  jqcabz2h: Message | null;  // message
  e3zqz33a: string | null;  // person
  adip7bw4: string;  // said
  bmfbgqt6: string;  // reason
  "3oner4zg": string;  // decision
  maczz6ef: string;  // overturned
  "5gbnohgv": boolean;  // banned
  /** Settles a report: what a moderator decided about it. A report whose decision was overturned is decided again this way. */
  decide(args: {
    decision: string;
  }): Promise<void>;
  /** Runs `decide` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  decideMany(items: DecideArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Overturns what was decided about a report, saying why: the report is open again, for a moderator to decide anew, and the decision overturned stays in its history. A ban it decided is lifted, so the person may come back. */
  overturn(args: {
    why: string;
  }): Promise<void>;
  /** Settles a report by banning the person it is about from the workspace, as [`ban`] does. */
  ban_reported(): Promise<void>;
}

/** The actions `ChatReport` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CHATREPORT_ACTIONS = ["decide", "decideMany", "overturn", "ban_reported"] as const;

/** The `tl.so.tilo.chat.Report` kind at `path`. */
export function chatReport(ws: tilo.Workspace, path: string): ChatReport {
  return resourceProxy(ws, path, SURFACE, CHATREPORT_ACTIONS) as unknown as ChatReport;
}

/** Thread — `tl.so.tilo.chat.Thread`. */
export interface Thread extends tilo.Resource {
  xuvfifa5: string;  // name
  cv664irm: string;  // summary
  ijy454g6: Array<tilo.Entry<Message>>;  // messages
  "2ki7h2z5": boolean;  // archived
  ep7fvntj: Array<tilo.Entry<ChatReport>>;  // reports
  /** Posts a message in a channel, a thread or a direct conversation as the invoker, answering another when `reply_to` names one. History and the catch-up say what a person did in its own words, counting the messages a step of them holds. A message in a direct conversation is news to the other people in it: each is told once, on every device of theirs, who wrote and what, and choosing the notice opens the conversation. A composer names the person's `draft`: what it holds is the message, and the draft is emptied in the same change, so the press is one step to undo. What is sent without a draft is the `body` given, as a program writing a message says it. A reply an agent's `turn` writes begins empty and takes the model's words as it writes them. */
  send(args?: {
    body?: string;
    reply_to?: Message;
    attachments?: unknown[];
    turn?: string;
    draft?: Draft;
  }): Promise<Message>;
  /** Writes down what has been said in a conversation so far, replacing what was written before: what an agent's next turn there reads in place of the messages it covers. */
  summarize(args: {
    summary: string;
  }): Promise<void>;
  /** Runs `summarize` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  summarizeMany(items: ThreadSummarizeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sends a view as a message: its title as what the message says, its definition for anyone who reads it to add to their own views. */
  send_view(args: {
    title: string;
    definition: string;
  }): Promise<Message>;
  /** Shares one of the person's windows live into a conversation: the shell shares the window with the workspace, as its own window bar does, and the message refers to that share, so the conversation shows the window live where the message stands. Its sharer chooses whether the members viewing it may control it. */
  share_window(args: {
    window: string;
    can_edit?: boolean;
  }): Promise<Message>;
  /** Posts a message written on another service, as it was written there: who wrote it, as that service names them, where and when. A bridge relays another service's conversation this way; the message's history names the bridge, and the message says whom it relays. */
  relay(args: {
    body: string;
    by: string;
    written_in: string;
    written?: number;
    attachments?: unknown[];
    reply_to?: Message;
    avatar?: string;
    service?: string;
    outside_emoji?: OutsideEmoji[];
    member?: string;
  }): Promise<Message>;
  /** Sets a thread aside once its talk is done, keeping everything said in it, or reopens it. Its chip says so under the message it started from; a reply in it reopens it. The person who started it or a moderator does this. */
  archive_thread(args?: {
    archived?: boolean;
  }): Promise<void>;
  /** Runs `archive_thread` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  archive_threadMany(items: ArchiveThreadArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Asks the agent that wrote `m` to carry on where its answer stopped: a reply to it from the person, naming the agent, which the agent's next turn reads with the answer before it. An answer that ran out of room, was stopped or broke off is finished this way, not written again from its start. */
  continue_answer(args: {
    m: Message;
  }): Promise<Message>;
  /** Deletes a message from the channel or thread it is in. Its author, or anyone the channel's permissions allow, may: the platform undoes it rather than asking first, and the removal is a version of the message, so who removed it stays answerable. Its reports stay, saying it was deleted, as far as the one deleting reads them. */
  delete(args: {
    m: Message;
  }): Promise<void>;
  /** Runs `delete` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deleteMany(items: ThreadDeleteArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Reports a message of a conversation to the workspace's moderators, saying why when the reporter does. Who reported it is the report's history's, so moderators see who asked; the conversation keeps it, with who wrote the message and what it said, after the message is deleted. */
  report(args: {
    m: Message;
    reason?: string;
  }): Promise<ChatReport>;
  /** A conversation as a Markdown file, which the system's one Export… lists and saves where the person says: every message in order, each under the name it shows with and the time it was written. */
  export_markdown(): Promise<unknown>;
  /** [`open_channel`] for a thread. */
  open_thread(): Promise<void>;
  /** Runs `open_thread` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  open_threadMany(items: OpenThreadArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Thread` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const THREAD_ACTIONS = ["send", "summarize", "summarizeMany", "send_view", "share_window", "relay", "archive_thread", "archive_threadMany", "continue_answer", "delete", "deleteMany", "report", "export_markdown", "open_thread", "open_threadMany"] as const;

/** The `tl.so.tilo.chat.Thread` kind at `path`. */
export function thread(ws: tilo.Workspace, path: string): Thread {
  return resourceProxy(ws, path, SURFACE, THREAD_ACTIONS) as unknown as Thread;
}

/** Direct — `tl.so.tilo.chat.Direct`. */
export interface Direct extends tilo.Resource {
  "5stv6ro7": string;  // name
  wph25aa5: string[];  // people
  ycadyqh5: string;  // person
  s7ol3kl4: string;  // summary
  y65whtrn: Array<tilo.Entry<Message>>;  // messages
  "3p73kpuw": Array<tilo.Entry<Thread>>;  // threads
  doiukw5i: Array<tilo.Entry<ChatReport>>;  // reports
  a7u4jddr: string;  // service
  f4cfgfpu: string;  // outside_id
  /** How one kind of work is done in this workspace: what a person writes down once, and what an agent reads before it starts that work. Carries a conversation to an outside service: its name as people read it ('Discord') and the id it has there. The bridge's own actions keep what joins the two sides; this is what every view reads to show the place a conversation is also said in. */
  bridge(args: {
    service: string;
    outside_id?: string;
  }): Promise<void>;
  /** Runs `bridge` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  bridgeMany(items: DirectBridgeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Keeps a conversation here: no service carries it any more. */
  unbridge(): Promise<void>;
  /** Runs `unbridge` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  unbridgeMany(items: DirectUnbridgeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** How many messages of a direct conversation the person asking has not read: the number its row shows beside the other person, and what an agent or a script reads of it. */
  unread_count(): Promise<number>;
  /** Posts a message in a channel, a thread or a direct conversation as the invoker, answering another when `reply_to` names one. History and the catch-up say what a person did in its own words, counting the messages a step of them holds. A message in a direct conversation is news to the other people in it: each is told once, on every device of theirs, who wrote and what, and choosing the notice opens the conversation. A composer names the person's `draft`: what it holds is the message, and the draft is emptied in the same change, so the press is one step to undo. What is sent without a draft is the `body` given, as a program writing a message says it. A reply an agent's `turn` writes begins empty and takes the model's words as it writes them. */
  send(args?: {
    body?: string;
    reply_to?: Message;
    attachments?: unknown[];
    turn?: string;
    draft?: Draft;
  }): Promise<Message>;
  /** Writes down what has been said in a conversation so far, replacing what was written before: what an agent's next turn there reads in place of the messages it covers. */
  summarize(args: {
    summary: string;
  }): Promise<void>;
  /** Runs `summarize` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  summarizeMany(items: DirectSummarizeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sends a view as a message: its title as what the message says, its definition for anyone who reads it to add to their own views. */
  send_view(args: {
    title: string;
    definition: string;
  }): Promise<Message>;
  /** Shares one of the person's windows live into a conversation: the shell shares the window with the workspace, as its own window bar does, and the message refers to that share, so the conversation shows the window live where the message stands. Its sharer chooses whether the members viewing it may control it. */
  share_window(args: {
    window: string;
    can_edit?: boolean;
  }): Promise<Message>;
  /** Posts a message written on another service, as it was written there: who wrote it, as that service names them, where and when. A bridge relays another service's conversation this way; the message's history names the bridge, and the message says whom it relays. */
  relay(args: {
    body: string;
    by: string;
    written_in: string;
    written?: number;
    attachments?: unknown[];
    reply_to?: Message;
    avatar?: string;
    service?: string;
    outside_emoji?: OutsideEmoji[];
    member?: string;
  }): Promise<Message>;
  /** Asks the agent that wrote `m` to carry on where its answer stopped: a reply to it from the person, naming the agent, which the agent's next turn reads with the answer before it. An answer that ran out of room, was stopped or broke off is finished this way, not written again from its start. */
  continue_answer(args: {
    m: Message;
  }): Promise<Message>;
  /** Deletes a message from the channel or thread it is in. Its author, or anyone the channel's permissions allow, may: the platform undoes it rather than asking first, and the removal is a version of the message, so who removed it stays answerable. Its reports stay, saying it was deleted, as far as the one deleting reads them. */
  delete(args: {
    m: Message;
  }): Promise<void>;
  /** Runs `delete` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deleteMany(items: DirectDeleteArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Reports a message of a conversation to the workspace's moderators, saying why when the reporter does. Who reported it is the report's history's, so moderators see who asked; the conversation keeps it, with who wrote the message and what it said, after the message is deleted. */
  report(args: {
    m: Message;
    reason?: string;
  }): Promise<ChatReport>;
  /** A conversation as a Markdown file, which the system's one Export… lists and saves where the person says: every message in order, each under the name it shows with and the time it was written. */
  export_markdown(): Promise<unknown>;
  /** Starts a thread from a message of a channel or a direct conversation, named after what the message says. */
  start_thread(args: {
    m: Message;
    name?: string;
  }): Promise<Thread>;
  /** [`open_channel`] for a direct conversation. */
  open_direct(): Promise<void>;
  /** Runs `open_direct` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  open_directMany(items: OpenDirectArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Direct` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DIRECT_ACTIONS = ["bridge", "bridgeMany", "unbridge", "unbridgeMany", "unread_count", "send", "summarize", "summarizeMany", "send_view", "share_window", "relay", "continue_answer", "delete", "deleteMany", "report", "export_markdown", "start_thread", "open_direct", "open_directMany"] as const;

/** The `tl.so.tilo.chat.Direct` kind at `path`. */
export function direct(ws: tilo.Workspace, path: string): Direct {
  return resourceProxy(ws, path, SURFACE, DIRECT_ACTIONS) as unknown as Direct;
}

/** Draft — `tl.so.tilo.chat.Draft`. */
export interface Draft extends tilo.Resource {
  "5shwuvrs": Direct | TextChannel | Thread;  // conversation
  rxmavdj2: string;  // body
  o5vmbr2v: unknown[];  // files
  /** Set one paragraph of a rich-text field to what a Markdown text says. */
  editBlock(args: {
    block: string;
    text: string;
  }): Promise<void>;
  /** Runs `edit-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  editBlockMany(items: DraftEditBlockArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Put a paragraph before the one the address names. */
  insertBlock(args: {
    block: string;
    text: string;
  }): Promise<void>;
  /** Runs `insert-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  insertBlockMany(items: DraftInsertBlockArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Remove the paragraph the address names, and the paragraphs it holds. */
  deleteBlock(args: {
    block: string;
  }): Promise<void>;
  /** Runs `delete-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deleteBlockMany(items: DraftDeleteBlockArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Adds the files a person chose to the message they are writing: the system's chooser runs this with what was chosen, and the draft holds them until it is sent. The chooser offers every kind of file, since a message carries any. */
  attach(args: {
    files: unknown[];
  }): Promise<void>;
  /** Runs `attach` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  attachMany(items: AttachArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes a file off the message a person is writing, before it is sent. */
  detach(args: {
    file: unknown;
  }): Promise<void>;
  /** Runs `detach` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  detachMany(items: DetachArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Draft` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DRAFT_ACTIONS = ["editBlock", "editBlockMany", "insertBlock", "insertBlockMany", "deleteBlock", "deleteBlockMany", "attach", "attachMany", "detach", "detachMany"] as const;

/** The `tl.so.tilo.chat.Draft` kind at `path`. */
export function draft(ws: tilo.Workspace, path: string): Draft {
  return resourceProxy(ws, path, SURFACE, DRAFT_ACTIONS) as unknown as Draft;
}

/** Attached — `tl.so.tilo.chat.Attached`. */
export interface Attached extends tilo.Resource {
  qsdc77jc: string;  // name
  h5lqkkjt: string;  // media
  g7nx7a7g: number;  // size
  wu2n7dpm: string;  // blob
  /** Opens the file a message carries in the app that opens its kind: the file reaches that app as the same content both read, never as a path neither has. */
  open_attachment(): Promise<void>;
}

/** The actions `Attached` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ATTACHED_ACTIONS = ["open_attachment"] as const;

/** The `tl.so.tilo.chat.Attached` kind at `path`. */
export function attached(ws: tilo.Workspace, path: string): Attached {
  return resourceProxy(ws, path, SURFACE, ATTACHED_ACTIONS) as unknown as Attached;
}

/** RelayedReaction — `tl.so.tilo.chat.RelayedReaction`. */
export interface RelayedReaction extends tilo.Resource {
  r2zcptjt: string;  // emoji
  awgtnj33: string;  // persona
  hammizbi: string;  // by
}

/** The actions `RelayedReaction` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const RELAYEDREACTION_ACTIONS = [] as const;

/** The `tl.so.tilo.chat.RelayedReaction` kind at `path`. */
export function relayedReaction(ws: tilo.Workspace, path: string): RelayedReaction {
  return resourceProxy(ws, path, SURFACE, RELAYEDREACTION_ACTIONS) as unknown as RelayedReaction;
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

/** Workspace emoji — `tilo.workspace.emoji`. */
export interface WorkspaceEmoji extends tilo.Resource {
  emoji: Record<string, Emoji>;  // Emoji
  /** A picture (PNG or WebP, base64, at most 12 000 bytes; 64 pixels a side draws sharply) under a name of 2 to 32 letters, digits and underscores, written `:name:` in reactions and text. */
  addEmoji(args: {
    name: string;
    image: string;
  }): Promise<void>;
  /** Runs `add-emoji` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  addEmojiMany(items: AddEmojiArgs[]): Promise<tilo.ActionAnswer[]>;
  /** One you added. */
  removeEmoji(args: {
    name: string;
  }): Promise<void>;
  /** Runs `remove-emoji` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  removeEmojiMany(items: RemoveEmojiArgs[]): Promise<tilo.ActionAnswer[]>;
  /** One you added: its picture and who added it stay. Reactions and text that wrote its old `:name:` no longer draw it. */
  renameEmoji(args: {
    name: string;
    to: string;
  }): Promise<void>;
  /** Runs `rename-emoji` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  renameEmojiMany(items: RenameEmojiArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Someone else's emoji: the workspace's roles decide who may. */
  renameAnyEmoji(args: {
    name: string;
    to: string;
  }): Promise<void>;
  /** Runs `rename-any-emoji` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  renameAnyEmojiMany(items: RenameAnyEmojiArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Someone else's emoji: the workspace's roles decide who may. */
  removeAnyEmoji(args: {
    name: string;
  }): Promise<void>;
  /** Runs `remove-any-emoji` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  removeAnyEmojiMany(items: RemoveAnyEmojiArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `WorkspaceEmoji` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const WORKSPACEEMOJI_ACTIONS = ["addEmoji", "addEmojiMany", "removeEmoji", "removeEmojiMany", "renameEmoji", "renameEmojiMany", "renameAnyEmoji", "renameAnyEmojiMany", "removeAnyEmoji", "removeAnyEmojiMany"] as const;

/** The `tilo.workspace.emoji` kind at `path`. */
export function workspaceEmoji(ws: tilo.Workspace, path: string): WorkspaceEmoji {
  return resourceProxy(ws, path, SURFACE, WORKSPACEEMOJI_ACTIONS) as unknown as WorkspaceEmoji;
}

/** Moderation — `tilo.moderation`. */
export interface Moderation extends tilo.Resource {
  readonly reports: Array<tilo.Entry<Report>>;  // Reports
  readonly decisions: Array<tilo.Entry<Decision>>;  // Decisions
  /** Tells the workspace's moderators that this message, and its author, should not be here. */
  reportMessage(args: {
    message: string;
    author: unknown;
    reason: string;
  }): Promise<void>;
  /** Runs `report-message` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  reportMessageMany(items: ReportMessageArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Tells the workspace's moderators that this person is misbehaving. */
  reportPerson(args: {
    person: unknown;
    reason: string;
  }): Promise<void>;
  /** Runs `report-person` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  reportPersonMany(items: ReportPersonArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Decide on a person. Resources what was done about a person and why, settling the reports named. */
  decideReports(args: {
    id: string;
    person: unknown;
    reports: string[];
    ruling: Ruling;
  }): Promise<void>;
  /** Runs `decide-reports` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  decideReportsMany(items: DecideReportsArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Marks a decision as overturned by a Moderator, who lifts its restriction or ban and brings back what it took back. */
  overturnDecision(args: {
    decision: string;
    note: string;
  }): Promise<void>;
  /** Runs `overturn-decision` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  overturnDecisionMany(items: OverturnDecisionArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Moderation` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const MODERATION_ACTIONS = ["reportMessage", "reportMessageMany", "reportPerson", "reportPersonMany", "decideReports", "decideReportsMany", "overturnDecision", "overturnDecisionMany"] as const;

/** The `tilo.moderation` kind at `path`. */
export function moderation(ws: tilo.Workspace, path: string): Moderation {
  return resourceProxy(ws, path, SURFACE, MODERATION_ACTIONS) as unknown as Moderation;
}

