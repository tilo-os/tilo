/**
 * Types for Passwords (`so.tilo.passwords`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.json`; write
 * it again rather than editing it.
 *
 * `Notice` is a `Resource` of the app's `tl.tilo.host.Notice`, made by its factory with the
 * resource's path (`passwords/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.passwords@30c76bd7d3a3aa8a";

/** What kind of secret an item is: what it is for, which sets the fields it has and the words that name them. One of "login", "api_key", "note", "wifi", "secret". */
export type Kind = "login" | "api_key" | "note" | "wifi" | "secret";

/** The arguments of `learn`. */
export interface LearnArgs {
  listed: unknown;
}

/** The arguments of `favorite`. */
export interface FavoriteArgs {
  favorite?: boolean;
}

/** The arguments of `preview_csv`. */
export interface PreviewCsvArgs {
  file: unknown;
}

/** The arguments of `clear_import`. */
export interface ClearImportArgs {
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

/** Item — `tl.so.tilo.passwords.Item`. */
export interface Item extends tilo.Resource {
  vvthph26: string;  // entry
  ildzcyo2: string;  // title
  ujkdowq4: Kind;  // kind
  qoa35hei: string[];  // tags
  tzeujwn7: boolean;  // favorite
  zpn4vzos: string[];  // places
  ksrrxwee: string[];  // labels
  fht5aqwz: boolean;  // noted
  hy77vohr: number;  // changed
  hzewwvyj: string[];  // concerns
  tpqopgvl: string;  // username
  txdg33mm: string;  // password
  fxvfkz3x: string;  // notes
  qgllbsrc: string[];  // origins
  japk242o: Record<string, string>;  // fields
  /** Writes what the vault says of an item (`listed`, one of what it lists) into its record, where it differs: its name, kind and tags, where it signs in, its fields' labels, whether it keeps a note, when it changed and what is wrong with it. */
  learn(args: {
    listed: unknown;
  }): Promise<void>;
  /** Runs `learn` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  learnMany(items: LearnArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Asks the vault to keep `ask` (its add, as JSON) and writes what it answers into `item`. Answers why it could not, or nothing. */
  keep(args: {
    ask: string;
  }): Promise<string>;
  /** Writes the item as it is, with `title`, `kind` and `places` in place of its own and `more` beside them. Answers why it could not, or nothing. */
  rewrite(args: {
    title: string;
    kind: Kind;
    places: string[];
    more: string[];
  }): Promise<string>;
  /** Writes who the item signs in as. */
  set_username(args: {
    username: string;
  }): Promise<string>;
  /** Changes the item's password, key or secret: the person types it on the system's card, and the vault keeps it. Nothing of it reaches this app's code, its records or history. */
  change_secret(): Promise<string>;
  /** Writes the item's note: typed on the system's card, kept in the vault. */
  write_note(): Promise<string>;
  /** Adds a field to the item, such as a PIN: its value typed on the system's card. */
  add_field(args?: {
    label?: string;
  }): Promise<string>;
  /** Takes a field off the item. */
  remove_field(args: {
    label: string;
  }): Promise<string>;
  /** Draws a strong password for the login where the vault is: 20 characters by default, with a capital, a small letter, a digit and a symbol. Nobody types or sees it. */
  generate_password(args?: {
    length?: number;
    capitals?: boolean;
    digits?: boolean;
    symbols?: boolean;
  }): Promise<string>;
  /** Puts an item in the person's favorites, or takes it out. */
  favorite(args?: {
    favorite?: boolean;
  }): Promise<void>;
  /** Runs `favorite` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  favoriteMany(items: FavoriteArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Gives an item a tag, as the words beside its tags are typed. */
  add_tag(args?: {
    tag?: string;
  }): Promise<string>;
  /** Takes a tag off an item. */
  remove_tag(args: {
    tag: string;
  }): Promise<string>;
  /** Gives an item an address it signs in to, checked as a web address, as typed beside them. */
  add_origin(args?: {
    origin?: string;
  }): Promise<string>;
  /** Takes an address off an item. */
  remove_origin(args: {
    origin: string;
  }): Promise<string>;
  /** Renames an item, in its record and in the vault. */
  rename(args: {
    title: string;
  }): Promise<string>;
  /** Changes what kind of secret an item is. */
  set_kind(args: {
    kind: Kind;
  }): Promise<string>;
  /** Puts the item's secret on the person's clipboard from the vault, which forgets it again after a while. It never passes through Passwords, and the vault records the copy as a use. */
  copy_secret(): Promise<string>;
  /** Puts what identifies the person to the account on their clipboard from the vault: a login's username, an API key's account. */
  copy_identity(): Promise<string>;
  /** Asks the vault to put `part` of the item on the person's clipboard. Answers why it could not, or nothing. */
  copy_part(args: {
    part: string;
  }): Promise<string>;
  /** Copies the addresses the item signs in to, one a line. */
  copy_origins(): Promise<void>;
  /** Asks the vault to show `part` of the item for a while, where `how` says (`show` for the window, `hold` for the host): the name it is shown under, or nothing where the item keeps none. The vault records each showing as a use. */
  shown_part(args: {
    part: string;
    how: string;
  }): Promise<string>;
  /** Shows the item's secret, who it signs in as, its note and its fields in the window, for 30 seconds at most: each part by a name only the window reads it under. */
  reveal(): Promise<Shown>;
  /** Reads who used the item, and the leaves its person gave: what the item's access page shows. */
  access(): Promise<Access>;
  /** Takes back a leave the item's person gave: the agent or app it names asks again next time. */
  take_back(args: {
    leave: Leave;
  }): Promise<Access>;
}

/** The actions `Item` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ITEM_ACTIONS = ["learn", "learnMany", "keep", "rewrite", "set_username", "change_secret", "write_note", "add_field", "remove_field", "generate_password", "favorite", "favoriteMany", "add_tag", "remove_tag", "add_origin", "remove_origin", "rename", "set_kind", "copy_secret", "copy_identity", "copy_part", "copy_origins", "shown_part", "reveal", "access", "take_back"] as const;

/** The `tl.so.tilo.passwords.Item` kind at `path`. */
export function item(ws: tilo.Workspace, path: string): Item {
  return resourceProxy(ws, path, SURFACE, ITEM_ACTIONS) as unknown as Item;
}

/** ImportRow — `tl.so.tilo.passwords.ImportRow`. */
export interface ImportRow extends tilo.Resource {
  l7jn4eol: string;  // title
  s6pvezan: string;  // username
  "4jcknnmq": string;  // origin
  "23s6djq3": boolean;  // skip
}

/** The actions `ImportRow` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const IMPORTROW_ACTIONS = [] as const;

/** The `tl.so.tilo.passwords.ImportRow` kind at `path`. */
export function importRow(ws: tilo.Workspace, path: string): ImportRow {
  return resourceProxy(ws, path, SURFACE, IMPORTROW_ACTIONS) as unknown as ImportRow;
}

/** Vault — `tl.so.tilo.passwords.Vault`. */
export interface Vault extends tilo.Resource {
  vflzrccj: Array<tilo.Entry<Item>>;  // items
  "6n7bahst": string[];  // converted
  wa4uxojt: Array<tilo.Entry<ImportRow>>;  // pending
  fatb5kmf: string;  // importing
  "3xf6uda7": string;  // notice
  /** Adds a new item to the vault from `ask` (its add, as JSON) and lists it: the item, or nothing where the vault did not keep it, with why in the vault's notice. */
  add_new(args: {
    ask: string;
  }): Promise<Item | null>;
  /** Starts a login: an item of the vault named by what is typed first, with a strong password drawn where the vault is, which nobody types or sees. The person changes it on the item. */
  new_item(args?: {
    title?: string;
  }): Promise<Item | null>;
  /** Adds a login in one step: what an agent, a script or the VM's own end-to-end check asks of the vault. The person types its password on the system's card, so whoever asked never holds it, and the vault keeps it. An address is checked as a web address. */
  save_login(args: {
    name: string;
    origin: string;
    username: string;
  }): Promise<Item | null>;
  /** The vault's logins, by name and by the item that holds each: what a script, an agent or another device's window lists. The passwords are never part of it. */
  list_logins(): Promise<LoginList>;
  /** Deletes an item from the vault. */
  delete(args: {
    item: Item;
  }): Promise<void>;
  /** Brings the vault and its items together: an item the vault keeps that is not listed (a mail account's password, a connector's token, one another device added) gets a record, what the vault says of each listed one reaches its record, one the vault no longer keeps leaves the list, and what an older Passwords kept in its own records moves into the vault and leaves the record. Runs while Passwords is open, and changes nothing when all is settled. */
  settle(): Promise<void>;
  /** Moves what an item's record keeps into the vault, once, and empties the record. A record that names an item the vault keeps changes that item; one that names none adds one. */
  carry(args: {
    item: Item;
  }): Promise<void>;
  /** Reads the browser's CSV the person chose into the vault's waiting rows, each marked when the vault already holds that login. The passwords stay in the file. */
  preview_csv(args: {
    file: unknown;
  }): Promise<void>;
  /** Runs `preview_csv` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  preview_csvMany(items: PreviewCsvArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Adds an item the host read from a file the person chose: `item`, its secrets each named by the token the host holds it under. Answers whether the vault kept it. */
  add_read(args: {
    item: HeldItem;
  }): Promise<boolean>;
  /** Brings the rows the person agreed to into the vault, skipping the ones the vault holds. The host reads their passwords from the file and the vault takes them from the host. */
  apply_import(): Promise<void>;
  /** Puts the waiting rows away. */
  clear_import(): Promise<void>;
  /** Runs `clear_import` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  clear_importMany(items: ClearImportArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Empties the vault's waiting rows. */
  clear_pending(): Promise<boolean>;
  /** Every listed item with its secrets, each named by the token the host holds it under: what the host writes into a file the person chose. The vault records each part handed over as a use. */
  held_items(): Promise<HeldItem[]>;
  /** The vault as a browser's CSV: the person chooses where it goes. */
  export_csv(): Promise<unknown>;
  /** The vault as an encrypted backup under the password the person types twice on the system's card: the person chooses where it goes. */
  export_backup(): Promise<unknown>;
  /** Brings the entries a backup holds into the vault with the password the person types on the system's card, and answers how many came in. An item the vault already lists under its name and address is left as it is, so restoring twice adds nothing. */
  restore_backup(args: {
    file: unknown;
  }): Promise<number>;
  /** Takes into the vault what this device's secret service holds and a workspace may share. The device's authentication material and its Wi-Fi networks stay where they are, and each entry leaves the device's service once the vault keeps it. */
  absorb_device(): Promise<void>;
}

/** The actions `Vault` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const VAULT_ACTIONS = ["add_new", "new_item", "save_login", "list_logins", "delete", "settle", "carry", "preview_csv", "preview_csvMany", "add_read", "apply_import", "clear_import", "clear_importMany", "clear_pending", "held_items", "export_csv", "export_backup", "restore_backup", "absorb_device"] as const;

/** The `tl.so.tilo.passwords.Vault` kind at `path`. */
export function vault(ws: tilo.Workspace, path: string): Vault {
  return resourceProxy(ws, path, SURFACE, VAULT_ACTIONS) as unknown as Vault;
}

/** LoginList — `tl.so.tilo.passwords.LoginList`. */
export interface LoginList extends tilo.Resource {
  nkbtq6y2: LoginEntry[];  // logins
}

/** The actions `LoginList` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LOGINLIST_ACTIONS = [] as const;

/** The `tl.so.tilo.passwords.LoginList` kind at `path`. */
export function loginList(ws: tilo.Workspace, path: string): LoginList {
  return resourceProxy(ws, path, SURFACE, LOGINLIST_ACTIONS) as unknown as LoginList;
}

/** LoginEntry — `tl.so.tilo.passwords.LoginEntry`. */
export interface LoginEntry extends tilo.Resource {
  mtdotryg: string;  // item
  yiseqxtu: string;  // name
}

/** The actions `LoginEntry` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LOGINENTRY_ACTIONS = [] as const;

/** The `tl.so.tilo.passwords.LoginEntry` kind at `path`. */
export function loginEntry(ws: tilo.Workspace, path: string): LoginEntry {
  return resourceProxy(ws, path, SURFACE, LOGINENTRY_ACTIONS) as unknown as LoginEntry;
}

/** Shown — `tl.so.tilo.passwords.Shown`. */
export interface Shown extends tilo.Resource {
  "4ggtifo6": string;  // secret
  k2igcsy4: string;  // identity
  kz5emarz: string;  // notes
  bs66mwlz: ShownField[];  // fields
}

/** The actions `Shown` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SHOWN_ACTIONS = [] as const;

/** The `tl.so.tilo.passwords.Shown` kind at `path`. */
export function shown(ws: tilo.Workspace, path: string): Shown {
  return resourceProxy(ws, path, SURFACE, SHOWN_ACTIONS) as unknown as Shown;
}

/** ShownField — `tl.so.tilo.passwords.ShownField`. */
export interface ShownField extends tilo.Resource {
  "47ctn52x": string;  // label
  "72h5hcgw": string;  // value
}

/** The actions `ShownField` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SHOWNFIELD_ACTIONS = [] as const;

/** The `tl.so.tilo.passwords.ShownField` kind at `path`. */
export function shownField(ws: tilo.Workspace, path: string): ShownField {
  return resourceProxy(ws, path, SURFACE, SHOWNFIELD_ACTIONS) as unknown as ShownField;
}

/** Access — `tl.so.tilo.passwords.Access`. */
export interface Access extends tilo.Resource {
  upwe4jla: Use[];  // uses
  n52kczfg: Leave[];  // leaves
  ll3dunru: string;  // trouble
}

/** The actions `Access` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ACCESS_ACTIONS = [] as const;

/** The `tl.so.tilo.passwords.Access` kind at `path`. */
export function access(ws: tilo.Workspace, path: string): Access {
  return resourceProxy(ws, path, SURFACE, ACCESS_ACTIONS) as unknown as Access;
}

/** Use — `tl.so.tilo.passwords.Use`. */
export interface Use extends tilo.Resource {
  dhbq7ocx: number;  // at
  ldvya4gh: string;  // by
  xsu4yq6o: string;  // what
  ccefrfoc: string;  // places
}

/** The actions `Use` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const USE_ACTIONS = [] as const;

/** The `tl.so.tilo.passwords.Use` kind at `path`. */
export function use(ws: tilo.Workspace, path: string): Use {
  return resourceProxy(ws, path, SURFACE, USE_ACTIONS) as unknown as Use;
}

/** Leave — `tl.so.tilo.passwords.Leave`. */
export interface Leave extends tilo.Resource {
  "2emebh4y": string;  // agent
  crfod54g: string;  // for_app
  eqysm75s: string;  // by
  rgwbpnf4: string;  // using
  "6ovlcphf": number;  // since
}

/** The actions `Leave` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LEAVE_ACTIONS = [] as const;

/** The `tl.so.tilo.passwords.Leave` kind at `path`. */
export function leave(ws: tilo.Workspace, path: string): Leave {
  return resourceProxy(ws, path, SURFACE, LEAVE_ACTIONS) as unknown as Leave;
}

/** TypedAsk — `tl.so.tilo.passwords.TypedAsk`. */
export interface TypedAsk extends tilo.Resource {
  upwiyhzz: number;  // place
}

/** The actions `TypedAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TYPEDASK_ACTIONS = [] as const;

/** The `tl.so.tilo.passwords.TypedAsk` kind at `path`. */
export function typedAsk(ws: tilo.Workspace, path: string): TypedAsk {
  return resourceProxy(ws, path, SURFACE, TYPEDASK_ACTIONS) as unknown as TypedAsk;
}

/** TokenAnswer — `tl.so.tilo.passwords.TokenAnswer`. */
export interface TokenAnswer extends tilo.Resource {
  "5t7dz6wt": string;  // token
}

/** The actions `TokenAnswer` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TOKENANSWER_ACTIONS = [] as const;

/** The `tl.so.tilo.passwords.TokenAnswer` kind at `path`. */
export function tokenAnswer(ws: tilo.Workspace, path: string): TokenAnswer {
  return resourceProxy(ws, path, SURFACE, TOKENANSWER_ACTIONS) as unknown as TokenAnswer;
}

/** AskedFile — `tl.so.tilo.passwords.AskedFile`. */
export interface AskedFile extends tilo.Resource {
  roquvgtj: string;  // url
}

/** The actions `AskedFile` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ASKEDFILE_ACTIONS = [] as const;

/** The `tl.so.tilo.passwords.AskedFile` kind at `path`. */
export function askedFile(ws: tilo.Workspace, path: string): AskedFile {
  return resourceProxy(ws, path, SURFACE, ASKEDFILE_ACTIONS) as unknown as AskedFile;
}

/** ImportAsk — `tl.so.tilo.passwords.ImportAsk`. */
export interface ImportAsk extends tilo.Resource {
  trzw7c4z: string;  // url
  xbct2xaa: number[];  // skip
}

/** The actions `ImportAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const IMPORTASK_ACTIONS = [] as const;

/** The `tl.so.tilo.passwords.ImportAsk` kind at `path`. */
export function importAsk(ws: tilo.Workspace, path: string): ImportAsk {
  return resourceProxy(ws, path, SURFACE, IMPORTASK_ACTIONS) as unknown as ImportAsk;
}

/** CsvRow — `tl.so.tilo.passwords.CsvRow`. */
export interface CsvRow extends tilo.Resource {
  ufq2o5qa: string;  // title
  pjl3fx3t: string;  // username
  "7ca4ejcb": string;  // origin
}

/** The actions `CsvRow` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CSVROW_ACTIONS = [] as const;

/** The `tl.so.tilo.passwords.CsvRow` kind at `path`. */
export function csvRow(ws: tilo.Workspace, path: string): CsvRow {
  return resourceProxy(ws, path, SURFACE, CSVROW_ACTIONS) as unknown as CsvRow;
}

/** RowsAnswer — `tl.so.tilo.passwords.RowsAnswer`. */
export interface RowsAnswer extends tilo.Resource {
  janzxbln: CsvRow[];  // rows
}

/** The actions `RowsAnswer` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ROWSANSWER_ACTIONS = [] as const;

/** The `tl.so.tilo.passwords.RowsAnswer` kind at `path`. */
export function rowsAnswer(ws: tilo.Workspace, path: string): RowsAnswer {
  return resourceProxy(ws, path, SURFACE, ROWSANSWER_ACTIONS) as unknown as RowsAnswer;
}

/** HeldItem — `tl.so.tilo.passwords.HeldItem`. */
export interface HeldItem extends tilo.Resource {
  "54afy6gc": string;  // id
  jtfqafog: string;  // kind
  qct2yfix: string;  // title
  pjgoev5i: string;  // username
  vqux3uy5: string[];  // origins
  pngdxwz4: string[];  // tags
  "3zfbij5u": string;  // password
  k2xr3in6: string;  // notes
  s5jm3xad: HeldField[];  // fields
}

/** The actions `HeldItem` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const HELDITEM_ACTIONS = [] as const;

/** The `tl.so.tilo.passwords.HeldItem` kind at `path`. */
export function heldItem(ws: tilo.Workspace, path: string): HeldItem {
  return resourceProxy(ws, path, SURFACE, HELDITEM_ACTIONS) as unknown as HeldItem;
}

/** HeldField — `tl.so.tilo.passwords.HeldField`. */
export interface HeldField extends tilo.Resource {
  b7ymmw3n: string;  // label
  wgkjnofz: string;  // value
}

/** The actions `HeldField` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const HELDFIELD_ACTIONS = [] as const;

/** The `tl.so.tilo.passwords.HeldField` kind at `path`. */
export function heldField(ws: tilo.Workspace, path: string): HeldField {
  return resourceProxy(ws, path, SURFACE, HELDFIELD_ACTIONS) as unknown as HeldField;
}

/** HeldItems — `tl.so.tilo.passwords.HeldItems`. */
export interface HeldItems extends tilo.Resource {
  awioulwr: HeldItem[];  // items
}

/** The actions `HeldItems` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const HELDITEMS_ACTIONS = [] as const;

/** The `tl.so.tilo.passwords.HeldItems` kind at `path`. */
export function heldItems(ws: tilo.Workspace, path: string): HeldItems {
  return resourceProxy(ws, path, SURFACE, HELDITEMS_ACTIONS) as unknown as HeldItems;
}

/** DeviceItems — `tl.so.tilo.passwords.DeviceItems`. */
export interface DeviceItems extends tilo.Resource {
  ghk43anz: HeldItem[];  // items
  lzu63qlu: string;  // error
}

/** The actions `DeviceItems` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DEVICEITEMS_ACTIONS = [] as const;

/** The `tl.so.tilo.passwords.DeviceItems` kind at `path`. */
export function deviceItems(ws: tilo.Workspace, path: string): DeviceItems {
  return resourceProxy(ws, path, SURFACE, DEVICEITEMS_ACTIONS) as unknown as DeviceItems;
}

/** DeviceRemove — `tl.so.tilo.passwords.DeviceRemove`. */
export interface DeviceRemove extends tilo.Resource {
  tzeqieyr: string[];  // ids
}

/** The actions `DeviceRemove` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DEVICEREMOVE_ACTIONS = [] as const;

/** The `tl.so.tilo.passwords.DeviceRemove` kind at `path`. */
export function deviceRemove(ws: tilo.Workspace, path: string): DeviceRemove {
  return resourceProxy(ws, path, SURFACE, DEVICEREMOVE_ACTIONS) as unknown as DeviceRemove;
}

/** ToFile — `tl.so.tilo.passwords.ToFile`. */
export interface ToFile extends tilo.Resource {
  "7eqpvzfc": HeldItem[];  // items
  kfrmuqfw: string;  // passphrase
}

/** The actions `ToFile` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TOFILE_ACTIONS = [] as const;

/** The `tl.so.tilo.passwords.ToFile` kind at `path`. */
export function toFile(ws: tilo.Workspace, path: string): ToFile {
  return resourceProxy(ws, path, SURFACE, TOFILE_ACTIONS) as unknown as ToFile;
}

/** UrlAnswer — `tl.so.tilo.passwords.UrlAnswer`. */
export interface UrlAnswer extends tilo.Resource {
  vlz4ie4r: string;  // url
}

/** The actions `UrlAnswer` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const URLANSWER_ACTIONS = [] as const;

/** The `tl.so.tilo.passwords.UrlAnswer` kind at `path`. */
export function urlAnswer(ws: tilo.Workspace, path: string): UrlAnswer {
  return resourceProxy(ws, path, SURFACE, URLANSWER_ACTIONS) as unknown as UrlAnswer;
}

/** Passwords — `tl.so.tilo.passwords`. */
export interface Passwords extends tilo.Resource {
}

/** The actions `Passwords` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PASSWORDS_ACTIONS = [] as const;

/** The `tl.so.tilo.passwords` kind at `path`. */
export function passwords(ws: tilo.Workspace, path: string): Passwords {
  return resourceProxy(ws, path, SURFACE, PASSWORDS_ACTIONS) as unknown as Passwords;
}

