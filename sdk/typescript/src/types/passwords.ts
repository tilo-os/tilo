/**
 * Types for Passwords (`so.tilo.passwords`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Item` is a `Resource` of the app's `tl.so.tilo.passwords.Item`, made by its factory with the
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
export const SURFACE = "so.tilo.passwords@37130bd8058a96d7";

/** What kind of secret an item is: what it is for, which sets the fields it has and the words that name them. One of "login", "api_key", "note", "wifi", "secret". */
export type Kind = "login" | "api_key" | "note" | "wifi" | "secret";

/** The arguments of `save_editor`. */
export interface SaveEditorArgs {
  password: string;
  notes: string;
  fields: string;
}

/** The arguments of `generate_password`. */
export interface GeneratePasswordArgs {
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

/** The arguments of `notice_age`. */
export interface NoticeAgeArgs {
}

/** The arguments of `delete`. */
export interface DeleteArgs {
  item: Item;
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

/** Item — `tl.so.tilo.passwords.Item`. */
export interface Item extends tilo.Resource {
  ildzcyo2: string;  // title
  ujkdowq4: Kind;  // kind
  tpqopgvl: string;  // username
  txdg33mm: string;  // password
  fxvfkz3x: string;  // notes
  qgllbsrc: string[];  // origins
  qoa35hei: string[];  // tags
  japk242o: Record<string, string>;  // fields
  tzeujwn7: boolean;  // favorite
  qzpekkx6: boolean;  // old
  /** Writes what the editor holds onto an item: the password and notes it was given, and the custom fields it typed. An empty password keeps the one the item had. */
  save_editor(args: {
    password: string;
    notes: string;
    fields: string;
  }): Promise<void>;
  /** Runs `save_editor` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  save_editorMany(items: SaveEditorArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Draws a strong password for the item: 20 characters, at least one capital, one small letter, one digit and one symbol. */
  generate_password(): Promise<void>;
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
  /** Copies the item's secret to the person's clipboard. */
  copy_secret(): Promise<void>;
  /** Copies what identifies the person to the account: a login's username, an API key's account, a Wi-Fi network's name. */
  copy_identity(): Promise<void>;
  /** Copies the addresses the item signs in to, one a line. */
  copy_origins(): Promise<void>;
  /** Notes an item whose password was last changed more than a year ago, as the daily check runs. */
  notice_age(): Promise<void>;
  /** Runs `notice_age` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  notice_ageMany(items: NoticeAgeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Replaces an item's addresses with `values`, one change. */
  write_origins(args: {
    values: string[];
  }): Promise<boolean>;
  /** Replaces an item's tags with `values`, one change. */
  write_tags(args: {
    values: string[];
  }): Promise<boolean>;
  /** Replaces an item's custom fields with `values`, one change. */
  write_fields(args: {
    values: Record<string, string>;
  }): Promise<boolean>;
}

/** The actions `Item` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ITEM_ACTIONS = ["save_editor", "save_editorMany", "generate_password", "generate_passwordMany", "favorite", "favoriteMany", "add_tag", "add_tagMany", "remove_tag", "remove_tagMany", "add_origin", "add_originMany", "remove_origin", "remove_originMany", "copy_secret", "copy_identity", "copy_origins", "notice_age", "notice_ageMany", "write_origins", "write_tags", "write_fields"] as const;

/** The `tl.so.tilo.passwords.Item` kind at `path`. */
export function item(ws: tilo.Workspace, path: string): Item {
  return resourceProxy(ws, path, SURFACE, ITEM_ACTIONS) as unknown as Item;
}

/** ImportRow — `tl.so.tilo.passwords.ImportRow`. */
export interface ImportRow extends tilo.Resource {
  l7jn4eol: string;  // title
  s6pvezan: string;  // username
  "4jcknnmq": string;  // origin
  jtrf57qd: string;  // password
  sx76etl2: string;  // notes
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
  "3xf6uda7": string;  // notice
  /** Starts an item: it opens in the window, named by what is typed first. */
  new_item(args?: {
    title?: string;
  }): Promise<Item>;
  /** Adds a login in one step: what an agent, a script or the VM's own end-to-end check asks of the vault. The name is required, the password is required, and an address is checked as a web address. */
  save_login(args: {
    name: string;
    origin: string;
    username: string;
    password: string;
  }): Promise<Item>;
  /** The vault's logins, by name and by the item that holds each: what a script, an agent or another device's window lists. The passwords are never part of it. */
  list_logins(): Promise<LoginList>;
  /** Deletes an item from the vault. */
  delete(args: {
    item: Item;
  }): Promise<void>;
  /** Runs `delete` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deleteMany(items: DeleteArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Reads the browser's CSV the person chose into the vault's waiting rows, each marked when the vault already holds that login. */
  preview_csv(args: {
    file: unknown;
  }): Promise<void>;
  /** Runs `preview_csv` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  preview_csvMany(items: PreviewCsvArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Brings the rows the person agreed to into the vault, skipping the ones the vault holds. */
  apply_import(): Promise<void>;
  /** Runs `apply_import` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  apply_importMany(items: ApplyImportArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Puts the waiting rows away. */
  clear_import(): Promise<void>;
  /** Runs `clear_import` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  clear_importMany(items: ClearImportArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Empties the vault's waiting rows. */
  clear_pending(): Promise<boolean>;
  /** The vault as a browser's CSV, named for what it holds: the person chooses where it goes. */
  export_csv(): Promise<unknown>;
  /** The vault as an encrypted backup under `password`: the person chooses where it goes. */
  export_backup(args: {
    password: string;
  }): Promise<unknown>;
  /** Brings the items a backup holds into the vault, and answers how many came in. */
  restore_backup(args: {
    password: string;
    file: unknown;
  }): Promise<number>;
  /** Takes into the vault what this device's secret service holds and a workspace may share. Each entry is sealed into the vault; the device's authentication material and its Wi-Fi networks stay where they are. An entry already taken in is not taken again. */
  absorb_device(): Promise<void>;
  /** Runs `absorb_device` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  absorb_deviceMany(items: AbsorbDeviceArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Vault` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const VAULT_ACTIONS = ["new_item", "save_login", "list_logins", "delete", "deleteMany", "preview_csv", "preview_csvMany", "apply_import", "apply_importMany", "clear_import", "clear_importMany", "clear_pending", "export_csv", "export_backup", "restore_backup", "absorb_device", "absorb_deviceMany"] as const;

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

/** AskedItem — `tl.so.tilo.passwords.AskedItem`. */
export interface AskedItem extends tilo.Resource {
  rzuvlal6: string;  // title
  "65lzjom4": string;  // kind
  gtezt3ys: string;  // username
  nqzlkgho: string;  // password
  nyoijvnx: string;  // notes
  pupxu7gb: string[];  // origins
  zrtsugqu: string[];  // tags
  yczcbynv: Record<string, string>;  // fields
}

/** The actions `AskedItem` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ASKEDITEM_ACTIONS = [] as const;

/** The `tl.so.tilo.passwords.AskedItem` kind at `path`. */
export function askedItem(ws: tilo.Workspace, path: string): AskedItem {
  return resourceProxy(ws, path, SURFACE, ASKEDITEM_ACTIONS) as unknown as AskedItem;
}

/** ItemsAsk — `tl.so.tilo.passwords.ItemsAsk`. */
export interface ItemsAsk extends tilo.Resource {
  "6nicnbs2": AskedItem[];  // items
}

/** The actions `ItemsAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ITEMSASK_ACTIONS = [] as const;

/** The `tl.so.tilo.passwords.ItemsAsk` kind at `path`. */
export function itemsAsk(ws: tilo.Workspace, path: string): ItemsAsk {
  return resourceProxy(ws, path, SURFACE, ITEMSASK_ACTIONS) as unknown as ItemsAsk;
}

/** BackupAsk — `tl.so.tilo.passwords.BackupAsk`. */
export interface BackupAsk extends tilo.Resource {
  a45t5mnl: AskedItem[];  // items
  wmhsd5ch: string;  // passphrase
}

/** The actions `BackupAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const BACKUPASK_ACTIONS = [] as const;

/** The `tl.so.tilo.passwords.BackupAsk` kind at `path`. */
export function backupAsk(ws: tilo.Workspace, path: string): BackupAsk {
  return resourceProxy(ws, path, SURFACE, BACKUPASK_ACTIONS) as unknown as BackupAsk;
}

/** RestoreAsk — `tl.so.tilo.passwords.RestoreAsk`. */
export interface RestoreAsk extends tilo.Resource {
  rnonlkte: string;  // url
  cynjyelt: string;  // passphrase
}

/** The actions `RestoreAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const RESTOREASK_ACTIONS = [] as const;

/** The `tl.so.tilo.passwords.RestoreAsk` kind at `path`. */
export function restoreAsk(ws: tilo.Workspace, path: string): RestoreAsk {
  return resourceProxy(ws, path, SURFACE, RESTOREASK_ACTIONS) as unknown as RestoreAsk;
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

/** RemoveAsk — `tl.so.tilo.passwords.RemoveAsk`. */
export interface RemoveAsk extends tilo.Resource {
  eevmellb: string[];  // ids
}

/** The actions `RemoveAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const REMOVEASK_ACTIONS = [] as const;

/** The `tl.so.tilo.passwords.RemoveAsk` kind at `path`. */
export function removeAsk(ws: tilo.Workspace, path: string): RemoveAsk {
  return resourceProxy(ws, path, SURFACE, REMOVEASK_ACTIONS) as unknown as RemoveAsk;
}

/** DeviceItem — `tl.so.tilo.passwords.DeviceItem`. */
export interface DeviceItem extends tilo.Resource {
  wnjsdr3d: string;  // id
  lwvhidtr: string;  // title
  vd3jnhgx: string;  // kind
  f7f2gp2a: string;  // username
  "6okrlhly": string;  // password
  p4viod5a: string;  // notes
  "4ejpoi2g": string[];  // origins
  wze5ieza: string[];  // tags
  gr5iqwm5: Record<string, string>;  // fields
}

/** The actions `DeviceItem` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DEVICEITEM_ACTIONS = [] as const;

/** The `tl.so.tilo.passwords.DeviceItem` kind at `path`. */
export function deviceItem(ws: tilo.Workspace, path: string): DeviceItem {
  return resourceProxy(ws, path, SURFACE, DEVICEITEM_ACTIONS) as unknown as DeviceItem;
}

/** DeviceAnswer — `tl.so.tilo.passwords.DeviceAnswer`. */
export interface DeviceAnswer extends tilo.Resource {
  vqmxqaz7: DeviceItem[];  // items
  is77ciid: string;  // error
}

/** The actions `DeviceAnswer` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DEVICEANSWER_ACTIONS = [] as const;

/** The `tl.so.tilo.passwords.DeviceAnswer` kind at `path`. */
export function deviceAnswer(ws: tilo.Workspace, path: string): DeviceAnswer {
  return resourceProxy(ws, path, SURFACE, DEVICEANSWER_ACTIONS) as unknown as DeviceAnswer;
}

/** CsvRow — `tl.so.tilo.passwords.CsvRow`. */
export interface CsvRow extends tilo.Resource {
  ufq2o5qa: string;  // title
  pjl3fx3t: string;  // username
  "7ca4ejcb": string;  // origin
  u3j7epmt: string;  // password
  we5a77lw: string;  // notes
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

