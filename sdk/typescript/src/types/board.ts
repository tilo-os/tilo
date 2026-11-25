/**
 * Types for Board (`so.tilo.lang.board`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.json`; write
 * it again rather than editing it.
 *
 * `Notice` is a `Resource` of the app's `tl.tilo.host.Notice`, made by its factory with the
 * resource's path (`board/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.lang.board@17be8937b9161eb2";

/** The arguments of `move_to`. */
export interface MoveToArgs {
  col: Column;
}

/** The arguments of `move_before`. */
export interface MoveBeforeArgs {
  card: Card;
  next: Card;
}

/** The arguments of `delete_card`. */
export interface DeleteCardArgs {
  card: Card;
}

/** The arguments of `rename_column`. */
export interface RenameColumnArgs {
  title: string;
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

/** Card — `tl.so.tilo.lang.board.Card`. */
export interface Card extends tilo.Resource {
  acqkumvl: string;  // title
  "6hfpfyn7": string;  // notes
  "4oqrwuyr": Column;  // column
  /** Moves a card to the foot of a column. */
  move_to(args: {
    col: Column;
  }): Promise<void>;
  /** Runs `move_to` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  move_toMany(items: MoveToArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Card` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CARD_ACTIONS = ["move_to", "move_toMany"] as const;

/** The `tl.so.tilo.lang.board.Card` kind at `path`. */
export function card(ws: tilo.Workspace, path: string): Card {
  return resourceProxy(ws, path, SURFACE, CARD_ACTIONS) as unknown as Card;
}

/** Column — `tl.so.tilo.lang.board.Column`. */
export interface Column extends tilo.Resource {
  "3vw7cjrp": string;  // title
  k24oebin: Array<tilo.Entry<Card>>;  // cards
  /** Adds a card at the foot of a column, under the title typed in the entry that adds there: a card is named where it is made, not left 'Untitled'. An entry with nothing but space in it makes no card: it never runs this. */
  new_card(args?: {
    title?: string;
  }): Promise<Card>;
  /** Moves a card to just above another, in that card's column. */
  move_before(args: {
    card: Card;
    next: Card;
  }): Promise<void>;
  /** Runs `move_before` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  move_beforeMany(items: MoveBeforeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Deletes a card. */
  delete_card(args: {
    card: Card;
  }): Promise<void>;
  /** Runs `delete_card` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  delete_cardMany(items: DeleteCardArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Renames a column. */
  rename_column(args: {
    title: string;
  }): Promise<void>;
  /** Runs `rename_column` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  rename_columnMany(items: RenameColumnArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Column` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const COLUMN_ACTIONS = ["new_card", "move_before", "move_beforeMany", "delete_card", "delete_cardMany", "rename_column", "rename_columnMany"] as const;

/** The `tl.so.tilo.lang.board.Column` kind at `path`. */
export function column(ws: tilo.Workspace, path: string): Column {
  return resourceProxy(ws, path, SURFACE, COLUMN_ACTIONS) as unknown as Column;
}

/** Board — `tl.so.tilo.lang.board.Board`. */
export interface Board extends tilo.Resource {
  hpfol33z: Array<tilo.Entry<Column>>;  // columns
  tuoyjxa6: Array<tilo.Entry<Card>>;  // cards
  /** Adds a column at the right of the board. */
  new_column(): Promise<Column>;
}

/** The actions `Board` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const BOARD_ACTIONS = ["new_column"] as const;

/** The `tl.so.tilo.lang.board.Board` kind at `path`. */
export function board(ws: tilo.Workspace, path: string): Board {
  return resourceProxy(ws, path, SURFACE, BOARD_ACTIONS) as unknown as Board;
}

/** Board — `tl.so.tilo.lang.board`. */
export interface LangBoard extends tilo.Resource {
}

/** The actions `LangBoard` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LANGBOARD_ACTIONS = [] as const;

/** The `tl.so.tilo.lang.board` kind at `path`. */
export function langBoard(ws: tilo.Workspace, path: string): LangBoard {
  return resourceProxy(ws, path, SURFACE, LANGBOARD_ACTIONS) as unknown as LangBoard;
}

