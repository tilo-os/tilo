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
export const SURFACE = "so.tilo.poker@846e8ee1be6c2b3b";

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

/** One of "None", "SmallBlind", "BigBlind", "Check", "Call", "Bet", "Raise", "AllIn", "Fold". */
export type Move = "None" | "SmallBlind" | "BigBlind" | "Check" | "Call" | "Bet" | "Raise" | "AllIn" | "Fold";

export interface Player {
  seat: number;
  chips: number;
  joined_ms: number;
}

/** One of "Idle", "Preflop", "Flop", "Turn", "River", "Done". */
export type Street = "Idle" | "Preflop" | "Flop" | "Turn" | "River" | "Done";

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
  }): Promise<void>;
  /** Runs `sit` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  sitMany(items: SitArgs[]): Promise<tilo.ActionAnswer[]>;
  leave(): Promise<void>;
  /** Runs `leave` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  leaveMany(items: LeaveArgs[]): Promise<tilo.ActionAnswer[]>;
  deal(): Promise<void>;
  /** Runs `deal` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  dealMany(items: DealArgs[]): Promise<tilo.ActionAnswer[]>;
  fold(): Promise<void>;
  /** Runs `fold` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  foldMany(items: FoldArgs[]): Promise<tilo.ActionAnswer[]>;
  call(): Promise<void>;
  /** Runs `call` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  callMany(items: CallArgs[]): Promise<tilo.ActionAnswer[]>;
  raise(args: {
    to: number;
  }): Promise<void>;
  /** Runs `raise` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  raiseMany(items: RaiseArgs[]): Promise<tilo.ActionAnswer[]>;
  allIn(): Promise<void>;
  /** Runs `all-in` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  allInMany(items: AllInArgs[]): Promise<tilo.ActionAnswer[]>;
  show(): Promise<void>;
  /** Runs `show` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  showMany(items: ShowArgs[]): Promise<tilo.ActionAnswer[]>;
  skip(): Promise<void>;
  /** Runs `skip` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  skipMany(items: SkipArgs[]): Promise<tilo.ActionAnswer[]>;
  rebuy(): Promise<void>;
  /** Runs `rebuy` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  rebuyMany(items: RebuyArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Table` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TABLE_ACTIONS = ["sit", "sitMany", "leave", "leaveMany", "deal", "dealMany", "fold", "foldMany", "call", "callMany", "raise", "raiseMany", "allIn", "allInMany", "show", "showMany", "skip", "skipMany", "rebuy", "rebuyMany"] as const;

/** The `so.tilo.poker.table` kind at `path`. */
export function table(ws: tilo.Workspace, path: string): Table {
  return resourceProxy(ws, path, SURFACE, TABLE_ACTIONS) as unknown as Table;
}

