/**
 * Types for Mail (`so.tilo.mail`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Account` is a `Resource` of the app's `tl.so.tilo.mail.Account`, made by its factory with the
 * resource's path (`mail/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.mail@ba1d8ef8586d61fd";

/** The arguments of `set_signature`. */
export interface SetSignatureArgs {
  signature: string;
}

/** The arguments of `mark_all_read`. */
export interface MarkAllReadArgs {
}

/** The arguments of `archive`. */
export interface ArchiveArgs {
}

/** The arguments of `to_inbox`. */
export interface ToInboxArgs {
}

/** The arguments of `move_to`. */
export interface MoveToArgs {
  to: Mailbox;
}

/** The arguments of `delete`. */
export interface DeleteArgs {
}

/** The arguments of `flag`. */
export interface FlagArgs {
  flagged?: boolean;
}

/** The arguments of `snooze`. */
export interface SnoozeArgs {
  until: string;
}

/** The arguments of `snooze_an_hour`. */
export interface SnoozeAnHourArgs {
}

/** The arguments of `snooze_until_tomorrow`. */
export interface SnoozeUntilTomorrowArgs {
}

/** The arguments of `unsnooze`. */
export interface UnsnoozeArgs {
}

/** The arguments of `label`. */
export interface LabelArgs {
  name: string;
  on?: boolean;
}

/** The arguments of `mute`. */
export interface MuteArgs {
  muted?: boolean;
}

/** The arguments of `mark_read`. */
export interface MarkReadArgs {
  unread?: boolean;
}

/** The arguments of `attach`. */
export interface AttachArgs {
  file: unknown;
}

/** The arguments of `detach`. */
export interface DetachArgs {
  name: string;
}

/** The arguments of `send_from`. */
export interface SendFromArgs {
  a: Account;
}

/** The arguments of `send_as`. */
export interface SendAsArgs {
  identity: string;
}

/** The arguments of `set_hold`. */
export interface SetHoldArgs {
  seconds: number;
}

/** The arguments of `rename_label`. */
export interface RenameLabelArgs {
  l: Label;
  name: string;
}

/** The arguments of `delete_label`. */
export interface DeleteLabelArgs {
  l: Label;
}

/** The arguments of `discard`. */
export interface DiscardArgs {
  d: Draft;
}

/** Account — `tl.so.tilo.mail.Account`. */
export interface Account extends tilo.Resource {
  yujnud5c: string;  // key
  "4lo3ei5n": string;  // address
  j6nqqrit: string;  // name
  b5k7mvka: string;  // trouble
  "33f3rqw2": boolean;  // online
  hzbwof46: string;  // fetched
  jxc5rufs: string;  // signature
  yinmk5zb: string;  // identities
  ehmoyzk2: Array<tilo.Entry<Mailbox>>;  // mailboxes
  qybqg4dl: Array<tilo.Entry<Thread>>;  // threads
  /** Sets or clears the workspace account's signature. */
  set_signature(args: {
    signature: string;
  }): Promise<void>;
  /** Runs `set_signature` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_signatureMany(items: SetSignatureArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Asks the account's server for its mail now: what a person presses when the window says the server could not be reached, or that what it fetched could not be saved. The account says how it stands once it answers. */
  check_now(): Promise<void>;
  /** Starts a message from the account. */
  new_message(): Promise<Draft>;
  /** Marks every conversation of the account read. */
  mark_all_read(): Promise<void>;
  /** Runs `mark_all_read` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  mark_all_readMany(items: MarkAllReadArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Account` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ACCOUNT_ACTIONS = ["set_signature", "set_signatureMany", "check_now", "new_message", "mark_all_read", "mark_all_readMany"] as const;

/** The `tl.so.tilo.mail.Account` kind at `path`. */
export function account(ws: tilo.Workspace, path: string): Account {
  return resourceProxy(ws, path, SURFACE, ACCOUNT_ACTIONS) as unknown as Account;
}

/** Mailbox — `tl.so.tilo.mail.Mailbox`. */
export interface Mailbox extends tilo.Resource {
  mh47w3p6: string;  // key
  tlasysm5: string;  // name
  xhiqdk3p: string;  // role
  dtd4czyv: number;  // unread
  rxoe5vzb: number;  // total
  al2vjuet: boolean;  // more
  /** Asks the account's server for the messages older than the oldest this workspace holds in the mailbox, so a folder longer than one page can be read back to its first message. They come in as new mail does, and the mailbox stops offering them once its first message is here. A mailbox whose server holds nothing older answers nothing, as a list reads to its end asking whether a page is there. */
  older_messages(): Promise<void>;
}

/** The actions `Mailbox` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const MAILBOX_ACTIONS = ["older_messages"] as const;

/** The `tl.so.tilo.mail.Mailbox` kind at `path`. */
export function mailbox(ws: tilo.Workspace, path: string): Mailbox {
  return resourceProxy(ws, path, SURFACE, MAILBOX_ACTIONS) as unknown as Mailbox;
}

/** Thread — `tl.so.tilo.mail.Thread`. */
export interface Thread extends tilo.Resource {
  ecmpoary: string;  // key
  zu23m2dm: string;  // account
  u732lnuk: string;  // subject
  o3knjgos: string;  // latest
  sxcvmlon: string;  // snippet
  vyc7aj7v: string;  // senders
  izafodav: number;  // count
  "56lwowdg": boolean;  // unread
  abe6c5jl: boolean;  // flagged
  "3vusnp7i": string[];  // boxes
  ueyfpzep: Array<tilo.Entry<Message>>;  // messages
  gy7z7du3: string[];  // labels
  bdoh33hs: string;  // snoozed_until
  z7qxz544: boolean;  // muted
  /** Answers the conversation's newest message, to whoever wrote it, quoting it under what is written. */
  reply(): Promise<Draft>;
  /** Answers the conversation's newest message, to everyone it went to, quoting it. */
  reply_all(): Promise<Draft>;
  /** Sends the conversation's newest message on, with its sender, what it says and the files it came with. */
  forward(): Promise<Draft>;
  /** Archives the conversation: its messages leave the inbox. */
  archive(): Promise<void>;
  /** Runs `archive` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  archiveMany(items: ArchiveArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Moves the conversation back to the inbox, and unmutes it: what a person brings back is what they want to see again. */
  to_inbox(): Promise<void>;
  /** Runs `to_inbox` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  to_inboxMany(items: ToInboxArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Moves the conversation to another mailbox of its account. */
  move_to(args: {
    to: Mailbox;
  }): Promise<void>;
  /** Runs `move_to` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  move_toMany(items: MoveToArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Deletes the conversation: its messages go to the trash. */
  delete(): Promise<void>;
  /** Runs `delete` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deleteMany(items: DeleteArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Deletes the conversation's trashed messages for good, from its server too, and the conversation with them when no message of it is left. This is the one deletion there is no trash to take back from: what is deleted for good is gone from the server, and the platform takes back this app's record of it only. */
  purge(): Promise<void>;
  /** Flags or unflags the conversation. */
  flag(args?: {
    flagged?: boolean;
  }): Promise<void>;
  /** Runs `flag` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  flagMany(items: FlagArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Puts the conversation away until a moment: it leaves the inbox, and comes back once that moment has come, with no window open. The moment is a field, so it survives closing and reopening, and an agent snoozes the same way. */
  snooze(args: {
    until: string;
  }): Promise<void>;
  /** Runs `snooze` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  snoozeMany(items: SnoozeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Puts the conversation away for an hour, the snooze people reach for most. */
  snooze_an_hour(): Promise<void>;
  /** Runs `snooze_an_hour` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  snooze_an_hourMany(items: SnoozeAnHourArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Puts the conversation away until tomorrow morning, at nine, in the person's own zone. */
  snooze_until_tomorrow(): Promise<void>;
  /** Runs `snooze_until_tomorrow` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  snooze_until_tomorrowMany(items: SnoozeUntilTomorrowArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Brings a snoozed conversation back now, whatever its moment was: it is in the inbox again. */
  unsnooze(): Promise<void>;
  /** Runs `unsnooze` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  unsnoozeMany(items: UnsnoozeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Gives the conversation a label, or takes one away: `on` says which. The label joins the workspace's labels the first time it is given, so it has a filter of its own from then on. */
  label(args: {
    name: string;
    on?: boolean;
  }): Promise<void>;
  /** Runs `label` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  labelMany(items: LabelArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Mutes the conversation, or unmutes it: a muted one stays out of the inbox until it is brought back, whatever its new messages do. */
  mute(args?: {
    muted?: boolean;
  }): Promise<void>;
  /** Runs `mute` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  muteMany(items: MuteArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Marks the conversation read, or unread again. */
  mark_read(args?: {
    unread?: boolean;
  }): Promise<void>;
  /** Runs `mark_read` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  mark_readMany(items: MarkReadArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Answers the conversation with what was typed: the message goes to whoever wrote its newest, as a reply to it, quoted. */
  answer(args: {
    body: string;
  }): Promise<Draft>;
}

/** The actions `Thread` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const THREAD_ACTIONS = ["reply", "reply_all", "forward", "archive", "archiveMany", "to_inbox", "to_inboxMany", "move_to", "move_toMany", "delete", "deleteMany", "purge", "flag", "flagMany", "snooze", "snoozeMany", "snooze_an_hour", "snooze_an_hourMany", "snooze_until_tomorrow", "snooze_until_tomorrowMany", "unsnooze", "unsnoozeMany", "label", "labelMany", "mute", "muteMany", "mark_read", "mark_readMany", "answer"] as const;

/** The `tl.so.tilo.mail.Thread` kind at `path`. */
export function thread(ws: tilo.Workspace, path: string): Thread {
  return resourceProxy(ws, path, SURFACE, THREAD_ACTIONS) as unknown as Thread;
}

/** Label — `tl.so.tilo.mail.Label`. */
export interface Label extends tilo.Resource {
  ammoua3h: string;  // name
}

/** The actions `Label` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LABEL_ACTIONS = [] as const;

/** The `tl.so.tilo.mail.Label` kind at `path`. */
export function label(ws: tilo.Workspace, path: string): Label {
  return resourceProxy(ws, path, SURFACE, LABEL_ACTIONS) as unknown as Label;
}

/** Message — `tl.so.tilo.mail.Message`. */
export interface Message extends tilo.Resource {
  kzjq6hvc: string;  // key
  kdxoo3c6: string;  // account
  ntqgnz4f: string;  // subject
  l3nc65iv: string;  // from
  mo6l36a3: string;  // from_name
  "46k23iwq": string;  // to
  bgzxugi5: string;  // cc
  tfxwkfn2: string;  // received
  sbrol5fc: string;  // snippet
  "4bchz3ch": string;  // body
  plb4mg2p: string;  // html
  anuds3qt: string;  // message_id
  asu6ymha: string;  // references
  yltl32r3: unknown[];  // attachments
  ubfcf6ep: boolean;  // unread
  vxxb4sqr: boolean;  // flagged
  xlejsz5j: string;  // mailbox
  hgysjvfy: string;  // original
  wuefabwr: string;  // parts
  /** The message as a Markdown file: what a person exports to read it, or keep it, elsewhere. */
  export_markdown(): Promise<unknown>;
  /** The message as a JSON file: what a script or an agent reads its record from. */
  export_json(): Promise<unknown>;
  /** The message as an `.eml` file, which every mail program opens. A message taken in from outside an account's server is exported as it was sent; one its server holds is written from what the workspace keeps of it. */
  export_eml(): Promise<unknown>;
  /** Answers one message of the conversation, to whoever wrote it, quoting it. */
  reply_to(): Promise<Draft>;
  /** Answers one message of the conversation, to everyone it went to but the person answering. */
  reply_all_to(): Promise<Draft>;
  /** Sends one message of the conversation on, with its sender, when and what it says, and the files it came with. */
  forward_message(): Promise<Draft>;
  /** One file the message came with, as a file of its own: what Save puts in Files, and what an agent reads or hands another app. */
  attachment(args: {
    name: string;
  }): Promise<unknown>;
}

/** The actions `Message` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const MESSAGE_ACTIONS = ["export_markdown", "export_json", "export_eml", "reply_to", "reply_all_to", "forward_message", "attachment"] as const;

/** The `tl.so.tilo.mail.Message` kind at `path`. */
export function message(ws: tilo.Workspace, path: string): Message {
  return resourceProxy(ws, path, SURFACE, MESSAGE_ACTIONS) as unknown as Message;
}

/** Draft — `tl.so.tilo.mail.Draft`. */
export interface Draft extends tilo.Resource {
  m2g6zrle: string;  // from
  zx2b4ax2: string;  // from_address
  hd3o4gkq: string;  // to
  cwukhrvn: string;  // cc
  ib44zkec: string;  // bcc
  tyxmravh: string;  // subject
  d5u2c7jm: string;  // body
  w6twdkob: string;  // in_reply_to
  w26tzvt4: string;  // references
  yfyt2cfh: boolean;  // sent
  lgrwf5yf: string;  // send_at
  h2nvr2op: boolean;  // recalled
  em6rrmae: string;  // invitation
  "2tq2zegc": unknown[];  // attachments
  ozukvva2: string;  // set_aside
  /** Takes a file along with the message: one the person picks, or one an agent hands over. A message takes up to sixteen files of up to 32 MB each, as mail servers take them. */
  attach(args: {
    file: unknown;
  }): Promise<void>;
  /** Runs `attach` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  attachMany(items: AttachArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Leaves a file the message was to take along behind. */
  detach(args: {
    name: string;
  }): Promise<void>;
  /** Runs `detach` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  detachMany(items: DetachArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Brings a message back to the outbox: it was sent to go at a moment still to come, and it is a draft again, whatever the moment. */
  cancel_later(): Promise<void>;
  /** Sends the message at a moment instead of now: it waits in the outbox until then, as the person's hold makes a message wait, and goes with no window open. Until it goes it can be taken back, and bringing it back leaves it a draft. */
  send_later(args: {
    at: string;
  }): Promise<void>;
  /** Sends the message at nine tomorrow, the moment people reach for most when they write in the evening and do not want it to arrive then. */
  send_in_the_morning(): Promise<void>;
  /** Sends the message from another workspace account. */
  send_from(args: {
    a: Account;
  }): Promise<void>;
  /** Runs `send_from` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  send_fromMany(items: SendFromArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sends the message as another of its account's names or addresses, `Name <address>`: what a person writing as a role rather than as themselves chooses. Empty sends as the account's own. */
  send_as(args: {
    identity: string;
  }): Promise<void>;
  /** Runs `send_as` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  send_asMany(items: SendAsArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sends the message from its account. It leaves the system, so it goes once, and waits in the outbox until the server took it; it is held back for the person's hold first, so they can take it back. The effect is an argument the action records, so the wait survives a window closing and an agent takes it back the same way. */
  send(): Promise<void>;
  /** Takes back a message that was sent and has not gone yet: it never leaves, and the message is a draft again, as it was. Once it went it cannot be taken back. */
  undo_send(): Promise<void>;
}

/** The actions `Draft` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DRAFT_ACTIONS = ["attach", "attachMany", "detach", "detachMany", "cancel_later", "send_later", "send_in_the_morning", "send_from", "send_fromMany", "send_as", "send_asMany", "send", "undo_send"] as const;

/** The `tl.so.tilo.mail.Draft` kind at `path`. */
export function draft(ws: tilo.Workspace, path: string): Draft {
  return resourceProxy(ws, path, SURFACE, DRAFT_ACTIONS) as unknown as Draft;
}

/** Mail — `tl.so.tilo.mail.Mail`. */
export interface Mail extends tilo.Resource {
  blig7xcq: Array<tilo.Entry<Account>>;  // accounts
  gkuo27e6: Array<tilo.Entry<Draft>>;  // drafts
  z3zcztpz: Array<tilo.Entry<Label>>;  // labels
  bag7bf25: Array<tilo.Entry<Thread>>;  // recovered_mail
  "4ow5ydy5": number;  // hold
  /** Sets how many seconds a sent message waits before it goes, so it can be taken back: no wait at all at zero. */
  set_hold(args: {
    seconds: number;
  }): Promise<void>;
  /** Runs `set_hold` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_holdMany(items: SetHoldArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Starts a message from the first account. */
  new_draft(): Promise<Draft>;
  /** Writes a message for the person to read and send: to whom, its subject and what it says, answering `in_reply_to` (a Message-ID) when it names one. Every call makes a draft of its own beside the others; nothing is written over and nothing is sent. Where an agent drafts a message, the person finds it in Drafts and sends it. */
  draft_message(args: {
    to: string;
    subject?: string;
    body?: string;
    cc?: string;
    in_reply_to?: string;
    from?: string;
  }): Promise<Draft>;
  /** Starts a message inviting `to` to an event, its invitation (`calendar_ics`, iCalendar) carried beside what it says: what Calendar hands Mail for each guest it invites. Nothing is sent; the person reads it, and sends it. */
  prepare_calendar_invitation(args: {
    to: string;
    subject: string;
    body: string;
    calendar_ics: string;
  }): Promise<Draft>;
  /** Takes in a message that reached the workspace from outside its accounts' servers: one moved in from another mail program (`tilo migrate`), or handed over through the API. It is kept with the mail held outside the accounts, in the conversation it belongs to (`conversation`, the Message-ID the conversation began with) and the folder it was in, read or not. `from` is the sender as a header writes it (`Ada Lovelace <ada@example.com>`), and `original` the content reference of the message as it was sent, less its large parts: `parts` says where each of those stands in it and which pieces of content it is in, one JSON text each (`{'at': 120, 'size': 4096, 'pieces': ['tilo.content:…']}`). `attachments` are the files it carries, each content the workspace holds. A message already held under its Message-ID is answered as it is, so delivering it again keeps it once. */
  deliver_message(args: {
    from: string;
    subject?: string;
    body?: string;
    folder?: string;
    read?: boolean;
    to?: string;
    cc?: string;
    date?: string;
    conversation?: string;
    message_id?: string;
    original?: string;
    parts?: string[];
    attachments?: unknown[];
  }): Promise<Message>;
  /** Opens a message kept as a file a mail program writes (`.eml`, a single message of an mbox, or Apple Mail's `.emlx`): its headers are read, and it is kept with the mail held outside the accounts, read, in the inbox. The same file opened again is the message already kept. */
  open_message_file(args: {
    file: unknown;
  }): Promise<Message>;
  /** Renames one of the workspace's labels, all the conversations that carry it with it. */
  rename_label(args: {
    l: Label;
    name: string;
  }): Promise<void>;
  /** Runs `rename_label` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  rename_labelMany(items: RenameLabelArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Deletes one of the workspace's labels, and takes it off every conversation that carries it. */
  delete_label(args: {
    l: Label;
  }): Promise<void>;
  /** Runs `delete_label` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  delete_labelMany(items: DeleteLabelArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Throws a message being written away. */
  discard(args: {
    d: Draft;
  }): Promise<void>;
  /** Runs `discard` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  discardMany(items: DiscardArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Mail` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const MAIL_ACTIONS = ["set_hold", "set_holdMany", "new_draft", "draft_message", "prepare_calendar_invitation", "deliver_message", "open_message_file", "rename_label", "rename_labelMany", "delete_label", "delete_labelMany", "discard", "discardMany"] as const;

/** The `tl.so.tilo.mail.Mail` kind at `path`. */
export function mail(ws: tilo.Workspace, path: string): Mail {
  return resourceProxy(ws, path, SURFACE, MAIL_ACTIONS) as unknown as Mail;
}

/** Mail — `tl.so.tilo.mail`. */
export interface TiloMail extends tilo.Resource {
}

/** The actions `TiloMail` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TILOMAIL_ACTIONS = [] as const;

/** The `tl.so.tilo.mail` kind at `path`. */
export function tiloMail(ws: tilo.Workspace, path: string): TiloMail {
  return resourceProxy(ws, path, SURFACE, TILOMAIL_ACTIONS) as unknown as TiloMail;
}

