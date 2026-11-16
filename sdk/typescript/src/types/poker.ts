/**
 * Types for Poker (`so.tilo.poker`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Table` is a `Resource` of the app's `so.tilo.poker.table`, made by its factory with the
 * resource's path (`poker/<resource>`): its attributes are the app's fields
 * (read-only ones `readonly`) and its methods are the app's actions, named in camel
 * case and taking the arguments the wire takes them under. An action that takes
 * longer answers a `Run`. A record is the object the wire carries, and an enum whose
 * cases carry fields is an object whose tag the app's schema names. An action whose
 * name is one the interface already has is run with `action("…")`, which the file
 * says where it is. An undoable action that answers nothing also has a `<action>Many`,
which applies it to every argument object of a list in a few all-or-nothing steps,
all of them one run.
 */

import { resourceProxy } from "tilo-sdk";
import type * as tilo from "tilo-sdk";

/** The version of the surface these types were made from: a workspace whose app
 * publishes another refuses their actions rather than run what they no longer
 * describe. */
export const SURFACE = "so.tilo.poker@ea2ab76e43cec872";

export interface Award {
  player: string;
  amount: number;
  category: string;
}

export interface Hand {
  number: number;
  street: Street;
  players: InHand[];
  button: number;
  deck: number[];
  board: number[];
  to_act: number | null;
  current_bet: number;
  min_raise: number;
  since_ms: number;
  awards: Award[];
}

export interface InHand {
  player: string;
  seat: number;
  stack: number;
  bet: number;
  committed: number;
  cards: number[];
  folded: boolean;
  acted: boolean;
  shown: boolean;
  last: Move;
}

/** One of "none", "small_blind", "big_blind", "check", "call", "bet", "raise", "all_in", "fold". */
export type Move = "none" | "small_blind" | "big_blind" | "check" | "call" | "bet" | "raise" | "all_in" | "fold";

export interface Player {
  seat: number;
  chips: number;
  joined_ms: number;
}

/** One of "idle", "preflop", "flop", "turn", "river", "done". */
export type Street = "idle" | "preflop" | "flop" | "turn" | "river" | "done";

/** The arguments of `sit`. */
export interface SitArgs {
  seat: number;
}

/** The arguments of `leave`. */
export interface LeaveArgs {
}

/** The arguments of `deal`. */
export interface DealArgs {
}

/** The arguments of `fold`. */
export interface FoldArgs {
}

/** The arguments of `call`. */
export interface CallArgs {
}

/** The arguments of `raise`. */
export interface RaiseArgs {
  to: number;
}

/** The arguments of `all-in`. */
export interface AllInArgs {
}

/** The arguments of `show`. */
export interface ShowArgs {
}

/** The arguments of `skip`. */
export interface SkipArgs {
}

/** The arguments of `rebuy`. */
export interface RebuyArgs {
}

/** Poker table — `so.tilo.poker.table`. */
export interface Table extends tilo.Resource {
  players: Record<string, Player>;
  hand: Hand;
  button_seat: number;
  sit(args: {
    seat: number;
  }): Promise<tilo.Run | void>;
  /** Runs `sit` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  sitMany(items: SitArgs[]): Promise<tilo.ActionAnswer[]>;
  leave(): Promise<tilo.Run | void>;
  /** Runs `leave` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  leaveMany(items: LeaveArgs[]): Promise<tilo.ActionAnswer[]>;
  deal(): Promise<tilo.Run | void>;
  /** Runs `deal` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  dealMany(items: DealArgs[]): Promise<tilo.ActionAnswer[]>;
  fold(): Promise<tilo.Run | void>;
  /** Runs `fold` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  foldMany(items: FoldArgs[]): Promise<tilo.ActionAnswer[]>;
  call(): Promise<tilo.Run | void>;
  /** Runs `call` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  callMany(items: CallArgs[]): Promise<tilo.ActionAnswer[]>;
  raise(args: {
    to: number;
  }): Promise<tilo.Run | void>;
  /** Runs `raise` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  raiseMany(items: RaiseArgs[]): Promise<tilo.ActionAnswer[]>;
  allIn(): Promise<tilo.Run | void>;
  /** Runs `all-in` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  allInMany(items: AllInArgs[]): Promise<tilo.ActionAnswer[]>;
  show(): Promise<tilo.Run | void>;
  /** Runs `show` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  showMany(items: ShowArgs[]): Promise<tilo.ActionAnswer[]>;
  skip(): Promise<tilo.Run | void>;
  /** Runs `skip` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  skipMany(items: SkipArgs[]): Promise<tilo.ActionAnswer[]>;
  rebuy(): Promise<tilo.Run | void>;
  /** Runs `rebuy` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  rebuyMany(items: RebuyArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The `so.tilo.poker.table` kind at `path`. */
export function table(ws: tilo.Workspace, path: string): Table {
  return resourceProxy(ws, path, SURFACE) as unknown as Table;
}

