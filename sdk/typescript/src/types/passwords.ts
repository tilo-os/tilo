/**
 * Types for Passwords (`so.tilo.passwords`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
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
export const SURFACE = "so.tilo.passwords@df338c74eaa45f8d";

/** What kind of secret an item is: what it is for, which sets the fields it has and the words that name them. One of "login", "api_key", "note", "wifi", "secret". */
export type Kind = "login" | "api_key" | "note" | "wifi" | "secret";

/** The arguments of `keep_facts`. */
export interface KeepFactsArgs {
  username: string;
  origins: string[];
}

/** The arguments of `set_username`. */
export interface SetUsernameArgs {
  username: string;
}

/** The arguments of `generate_password`. */
export interface GeneratePasswordArgs {
  length?: number;
  capitals?: boolean;
  digits?: boolean;
  symbols?: boolean;
}

/** The arguments of `favorite`. */
export interface FavoriteArgs {
  favorite?: boolean;
}

/** The arguments of `add_tag`. */
export interface AddTagArgs {
  tag?: string;
}

/** The arguments of `remove_tag`. */
export interface RemoveTagArgs {
  tag: string;
}

/** The arguments of `add_origin`. */
export interface AddOriginArgs {
  origin?: string;
}

/** The arguments of `remove_origin`. */
export interface RemoveOriginArgs {
  origin: string;
}

/** The arguments of `rename`. */
export interface RenameArgs {
  title: string;
}

/** The arguments of `set_kind`. */
export interface SetKindArgs {
  kind: Kind;
}

/** The arguments of `carry`. */
export interface CarryArgs {
}

/** The arguments of `delete`. */
export interface DeleteArgs {
  item: Item;
}

/** The arguments of `settle`. */
export interface SettleArgs {
}

/** The arguments of `preview_csv`. */
export interface PreviewCsvArgs {
  file: unknown;
}

/** The arguments of `apply_import`. */
export interface ApplyImportArgs {
}

/** The arguments of `clear_import`. */
export interface ClearImportArgs {
}

/** The arguments of `absorb_device`. */
export interface AbsorbDeviceArgs {
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
  tpqopgvl: string;  // username
  txdg33mm: string;  // password
  fxvfkz3x: string;  // notes
  qgllbsrc: string[];  // origins
  japk242o: Record<string, string>;  // fields
  /** Writes what is safe to say of an item into its vault entry: who it signs in as and where, with the item's name, kind and tags. */
  keep_facts(args: {
    username: string;
    origins: string[];
  }): Promise<void>;
  /** Runs `keep_facts` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  keep_factsMany(items: KeepFactsArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Writes who the item signs in as. */
  set_username(args: {
    username: string;
  }): Promise<void>;
  /** Runs `set_username` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_usernameMany(items: SetUsernameArgs[]): Promise<tilo.ActionAnswer[]>;
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
  /** Draws a strong password for the item where the vault is: 20 characters by default, with a capital, a small letter, a digit and a symbol. Nobody types or sees it. */
  generate_password(args?: {
    length?: number;
    capitals?: boolean;
    digits?: boolean;
    symbols?: boolean;
  }): Promise<void>;
  /** Runs `generate_password` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  generate_passwordMany(items: GeneratePasswordArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Puts an item in the person's favorites, or takes it out. */
  favorite(args?: {
    favorite?: boolean;
  }): Promise<void>;
  /** Runs `favorite` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  favoriteMany(items: FavoriteArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Gives an item a tag, as the words beside its tags are typed. */
  add_tag(args?: {
    tag?: string;
  }): Promise<void>;
  /** Runs `add_tag` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  add_tagMany(items: AddTagArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes a tag off an item. */
  remove_tag(args: {
    tag: string;
  }): Promise<void>;
  /** Runs `remove_tag` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remove_tagMany(items: RemoveTagArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Gives an item an address it signs in to, checked as a web address, as typed beside them. */
  add_origin(args?: {
    origin?: string;
  }): Promise<void>;
  /** Runs `add_origin` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  add_originMany(items: AddOriginArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes an address off an item. */
  remove_origin(args: {
    origin: string;
  }): Promise<void>;
  /** Runs `remove_origin` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remove_originMany(items: RemoveOriginArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Renames an item, in its record and its vault entry. */
  rename(args: {
    title: string;
  }): Promise<void>;
  /** Runs `rename` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  renameMany(items: RenameArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Changes what kind of secret an item is. */
  set_kind(args: {
    kind: Kind;
  }): Promise<void>;
  /** Runs `set_kind` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_kindMany(items: SetKindArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Copies the item's secret to the person's clipboard, which forgets it again after a while. Copying reads the vault and keeps nothing. */
  copy_secret(): Promise<void>;
  /** Copies what identifies the person to the account: a login's username, an API key's account, a Wi-Fi network's name. */
  copy_identity(): Promise<void>;
  /** Copies the addresses the item signs in to, one a line. */
  copy_origins(): Promise<void>;
  /** Moves what an item's record keeps into the vault, once, and empties the record: an entry the vault already keeps under the item's id stays as it is. */
  carry(): Promise<void>;
  /** Runs `carry` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  carryMany(items: CarryArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Item` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ITEM_ACTIONS = ["keep_facts", "keep_factsMany", "set_username", "set_usernameMany", "change_secret", "write_note", "add_field", "remove_field", "generate_password", "generate_passwordMany", "favorite", "favoriteMany", "add_tag", "add_tagMany", "remove_tag", "remove_tagMany", "add_origin", "add_originMany", "remove_origin", "remove_originMany", "rename", "renameMany", "set_kind", "set_kindMany", "copy_secret", "copy_identity", "copy_origins", "carry", "carryMany"] as const;

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
  /** Starts an item: an entry of the vault and the item that lists it, named by what is typed first. Its secret is typed on the system's card, never here. */
  new_item(args?: {
    title?: string;
  }): Promise<Item>;
  /** Adds a login in one step: what an agent, a script or the VM's own end-to-end check asks of the vault. The person types its password on the system's card, so whoever asked never holds it, and the vault keeps it. An address is checked as a web address. */
  save_login(args: {
    name: string;
    origin: string;
    username: string;
  }): Promise<Item>;
  /** The vault's logins, by name and by the entry that holds each: what a script, an agent or another device's window lists. The passwords are never part of it. */
  list_logins(): Promise<LoginList>;
  /** Deletes an item, and the entry it stands for, from the vault. */
  delete(args: {
    item: Item;
  }): Promise<void>;
  /** Runs `delete` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deleteMany(items: DeleteArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Brings the vault and its items together: an entry kept elsewhere (a mail account's password, a connector's token, one another device added) gets an item, an item's name, kind and tags reach its entry, and what an older Passwords kept in its own records moves into the vault and leaves the record. Runs while Passwords is open, and changes nothing when all is settled. */
  settle(): Promise<void>;
  /** Runs `settle` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  settleMany(items: SettleArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Reads the browser's CSV the person chose into the vault's waiting rows, each marked when the vault already holds that login. The passwords stay in the file. */
  preview_csv(args: {
    file: unknown;
  }): Promise<void>;
  /** Runs `preview_csv` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  preview_csvMany(items: PreviewCsvArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Brings the rows the person agreed to into the vault, skipping the ones the vault holds. The vault reads their passwords from the file. */
  apply_import(): Promise<void>;
  /** Runs `apply_import` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  apply_importMany(items: ApplyImportArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Puts the waiting rows away. */
  clear_import(): Promise<void>;
  /** Runs `clear_import` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  clear_importMany(items: ClearImportArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Empties the vault's waiting rows. */
  clear_pending(): Promise<boolean>;
  /** The vault as a browser's CSV: the person chooses where it goes. */
  export_csv(): Promise<unknown>;
  /** The vault as an encrypted backup under the password the person types twice on the system's card: the person chooses where it goes. */
  export_backup(): Promise<unknown>;
  /** Brings the entries a backup holds into the vault with the password the person types on the system's card, and answers how many came in. An entry the vault already keeps is left as it is, so restoring twice adds nothing. */
  restore_backup(args: {
    file: unknown;
  }): Promise<number>;
  /** Takes into the vault what this device's secret service holds and a workspace may share. The device's authentication material and its Wi-Fi networks stay where they are, and each entry leaves the device's service once the vault keeps it. */
  absorb_device(): Promise<void>;
  /** Runs `absorb_device` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  absorb_deviceMany(items: AbsorbDeviceArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Vault` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const VAULT_ACTIONS = ["new_item", "save_login", "list_logins", "delete", "deleteMany", "settle", "settleMany", "preview_csv", "preview_csvMany", "apply_import", "apply_importMany", "clear_import", "clear_importMany", "clear_pending", "export_csv", "export_backup", "restore_backup", "absorb_device", "absorb_deviceMany"] as const;

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

/** TypedAsk — `tl.so.tilo.passwords.TypedAsk`. */
export interface TypedAsk extends tilo.Resource {
  "5se7svg2": string;  // id
  "3f2i24zq": string;  // part
}

/** The actions `TypedAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TYPEDASK_ACTIONS = [] as const;

/** The `tl.so.tilo.passwords.TypedAsk` kind at `path`. */
export function typedAsk(ws: tilo.Workspace, path: string): TypedAsk {
  return resourceProxy(ws, path, SURFACE, TYPEDASK_ACTIONS) as unknown as TypedAsk;
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

/** CountAnswer — `tl.so.tilo.passwords.CountAnswer`. */
export interface CountAnswer extends tilo.Resource {
  "35klc2sc": number;  // imported
}

/** The actions `CountAnswer` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const COUNTANSWER_ACTIONS = [] as const;

/** The `tl.so.tilo.passwords.CountAnswer` kind at `path`. */
export function countAnswer(ws: tilo.Workspace, path: string): CountAnswer {
  return resourceProxy(ws, path, SURFACE, COUNTANSWER_ACTIONS) as unknown as CountAnswer;
}

/** RestoredAnswer — `tl.so.tilo.passwords.RestoredAnswer`. */
export interface RestoredAnswer extends tilo.Resource {
  ks2ix353: number;  // restored
}

/** The actions `RestoredAnswer` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const RESTOREDANSWER_ACTIONS = [] as const;

/** The `tl.so.tilo.passwords.RestoredAnswer` kind at `path`. */
export function restoredAnswer(ws: tilo.Workspace, path: string): RestoredAnswer {
  return resourceProxy(ws, path, SURFACE, RESTOREDANSWER_ACTIONS) as unknown as RestoredAnswer;
}

/** CarriedAnswer — `tl.so.tilo.passwords.CarriedAnswer`. */
export interface CarriedAnswer extends tilo.Resource {
  wd5mzler: number;  // carried
  arw4q74m: string;  // error
}

/** The actions `CarriedAnswer` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CARRIEDANSWER_ACTIONS = [] as const;

/** The `tl.so.tilo.passwords.CarriedAnswer` kind at `path`. */
export function carriedAnswer(ws: tilo.Workspace, path: string): CarriedAnswer {
  return resourceProxy(ws, path, SURFACE, CARRIEDANSWER_ACTIONS) as unknown as CarriedAnswer;
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
  /** Hands what the person typed on the system's card to the vault as `part` of the entry `id`: the host passes it on, so it never becomes a value here. Answers why it could not, or nothing. */
  keep_typed(args: {
    id: string;
    part: string;
  }): Promise<string>;
}

/** The actions `Passwords` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PASSWORDS_ACTIONS = ["keep_typed"] as const;

/** The `tl.so.tilo.passwords` kind at `path`. */
export function passwords(ws: tilo.Workspace, path: string): Passwords {
  return resourceProxy(ws, path, SURFACE, PASSWORDS_ACTIONS) as unknown as Passwords;
}

