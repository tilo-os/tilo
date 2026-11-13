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
 * says where it is.
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

/** Poker table — `so.tilo.poker.table`. */
export interface Table extends tilo.Resource {
  players: Record<string, Player>;
  hand: Hand;
  button_seat: number;
  sit(args: {
    seat: number;
  }): Promise<tilo.Run | void>;
  leave(): Promise<tilo.Run | void>;
  deal(): Promise<tilo.Run | void>;
  fold(): Promise<tilo.Run | void>;
  call(): Promise<tilo.Run | void>;
  raise(args: {
    to: number;
  }): Promise<tilo.Run | void>;
  allIn(): Promise<tilo.Run | void>;
  show(): Promise<tilo.Run | void>;
  skip(): Promise<tilo.Run | void>;
  rebuy(): Promise<tilo.Run | void>;
}

/** The `so.tilo.poker.table` kind at `path`. */
export function table(ws: tilo.Workspace, path: string): Table {
  return resourceProxy(ws, path, SURFACE) as unknown as Table;
}

