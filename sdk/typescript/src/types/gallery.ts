/**
 * Types for Gallery (`so.tilo.lang.gallery`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Item` is a `Resource` of the app's `tl.so.tilo.lang.gallery.Item`, made by its factory with the
 * resource's path (`gallery/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.lang.gallery@50b5417c4f24c633";

/** What a pinned app is, and the picture it shows on a dock. One of "notes", "tasks", "mail". */
export type Kind = "notes" | "tasks" | "mail";

/** How soon a report is due, as a case the report holds. One of "none", "low", "medium", "high". */
export type Urgency = "none" | "low" | "medium" | "high";

/** The arguments of `finish`. */
export interface FinishArgs {
}

/** The arguments of `react`. */
export interface ReactArgs {
  emoji: string;
}

/** The arguments of `add_item`. */
export interface AddItemArgs {
}

/** The arguments of `remove_item`. */
export interface RemoveItemArgs {
  x: Item;
}

/** The arguments of `note_change`. */
export interface NoteChangeArgs {
  summary: string;
}

/** The arguments of `seek_to`. */
export interface SeekToArgs {
  place: number;
}

/** Item — `tl.so.tilo.lang.gallery.Item`. */
export interface Item extends tilo.Resource {
  n5clkmib: string;  // title
  znpp3agb: string;  // notes
  xttl6aec: string;  // stage
  "44yujnht": Record<string, string[]>;  // reactions
  /** Marks an item done. */
  finish(): Promise<void>;
  /** Runs `finish` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  finishMany(items: FinishArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Reacts to an item with an emoji, or takes the person's reaction back. */
  react(args: {
    emoji: string;
  }): Promise<void>;
  /** Runs `react` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  reactMany(items: ReactArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Item` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ITEM_ACTIONS = ["finish", "finishMany", "react", "reactMany"] as const;

/** The `tl.so.tilo.lang.gallery.Item` kind at `path`. */
export function item(ws: tilo.Workspace, path: string): Item {
  return resourceProxy(ws, path, SURFACE, ITEM_ACTIONS) as unknown as Item;
}

/** Gallery — `tl.so.tilo.lang.gallery.Gallery`. */
export interface Gallery extends tilo.Resource {
  bmdkc22s: string;  // name
  cbwtcaei: string;  // basics_note
  kwuh65ek: string;  // controls_title
  gfaekx4h: boolean;  // controls_by_email
  wqgo62si: boolean;  // controls_repeating
  "4a4siddd": number;  // controls_margin
  "3aihctss": number;  // controls_share
  bfnlu2pr: string;  // controls_room
  "5rd763gr": Urgency;  // controls_urgency
  tov7hrtu: string | null;  // controls_owner
  pwg7s3ul: string | null;  // controls_reviewer
  jg35gcbg: string;  // controls_starts
  ncm2bwon: string;  // controls_clock
  hy2q7by2: string | null;  // controls_ends
  izjturst: string;  // controls_accent
  jggqxsf4: string;  // controls_shortcut
  icvcwg6u: string;  // controls_recipient
  socb2mg5: string;  // controls_note
  jxnozq52: string;  // controls_notes
  fqnths44: boolean;  // controls_background
  "6fezt7u6": boolean;  // controls_lettering
  "5fbblehc": Array<tilo.Entry<Item>>;  // backlog_items
  rbbe5kqg: string;  // backlog_draft
  frzefp4n: Array<tilo.Entry<Item>>;  // release_items
  upmpqtnv: Array<tilo.Entry<Row>>;  // data_rows
  oj2bybhm: Array<tilo.Entry<Node>>;  // data_files
  emr3i6eu: Array<tilo.Entry<Message>>;  // data_messages
  "6jhjiv7f": Array<tilo.Entry<Card>>;  // data_cards
  a3frzqb4: Array<tilo.Entry<Change>>;  // data_changes
  mo2q5faz: Array<tilo.Entry<Photo>>;  // media_photos
  qd46u3ke: number;  // media_at
  "3tdz6w5q": string;  // media_source
  wivzmqm3: Array<tilo.Entry<Pinned>>;  // chrome_apps
  /** Adds what the backlog's line says as an item to do, and clears the line. */
  add_item(): Promise<void>;
  /** Runs `add_item` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  add_itemMany(items: AddItemArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes an item off the backlog. */
  remove_item(args: {
    x: Item;
  }): Promise<void>;
  /** Runs `remove_item` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remove_itemMany(items: RemoveItemArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Note a change to the document, made by whoever asks for it. */
  note_change(args: {
    summary: string;
  }): Promise<void>;
  /** Runs `note_change` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  note_changeMany(items: NoteChangeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Move the song to a place in it, as a share of its length. */
  seek_to(args: {
    place: number;
  }): Promise<void>;
  /** Runs `seek_to` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  seek_toMany(items: SeekToArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Gallery` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const GALLERY_ACTIONS = ["add_item", "add_itemMany", "remove_item", "remove_itemMany", "note_change", "note_changeMany", "seek_to", "seek_toMany"] as const;

/** The `tl.so.tilo.lang.gallery.Gallery` kind at `path`. */
export function gallery(ws: tilo.Workspace, path: string): Gallery {
  return resourceProxy(ws, path, SURFACE, GALLERY_ACTIONS) as unknown as Gallery;
}

/** Row — `tl.so.tilo.lang.gallery.Row`. */
export interface Row extends tilo.Resource {
  od2pk6d3: string;  // title
  jqhagvfx: string;  // state
  pu34cefb: number;  // size
}

/** The actions `Row` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ROW_ACTIONS = [] as const;

/** The `tl.so.tilo.lang.gallery.Row` kind at `path`. */
export function row(ws: tilo.Workspace, path: string): Row {
  return resourceProxy(ws, path, SURFACE, ROW_ACTIONS) as unknown as Row;
}

/** Node — `tl.so.tilo.lang.gallery.Node`. */
export interface Node extends tilo.Resource {
  sartpsxo: string;  // title
  iqq34vjp: Node | null;  // parent
  jd5rkocy: boolean;  // folder
  "5dlddfkg": number;  // size
}

/** The actions `Node` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const NODE_ACTIONS = [] as const;

/** The `tl.so.tilo.lang.gallery.Node` kind at `path`. */
export function node(ws: tilo.Workspace, path: string): Node {
  return resourceProxy(ws, path, SURFACE, NODE_ACTIONS) as unknown as Node;
}

/** Message — `tl.so.tilo.lang.gallery.Message`. */
export interface Message extends tilo.Resource {
  lcfap66g: string;  // title
  ssylkjtu: string;  // from
  vyexrz4i: string;  // body
  thqsnyuw: string;  // at
  wavjxiov: boolean;  // mine
}

/** The actions `Message` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const MESSAGE_ACTIONS = [] as const;

/** The `tl.so.tilo.lang.gallery.Message` kind at `path`. */
export function message(ws: tilo.Workspace, path: string): Message {
  return resourceProxy(ws, path, SURFACE, MESSAGE_ACTIONS) as unknown as Message;
}

/** Card — `tl.so.tilo.lang.gallery.Card`. */
export interface Card extends tilo.Resource {
  y5gdkqew: string;  // title
  ii4dp5pl: number;  // count
}

/** The actions `Card` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CARD_ACTIONS = [] as const;

/** The `tl.so.tilo.lang.gallery.Card` kind at `path`. */
export function card(ws: tilo.Workspace, path: string): Card {
  return resourceProxy(ws, path, SURFACE, CARD_ACTIONS) as unknown as Card;
}

/** Change — `tl.so.tilo.lang.gallery.Change`. */
export interface Change extends tilo.Resource {
  dmejdwow: string;  // title
  kbnmru3y: string;  // summary
}

/** The actions `Change` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CHANGE_ACTIONS = [] as const;

/** The `tl.so.tilo.lang.gallery.Change` kind at `path`. */
export function change(ws: tilo.Workspace, path: string): Change {
  return resourceProxy(ws, path, SURFACE, CHANGE_ACTIONS) as unknown as Change;
}

/** Photo — `tl.so.tilo.lang.gallery.Photo`. */
export interface Photo extends tilo.Resource {
  "6zq4etzb": string;  // title
  s74gxrkc: string;  // place
  "5d3oxbry": string;  // picture
  lb2wi6nq: boolean;  // tall
}

/** The actions `Photo` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PHOTO_ACTIONS = [] as const;

/** The `tl.so.tilo.lang.gallery.Photo` kind at `path`. */
export function photo(ws: tilo.Workspace, path: string): Photo {
  return resourceProxy(ws, path, SURFACE, PHOTO_ACTIONS) as unknown as Photo;
}

/** Pinned — `tl.so.tilo.lang.gallery.Pinned`. */
export interface Pinned extends tilo.Resource {
  ciwf3kva: string;  // title
  "5tfi5mod": Kind;  // kind
}

/** The actions `Pinned` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PINNED_ACTIONS = [] as const;

/** The `tl.so.tilo.lang.gallery.Pinned` kind at `path`. */
export function pinned(ws: tilo.Workspace, path: string): Pinned {
  return resourceProxy(ws, path, SURFACE, PINNED_ACTIONS) as unknown as Pinned;
}

/** Gallery — `tl.so.tilo.lang.gallery`. */
export interface LangGallery extends tilo.Resource {
  /** Note the item a row opened, in the page's answer. */
  note_item(args: {
    last: string;
    x: Item;
  }): Promise<void>;
  /** Note the row the table picked, in the page's answer. */
  picked(args: {
    last: string;
    row: Row;
  }): Promise<void>;
  /** Note the row the table opened, in the page's answer. */
  opened(args: {
    last: string;
    row: Row;
  }): Promise<void>;
  /** Note the file the tree picked, in the page's answer. */
  picked_node(args: {
    last: string;
    node: Node;
  }): Promise<void>;
  /** Note the file the tree opened, in the page's answer. */
  opened_node(args: {
    last: string;
    node: Node;
  }): Promise<void>;
  /** Note the card the grid opened, in the page's answer. */
  opened_card(args: {
    last: string;
    card: Card;
  }): Promise<void>;
  /** Note the change the history took back, in the page's answer. */
  reverted(args: {
    last: string;
    change: Change;
  }): Promise<void>;
  /** Note the change the history opened, in the page's answer. */
  opened_change(args: {
    last: string;
    change: Change;
  }): Promise<void>;
  /** A page shown in a picture, from the first to the last. */
  picture_from(args: {
    first: number;
  }): Promise<void>;
  /** Note the page the rail chose, in the window's last answer. */
  mark_page(args: {
    last: string;
    index: number;
  }): Promise<void>;
  /** Note where a page moved to, in the window's last answer. */
  mark_move(args: {
    last: string;
    index: number;
    at: number;
  }): Promise<void>;
  /** Note the files dropped, in the window's last answer. */
  mark_files(args: {
    last: string;
    files: unknown[];
  }): Promise<void>;
  /** Note the picture a card opened, in the page's answer. */
  note_photo(args: {
    last: string;
    p: Photo;
  }): Promise<void>;
  /** Note the item moved, in the window's last answer. */
  mark_moved(args: {
    last: string;
    moved: Pinned;
    at: number;
  }): Promise<void>;
  /** Note what a drop target took, in the window's last answer. */
  dropped(args: {
    last: string;
    target: unknown;
  }): Promise<void>;
}

/** The actions `LangGallery` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LANGGALLERY_ACTIONS = ["note_item", "picked", "opened", "picked_node", "opened_node", "opened_card", "reverted", "opened_change", "picture_from", "mark_page", "mark_move", "mark_files", "note_photo", "mark_moved", "dropped"] as const;

/** The `tl.so.tilo.lang.gallery` kind at `path`. */
export function langGallery(ws: tilo.Workspace, path: string): LangGallery {
  return resourceProxy(ws, path, SURFACE, LANGGALLERY_ACTIONS) as unknown as LangGallery;
}

