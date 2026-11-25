/**
 * Types for Authenticator (`so.tilo.totp`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.json`; write
 * it again rather than editing it.
 *
 * `Notice` is a `Resource` of the app's `tl.tilo.host.Notice`, made by its factory with the
 * resource's path (`totp/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.totp@3e62ea2429cdfe14";

/** The arguments of `recolor_account`. */
export interface RecolorAccountArgs {
  color: number;
}

/** The arguments of `move_up`. */
export interface MoveUpArgs {
  a: Account;
}

/** The arguments of `move_down`. */
export interface MoveDownArgs {
  a: Account;
}

/** The arguments of `clear_notice`. */
export interface ClearNoticeArgs {
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

/** Account — `tl.so.tilo.totp.Account`. */
export interface Account extends tilo.Resource {
  wn7r7rgq: string;  // issuer
  k6loujfj: string;  // label
  sj2mtsnu: string;  // item
  "2f4aknif": number;  // color
  g3aq6san: string;  // tuned_at
  /** Renames an account's service: what a person changing the name it shows did. The codes' settings come from the form, which read them when it opened: an action never reads the vault, so what it keeps is what the window showed the person. */
  rename_account(args: {
    issuer: string;
    algorithm: string;
    digits: number;
    period: number;
  }): Promise<void>;
  /** Changes who an account signs in as: what a person changing its name did. Its codes' settings come from the form, as a rename's do. */
  relabel_account(args: {
    label: string;
    algorithm: string;
    digits: number;
    period: number;
  }): Promise<void>;
  /** Gives an account a colour of its own: what a person recolouring it did. */
  recolor_account(args: {
    color: number;
  }): Promise<void>;
  /** Runs `recolor_account` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  recolor_accountMany(items: RecolorAccountArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Changes how an account makes its codes, and its key with them: what a person changing the algorithm, the digits, the period or the key did. */
  recode_account(args: {
    algorithm: string;
    digits: number;
    period: number;
    secret?: string;
  }): Promise<void>;
  /** Changes what an account says, and its key and how its codes are made. A key left out keeps the one the vault holds: the service's key never leaves it, so changing an account's details can never show it. */
  update_account(args: {
    issuer: string;
    label?: string;
    secret?: string;
    algorithm?: string;
    digits?: number;
    period?: number;
  }): Promise<void>;
  /** Copies an account's current code to the clipboard, and says so: the person asked for it, and a window that shows nothing would leave them guessing whether it worked. */
  copy_code(): Promise<void>;
}

/** The actions `Account` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ACCOUNT_ACTIONS = ["rename_account", "relabel_account", "recolor_account", "recolor_accountMany", "recode_account", "update_account", "copy_code"] as const;

/** The `tl.so.tilo.totp.Account` kind at `path`. */
export function account(ws: tilo.Workspace, path: string): Account {
  return resourceProxy(ws, path, SURFACE, ACCOUNT_ACTIONS) as unknown as Account;
}

/** Vault — `tl.so.tilo.totp.Vault`. */
export interface Vault extends tilo.Resource {
  "4rdnjfde": Array<tilo.Entry<Account>>;  // accounts
  tcfimqtj: string;  // notice
  glvalakf: string;  // notice_kind
  djbrztzn: string;  // pending
  abeenkwo: string;  // pending_issuer
  t2vgrcpe: string;  // pending_label
  kqaty43e: string;  // pending_algorithm
  nlfjadyy: number;  // pending_digits
  ouj3ko22: number;  // pending_period
  /** Starts an account from what the person typed, or what a setup link said: what an agent, a script or the form asks. The vault keeps the seed; the account keeps the words. */
  add_account(args: {
    issuer: string;
    secret: string;
    label?: string;
    algorithm?: string;
    digits?: number;
    period?: number;
    color?: number;
  }): Promise<Account>;
  /** Adds the account a setup link names: what the QR scan and the paste field both end in. */
  import_uri(args: {
    uri: string;
  }): Promise<Account>;
  /** Reads the account a picture's QR code names, and keeps its key in the vault for the form to show: nothing is added until the person reads what the picture said and saves it. */
  scan_picture(args: {
    file: unknown;
  }): Promise<void>;
  /** Adds the account the picture read, with the key the vault already holds for it. */
  add_pending(): Promise<Account>;
  /** Lets the picture's account go: the key the vault kept for it is forgotten too. */
  drop_pending(): Promise<void>;
  /** Moves an account one place up the list. */
  move_up(args: {
    a: Account;
  }): Promise<void>;
  /** Runs `move_up` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  move_upMany(items: MoveUpArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Moves an account one place down the list. */
  move_down(args: {
    a: Account;
  }): Promise<void>;
  /** Runs `move_down` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  move_downMany(items: MoveDownArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Deletes an account and the key the vault kept for it. */
  delete_account(args: {
    a: Account;
  }): Promise<void>;
  refuse(args: {
    why: string;
  }): Promise<Account>;
  /** Saves the form: starts the account, or changes the one it was opened over, answering whether it saved. A refusal says why in the bar and leaves the form open with what was typed, so a window only leaves the form when this answered that it saved. */
  save_account(args: {
    issuer: string;
    label: string;
    secret: string;
    algorithm: string;
    digits: number;
    period: number;
    editing?: Account;
    color?: number;
    was_algorithm?: string;
    was_digits?: number;
    was_period?: number;
  }): Promise<boolean>;
  clear_notice(): Promise<void>;
  /** Runs `clear_notice` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  clear_noticeMany(items: ClearNoticeArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Vault` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const VAULT_ACTIONS = ["add_account", "import_uri", "scan_picture", "add_pending", "drop_pending", "move_up", "move_upMany", "move_down", "move_downMany", "delete_account", "refuse", "save_account", "clear_notice", "clear_noticeMany"] as const;

/** The `tl.so.tilo.totp.Vault` kind at `path`. */
export function vault(ws: tilo.Workspace, path: string): Vault {
  return resourceProxy(ws, path, SURFACE, VAULT_ACTIONS) as unknown as Vault;
}

/** ParseAsk — `tl.so.tilo.totp.ParseAsk`. */
export interface ParseAsk extends tilo.Resource {
  qhgdgyyn: string;  // uri
}

/** The actions `ParseAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PARSEASK_ACTIONS = [] as const;

/** The `tl.so.tilo.totp.ParseAsk` kind at `path`. */
export function parseAsk(ws: tilo.Workspace, path: string): ParseAsk {
  return resourceProxy(ws, path, SURFACE, PARSEASK_ACTIONS) as unknown as ParseAsk;
}

/** ParsedAnswer — `tl.so.tilo.totp.ParsedAnswer`. */
export interface ParsedAnswer extends tilo.Resource {
  x5elnfmo: string;  // issuer
  ac2s7eqm: string;  // label
  nsnnmxnd: string;  // secret
  "234azfht": string;  // algorithm
  cikeo42j: number;  // digits
  p5k5ujrm: number;  // period
}

/** The actions `ParsedAnswer` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PARSEDANSWER_ACTIONS = [] as const;

/** The `tl.so.tilo.totp.ParsedAnswer` kind at `path`. */
export function parsedAnswer(ws: tilo.Workspace, path: string): ParsedAnswer {
  return resourceProxy(ws, path, SURFACE, PARSEDANSWER_ACTIONS) as unknown as ParsedAnswer;
}

/** ScanAsk — `tl.so.tilo.totp.ScanAsk`. */
export interface ScanAsk extends tilo.Resource {
  j757xqu7: string;  // address
}

/** The actions `ScanAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SCANASK_ACTIONS = [] as const;

/** The `tl.so.tilo.totp.ScanAsk` kind at `path`. */
export function scanAsk(ws: tilo.Workspace, path: string): ScanAsk {
  return resourceProxy(ws, path, SURFACE, SCANASK_ACTIONS) as unknown as ScanAsk;
}

/** ScanAnswer — `tl.so.tilo.totp.ScanAnswer`. */
export interface ScanAnswer extends tilo.Resource {
  "4jvzelry": string;  // uri
}

/** The actions `ScanAnswer` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SCANANSWER_ACTIONS = [] as const;

/** The `tl.so.tilo.totp.ScanAnswer` kind at `path`. */
export function scanAnswer(ws: tilo.Workspace, path: string): ScanAnswer {
  return resourceProxy(ws, path, SURFACE, SCANANSWER_ACTIONS) as unknown as ScanAnswer;
}

/** CheckAsk — `tl.so.tilo.totp.CheckAsk`. */
export interface CheckAsk extends tilo.Resource {
  shi3ofgj: string;  // secret
}

/** The actions `CheckAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CHECKASK_ACTIONS = [] as const;

/** The `tl.so.tilo.totp.CheckAsk` kind at `path`. */
export function checkAsk(ws: tilo.Workspace, path: string): CheckAsk {
  return resourceProxy(ws, path, SURFACE, CHECKASK_ACTIONS) as unknown as CheckAsk;
}

/** CheckAnswer — `tl.so.tilo.totp.CheckAnswer`. */
export interface CheckAnswer extends tilo.Resource {
  pon234hn: boolean;  // ok
}

/** The actions `CheckAnswer` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CHECKANSWER_ACTIONS = [] as const;

/** The `tl.so.tilo.totp.CheckAnswer` kind at `path`. */
export function checkAnswer(ws: tilo.Workspace, path: string): CheckAnswer {
  return resourceProxy(ws, path, SURFACE, CHECKANSWER_ACTIONS) as unknown as CheckAnswer;
}

/** Authenticator — `tl.so.tilo.totp`. */
export interface Totp extends tilo.Resource {
}

/** The actions `Totp` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TOTP_ACTIONS = [] as const;

/** The `tl.so.tilo.totp` kind at `path`. */
export function totp(ws: tilo.Workspace, path: string): Totp {
  return resourceProxy(ws, path, SURFACE, TOTP_ACTIONS) as unknown as Totp;
}

