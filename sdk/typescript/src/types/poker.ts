/**
 * Types for Poker (`so.tilo.poker`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.json`; write
 * it again rather than editing it.
 *
 * `Notice` is a `Resource` of the app's `tl.tilo.host.Notice`, made by its factory with the
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
export const SURFACE = "so.tilo.poker@819b4c26ac3cb9f9";

/** What a player did last in the hand. One of "none", "small_blind", "big_blind", "check", "call", "bet", "raise", "all_in", "fold". */
export type Move = "none" | "small_blind" | "big_blind" | "check" | "call" | "bet" | "raise" | "all_in" | "fold";

/** What a five-card hand is, weakest first: the order its worth compares in. One of "high_card", "pair", "two_pair", "trips", "straight", "flush", "full_house", "quads", "straight_flush". */
export type Category = "high_card" | "pair" | "two_pair" | "trips" | "straight" | "flush" | "full_house" | "quads" | "straight_flush";

/** Where a hand has got to. One of "idle", "preflop", "flop", "turn", "river", "done". */
export type Street = "idle" | "preflop" | "flop" | "turn" | "river" | "done";

/** The arguments of `sit`. */
export interface SitArgs {
  seat?: number;
}

/** The arguments of `leave`. */
export interface LeaveArgs {
}

/** The arguments of `deal`. */
export interface DealArgs {
  at?: string;
}

/** The arguments of `advance`. */
export interface AdvanceArgs {
}

/** The arguments of `close_bets`. */
export interface CloseBetsArgs {
}

/** The arguments of `settle_hand`. */
export interface SettleHandArgs {
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

/** The arguments of `all_in`. */
export interface AllInArgs {
}

/** The arguments of `show`. */
export interface ShowArgs {
}

/** The arguments of `skip`. */
export interface SkipArgs {
  at?: string;
}

/** The arguments of `rebuy`. */
export interface RebuyArgs {
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

/** InHand — `tl.so.tilo.poker.InHand`. */
export interface InHand extends tilo.Resource {
  lmmv5sj5: string;  // player
  "7qadgu4j": number;  // seat
  cstpy5cg: number;  // stack
  f2j35cui: number;  // bet
  "62xrdt5x": number;  // committed
  upvuk57p: number[];  // cards
  "4jhijusz": boolean;  // folded
  jt3hhmqs: boolean;  // acted
  zqefi644: boolean;  // shown
  bql52yxi: Move;  // last
}

/** The actions `InHand` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const INHAND_ACTIONS = [] as const;

/** The `tl.so.tilo.poker.InHand` kind at `path`. */
export function inHand(ws: tilo.Workspace, path: string): InHand {
  return resourceProxy(ws, path, SURFACE, INHAND_ACTIONS) as unknown as InHand;
}

/** Award — `tl.so.tilo.poker.Award`. */
export interface Award extends tilo.Resource {
  nu36auvm: string;  // player
  ym5jj4ap: number;  // amount
  jhe7cgkq: Category | null;  // category
}

/** The actions `Award` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const AWARD_ACTIONS = [] as const;

/** The `tl.so.tilo.poker.Award` kind at `path`. */
export function award(ws: tilo.Workspace, path: string): Award {
  return resourceProxy(ws, path, SURFACE, AWARD_ACTIONS) as unknown as Award;
}

/** Hand — `tl.so.tilo.poker.Hand`. */
export interface Hand extends tilo.Resource {
  eqwpzjbn: number;  // number
  "5s4ds2nn": Street;  // street
  "6wlyoas5": Array<tilo.Entry<InHand>>;  // players
  e5al5sgz: number;  // button
  iferrh2h: number[];  // deck
  "4wccktra": number;  // next
  f73z3brr: number[];  // board
  jvq7xgfb: number | null;  // to_act
  ycyrisz2: number;  // current_bet
  efpbnvul: number;  // min_raise
  z7i5z7cq: string | null;  // since
  "4l6pd2lu": Array<tilo.Entry<Award>>;  // awards
}

/** The actions `Hand` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const HAND_ACTIONS = [] as const;

/** The `tl.so.tilo.poker.Hand` kind at `path`. */
export function hand(ws: tilo.Workspace, path: string): Hand {
  return resourceProxy(ws, path, SURFACE, HAND_ACTIONS) as unknown as Hand;
}

/** Player — `tl.so.tilo.poker.Player`. */
export interface Player extends tilo.Resource {
  uq3kgvxc: string;  // person
  noqmxnuc: number;  // seat
  l3nts24i: number;  // chips
  "5d2doysh": string;  // joined
}

/** The actions `Player` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PLAYER_ACTIONS = [] as const;

/** The `tl.so.tilo.poker.Player` kind at `path`. */
export function player(ws: tilo.Workspace, path: string): Player {
  return resourceProxy(ws, path, SURFACE, PLAYER_ACTIONS) as unknown as Player;
}

/** Table — `tl.so.tilo.poker.Table`. */
export interface Table extends tilo.Resource {
  waqmzca5: string;  // title
  kbxb2nbt: Array<tilo.Entry<Player>>;  // players
  vemnbttj: Hand;  // hand
  a4rb7zva: number;  // button_seat
  /** Takes a seat at the table, with a buy-in of chips. */
  sit(args?: {
    seat?: number;
  }): Promise<void>;
  /** Runs `sit` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  sitMany(items: SitArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Leaves the table. */
  leave(): Promise<void>;
  /** Runs `leave` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  leaveMany(items: LeaveArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Deals the next hand: shuffles, moves the button and posts the blinds. */
  deal(args?: {
    at?: string;
  }): Promise<void>;
  /** Runs `deal` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  dealMany(items: DealArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Moves the hand on once its street's betting is done: the next player's turn, the next cards dealt, or the hand paid out. Every move calls it; a call of its own changes only what the table's rules have already decided, and refuses while a move is open. */
  advance(): Promise<void>;
  /** Runs `advance` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  advanceMany(items: AdvanceArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Puts every player's bet back to zero for the next street: what closing a street's betting leaves behind. */
  close_bets(): Promise<void>;
  /** Runs `close_bets` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  close_betsMany(items: CloseBetsArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Pays the pots and ends the hand. The rules of the table call it once the hand is decided; a call of its own pays what the showdown pays anyway and refuses while betting is open. */
  settle_hand(): Promise<void>;
  /** Runs `settle_hand` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  settle_handMany(items: SettleHandArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Folds your hand. */
  fold(): Promise<void>;
  /** Runs `fold` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  foldMany(items: FoldArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Checks when nobody has bet, or calls the bet you face. */
  call(): Promise<void>;
  /** Runs `call` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  callMany(items: CallArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Bets or raises to a total for this street. */
  raise(args: {
    to: number;
  }): Promise<void>;
  /** Runs `raise` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  raiseMany(items: RaiseArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Puts all your chips in. */
  all_in(): Promise<void>;
  /** Runs `all_in` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  all_inMany(items: AllInArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shows your cards to the table after the hand. */
  show(): Promise<void>;
  /** Runs `show` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  showMany(items: ShowArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Folds the hand of a player who has taken longer than their turn. */
  skip(args?: {
    at?: string;
  }): Promise<void>;
  /** Runs `skip` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  skipMany(items: SkipArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Buys back in once you have no chips. */
  rebuy(): Promise<void>;
  /** Runs `rebuy` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  rebuyMany(items: RebuyArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Table` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TABLE_ACTIONS = ["sit", "sitMany", "leave", "leaveMany", "deal", "dealMany", "advance", "advanceMany", "close_bets", "close_betsMany", "settle_hand", "settle_handMany", "fold", "foldMany", "call", "callMany", "raise", "raiseMany", "all_in", "all_inMany", "show", "showMany", "skip", "skipMany", "rebuy", "rebuyMany"] as const;

/** The `tl.so.tilo.poker.Table` kind at `path`. */
export function table(ws: tilo.Workspace, path: string): Table {
  return resourceProxy(ws, path, SURFACE, TABLE_ACTIONS) as unknown as Table;
}

/** Poker — `tl.so.tilo.poker`. */
export interface Poker extends tilo.Resource {
}

/** The actions `Poker` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const POKER_ACTIONS = [] as const;

/** The `tl.so.tilo.poker` kind at `path`. */
export function poker(ws: tilo.Workspace, path: string): Poker {
  return resourceProxy(ws, path, SURFACE, POKER_ACTIONS) as unknown as Poker;
}

