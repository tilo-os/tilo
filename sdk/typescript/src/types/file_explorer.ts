/**
 * Types for Files (`so.tilo.file-explorer`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Notice` is a `Resource` of the app's `tl.tilo.host.Notice`, made by its factory with the
 * resource's path (`file-explorer/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.file-explorer@0e7f6404f2e5daae";

/** How a folder's entries read. One of "name", "size", "modified", "format". */
export type SortBy = "name" | "size" | "modified" | "format";

/** The arguments of `read_listing`. */
export interface ReadListingArgs {
  place?: string;
}

/** The arguments of `changed`. */
export interface ChangedArgs {
}

/** The arguments of `go`. */
export interface GoArgs {
  place: string;
}

/** The arguments of `show`. */
export interface ShowArgs {
  path: string;
}

/** The arguments of `back`. */
export interface BackArgs {
}

/** The arguments of `forward`. */
export interface ForwardArgs {
}

/** The arguments of `up`. */
export interface UpArgs {
}

/** The arguments of `dismiss_notice`. */
export interface DismissNoticeArgs {
}

/** The arguments of `sort_by`. */
export interface SortByArgs {
  by: SortBy;
}

/** The arguments of `ascending`. */
export interface AscendingArgs {
  first: boolean;
}

/** The arguments of `show_grid`. */
export interface ShowGridArgs {
  grid: boolean;
}

/** The arguments of `choose_view`. */
export interface ChooseViewArgs {
  i: number;
}

/** The arguments of `toggle_hidden`. */
export interface ToggleHiddenArgs {
  hidden: boolean;
}

/** The arguments of `open_entry`. */
export interface OpenEntryArgs {
  e: Entry;
}

/** The arguments of `find_openers`. */
export interface FindOpenersArgs {
  path: string;
}

/** The arguments of `open_with`. */
export interface OpenWithArgs {
  chosen: string;
}

/** The arguments of `close_openers`. */
export interface CloseOpenersArgs {
}

/** The arguments of `open_terminal`. */
export interface OpenTerminalArgs {
  folder: string;
}

/** The arguments of `compress_entry`. */
export interface CompressEntryArgs {
  path: string;
  zip: boolean;
}

/** The arguments of `extract_entry`. */
export interface ExtractEntryArgs {
  path: string;
}

/** The arguments of `open_recent`. */
export interface OpenRecentArgs {
  r: Recent;
}

/** The arguments of `touch_entry`. */
export interface TouchEntryArgs {
  e: Entry;
}

/** The arguments of `choose`. */
export interface ChooseArgs {
  paths: string[];
}

/** The arguments of `select_all`. */
export interface SelectAllArgs {
}

/** The arguments of `clear_selection`. */
export interface ClearSelectionArgs {
}

/** The arguments of `note_recent`. */
export interface NoteRecentArgs {
  name: string;
  path: string;
}

/** The arguments of `new_folder`. */
export interface NewFolderArgs {
  name?: string;
}

/** The arguments of `start_new_file`. */
export interface StartNewFileArgs {
}

/** The arguments of `new_from_template`. */
export interface NewFromTemplateArgs {
  template: string;
  typed: string;
}

/** The arguments of `new_file`. */
export interface NewFileArgs {
  name?: string;
}

/** The arguments of `rename_entry`. */
export interface RenameEntryArgs {
  path: string;
  name: string;
}

/** The arguments of `trash_entries`. */
export interface TrashEntriesArgs {
  paths: string[];
}

/** The arguments of `restore_entry`. */
export interface RestoreEntryArgs {
  name: string;
}

/** The arguments of `delete_permanently`. */
export interface DeletePermanentlyArgs {
  path: string;
}

/** The arguments of `empty_trash`. */
export interface EmptyTrashArgs {
}

/** The arguments of `carry`. */
export interface CarryArgs {
  paths: string[];
  cut: boolean;
}

/** The arguments of `carry_selection`. */
export interface CarrySelectionArgs {
  cut: boolean;
}

/** The arguments of `compress_selection`. */
export interface CompressSelectionArgs {
  zip: boolean;
}

/** The arguments of `paste`. */
export interface PasteArgs {
}

/** The arguments of `key_on`. */
export interface KeyOnArgs {
  focused: string;
  key: string;
}

/** The arguments of `carry_row`. */
export interface CarryRowArgs {
  path: string;
  cut: boolean;
}

/** The arguments of `trash_row`. */
export interface TrashRowArgs {
  path: string;
}

/** The arguments of `add_favorite`. */
export interface AddFavoriteArgs {
  place: string;
}

/** The arguments of `remove_favorite`. */
export interface RemoveFavoriteArgs {
  favorite: Favorite;
}

/** The arguments of `inspect`. */
export interface InspectArgs {
  e: Entry;
}

/** The arguments of `name_it`. */
export interface NameItArgs {
  naming: string;
  typed: string;
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

/** Entry — `tl.so.tilo.file-explorer.Entry`. */
export interface Entry extends tilo.Resource {
  lm429k0c: string;  // name
  sq7vzl81: string;  // path
  "7x0zd867": boolean;  // folder
  tnbzoc5e: number;  // size
  ora1368t: string;  // modified
  qttidvz5: boolean;  // hidden
  iqh17pvk: string;  // original
}

/** The actions `Entry` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ENTRY_ACTIONS = [] as const;

/** The `tl.so.tilo.file-explorer.Entry` kind at `path`. */
export function entry(ws: tilo.Workspace, path: string): Entry {
  return resourceProxy(ws, path, SURFACE, ENTRY_ACTIONS) as unknown as Entry;
}

/** Listing — `tl.so.tilo.file-explorer.Listing`. */
export interface Listing extends tilo.Resource {
  uzztpcqp: string;  // path
  "9ilrk28j": Entry[];  // entries
  qwo5qxtp: string;  // error
  yflohkj0: boolean;  // read
  fzgm7n2x: string;  // watch
}

/** The actions `Listing` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LISTING_ACTIONS = [] as const;

/** The `tl.so.tilo.file-explorer.Listing` kind at `path`. */
export function listing(ws: tilo.Workspace, path: string): Listing {
  return resourceProxy(ws, path, SURFACE, LISTING_ACTIONS) as unknown as Listing;
}

/** Place — `tl.so.tilo.file-explorer.Place`. */
export interface Place extends tilo.Resource {
  kcy1i0qo: string;  // label
  z0dyhfw9: string;  // path
  "2v9tcdo2": string;  // group
}

/** The actions `Place` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PLACE_ACTIONS = [] as const;

/** The `tl.so.tilo.file-explorer.Place` kind at `path`. */
export function place(ws: tilo.Workspace, path: string): Place {
  return resourceProxy(ws, path, SURFACE, PLACE_ACTIONS) as unknown as Place;
}

/** Favorite — `tl.so.tilo.file-explorer.Favorite`. */
export interface Favorite extends tilo.Resource {
  m5mpzvqt: string;  // place
}

/** The actions `Favorite` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const FAVORITE_ACTIONS = [] as const;

/** The `tl.so.tilo.file-explorer.Favorite` kind at `path`. */
export function favorite(ws: tilo.Workspace, path: string): Favorite {
  return resourceProxy(ws, path, SURFACE, FAVORITE_ACTIONS) as unknown as Favorite;
}

/** Recent — `tl.so.tilo.file-explorer.Recent`. */
export interface Recent extends tilo.Resource {
  v8w5s5s2: string;  // name
  ddeyygde: string;  // path
  flcdg3ss: string;  // opened
}

/** The actions `Recent` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const RECENT_ACTIONS = [] as const;

/** The `tl.so.tilo.file-explorer.Recent` kind at `path`. */
export function recent(ws: tilo.Workspace, path: string): Recent {
  return resourceProxy(ws, path, SURFACE, RECENT_ACTIONS) as unknown as Recent;
}

/** Session — `tl.so.tilo.file-explorer.Session`. */
export interface Session extends tilo.Resource {
  zxu7qmx7: string;  // here
  roylouv1: SortBy;  // sort
  hm8xkfti: boolean;  // ascending
  gkgwx8bk: boolean;  // hidden
  "31bypi4c": boolean;  // grid
  "9cdifavr": string[];  // went
  "4tiugype": string[];  // ahead
  "6twkbcct": string[];  // carrying
  "5y3gacnc": boolean;  // cutting
  "9psytdam": Array<tilo.Entry<Listing>>;  // listings
  "8kzpf3es": unknown | null;  // preview
  omzv6cq5: string;  // previewed
  l1bciwln: string;  // words
  wduoe4ej: string[];  // chosen
  zgzedmq4: string;  // selected
  kljwn54u: string[];  // templates
  uswipaet: string;  // opening
  v4v6h4ut: unknown[];  // openers
  fpb2xkum: string;  // notice
}

/** The actions `Session` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SESSION_ACTIONS = [] as const;

/** The `tl.so.tilo.file-explorer.Session` kind at `path`. */
export function session(ws: tilo.Workspace, path: string): Session {
  return resourceProxy(ws, path, SURFACE, SESSION_ACTIONS) as unknown as Session;
}

/** Files — `tl.so.tilo.file-explorer.Files`. */
export interface Files extends tilo.Resource {
  "7dfrn16h": Array<tilo.Entry<Session>>;  // sessions
  fuihvmu6: Array<tilo.Entry<Place>>;  // places
  j9xycg06: Array<tilo.Entry<Favorite>>;  // favorites
  "60ufpbms": Array<tilo.Entry<Recent>>;  // recent
  /** Reads the place `place` into this window's listing for it: a folder's entries, this device's trash, or nothing for the places this app keeps itself. */
  read_listing(args?: {
    place?: string;
  }): Promise<void>;
  /** Runs `read_listing` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  read_listingMany(items: ReadListingArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Runs when a watched folder changes: reads the folder shown again. */
  changed(): Promise<void>;
  /** Runs `changed` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  changedMany(items: ChangedArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Goes to the place `place`, reading it: a folder, the trash, the recent files or the apps. */
  go(args: {
    place: string;
  }): Promise<void>;
  /** Runs `go` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  goMany(items: GoArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shows the place `path` names: the folder itself, or the folder holding the file, with that file marked. Another app asks this when it shows where something it made or saved stands; a person opens a file with `open_entry` instead, which opens what the file holds. */
  show(args: {
    path: string;
  }): Promise<void>;
  /** Runs `show` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  showMany(items: ShowArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Goes back to the place the person came from. */
  back(): Promise<void>;
  /** Runs `back` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  backMany(items: BackArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Goes forward again to the place the person left. */
  forward(): Promise<void>;
  /** Runs `forward` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  forwardMany(items: ForwardArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Goes to the folder above the one shown. */
  up(): Promise<void>;
  /** Runs `up` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  upMany(items: UpArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sends the entries the person chose to the nearby device `nearby`, as Nearby Send offers it. */
  send_nearby(args: {
    nearby: string;
  }): Promise<void>;
  /** Dismisses what the window last said went wrong. */
  dismiss_notice(): Promise<void>;
  /** Runs `dismiss_notice` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  dismiss_noticeMany(items: DismissNoticeArgs[]): Promise<tilo.ActionAnswer[]>;
  // the app's `refresh` action is the SDK's own: run it with `action("refresh")`.
  /** Sorts the folder shown by `by`, the way `ascending` says. */
  sort_by(args: {
    by: SortBy;
  }): Promise<void>;
  /** Runs `sort_by` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  sort_byMany(items: SortByArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sorts the folder shown oldest or smallest first, or the other way. */
  ascending(args: {
    first: boolean;
  }): Promise<void>;
  /** Runs `ascending` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  ascendingMany(items: AscendingArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Draws the folder as a grid of cards, or as rows. */
  show_grid(args: {
    grid: boolean;
  }): Promise<void>;
  /** Runs `show_grid` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  show_gridMany(items: ShowGridArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Draws the folder the way the segment at place `i` says: the list at 0, the grid at 1. */
  choose_view(args: {
    i: number;
  }): Promise<void>;
  /** Runs `choose_view` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  choose_viewMany(items: ChooseViewArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shows or hides the names that start with a dot. */
  toggle_hidden(args: {
    hidden: boolean;
  }): Promise<void>;
  /** Runs `toggle_hidden` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  toggle_hiddenMany(items: ToggleHiddenArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Opens the entry `e`: a folder in this window, a file in the app that opens its kind. */
  open_entry(args: {
    e: Entry;
  }): Promise<void>;
  /** Runs `open_entry` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  open_entryMany(items: OpenEntryArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Finds the apps that open the entry at `path`, for the person to choose one. */
  find_openers(args: {
    path: string;
  }): Promise<void>;
  /** Runs `find_openers` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  find_openersMany(items: FindOpenersArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Opens the entry the person asked to open with in `chosen`, one of the apps offered. */
  open_with(args: {
    chosen: string;
  }): Promise<void>;
  /** Runs `open_with` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  open_withMany(items: OpenWithArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Puts away the apps offered for the entry the person asked to open with. */
  close_openers(): Promise<void>;
  /** Runs `close_openers` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  close_openersMany(items: CloseOpenersArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Opens a terminal in the folder at `folder`. */
  open_terminal(args: {
    folder: string;
  }): Promise<void>;
  /** Runs `open_terminal` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  open_terminalMany(items: OpenTerminalArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Packs the entry at `path` into a new archive beside it, named after it: `zip` picks a ZIP, and TAR.ZST otherwise. */
  compress_entry(args: {
    path: string;
    zip: boolean;
  }): Promise<void>;
  /** Runs `compress_entry` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  compress_entryMany(items: CompressEntryArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Extracts the archive at `path` into a new folder beside it, named after it. */
  extract_entry(args: {
    path: string;
  }): Promise<void>;
  /** Runs `extract_entry` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  extract_entryMany(items: ExtractEntryArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Opens a file from Recent again, as the entry it was. */
  open_recent(args: {
    r: Recent;
  }): Promise<void>;
  /** Runs `open_recent` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  open_recentMany(items: OpenRecentArgs[]): Promise<tilo.ActionAnswer[]>;
  /** A press on an entry: a folder opens as a person presses it once, and a file is read into the inspector beside the list. */
  touch_entry(args: {
    e: Entry;
  }): Promise<void>;
  /** Runs `touch_entry` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  touch_entryMany(items: TouchEntryArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The entries the list reports as chosen, in the order they stand there. */
  choose(args: {
    paths: string[];
  }): Promise<void>;
  /** Runs `choose` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  chooseMany(items: ChooseArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Chooses every entry the folder shown places: what a press on 'Select all' does. */
  select_all(): Promise<void>;
  /** Runs `select_all` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  select_allMany(items: SelectAllArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Chooses nothing. */
  clear_selection(): Promise<void>;
  /** Runs `clear_selection` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  clear_selectionMany(items: ClearSelectionArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Notes that the file `path` was opened, so it stands under Recent. */
  note_recent(args: {
    name: string;
    path: string;
  }): Promise<void>;
  /** Runs `note_recent` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  note_recentMany(items: NoteRecentArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Makes a folder named `name` in the place shown. */
  new_folder(args?: {
    name?: string;
  }): Promise<void>;
  /** Runs `new_folder` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  new_folderMany(items: NewFolderArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Reads the person's `~/Templates`, so naming a new file can offer them. */
  start_new_file(): Promise<void>;
  /** Runs `start_new_file` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  start_new_fileMany(items: StartNewFileArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Copies the template `template` into the place shown under `typed`'s name, or the template's. */
  new_from_template(args: {
    template: string;
    typed: string;
  }): Promise<void>;
  /** Runs `new_from_template` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  new_from_templateMany(items: NewFromTemplateArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Makes a text file named `name` in the place shown, for the person to fill. */
  new_file(args?: {
    name?: string;
  }): Promise<void>;
  /** Runs `new_file` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  new_fileMany(items: NewFileArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Opens the drive `drive` and goes to where it is open, so a removable disk is a place like any other folder. A drive that still has to open shows as such until the device says where it is. */
  open_drive(args: {
    drive: unknown;
  }): Promise<void>;
  /** Renames the entry at `path` to `name` in its own folder. */
  rename_entry(args: {
    path: string;
    name: string;
  }): Promise<void>;
  /** Runs `rename_entry` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  rename_entryMany(items: RenameEntryArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Moves the entries at `paths` to this device's trash. */
  trash_entries(args: {
    paths: string[];
  }): Promise<void>;
  /** Runs `trash_entries` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  trash_entriesMany(items: TrashEntriesArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Restores the trashed entry `name` to where it came from. */
  restore_entry(args: {
    name: string;
  }): Promise<void>;
  /** Runs `restore_entry` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  restore_entryMany(items: RestoreEntryArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Removes the entry at `path` for good. */
  delete_permanently(args: {
    path: string;
  }): Promise<void>;
  /** Runs `delete_permanently` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  delete_permanentlyMany(items: DeletePermanentlyArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Empties this device's trash: every entry, and the receipt that would put it back. */
  empty_trash(): Promise<void>;
  /** Runs `empty_trash` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  empty_trashMany(items: EmptyTrashArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Remembers the entries at `paths` as what a paste copies; `cut` makes it a move. */
  carry(args: {
    paths: string[];
    cut: boolean;
  }): Promise<void>;
  /** Runs `carry` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  carryMany(items: CarryArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Remembers the entries the person chose as what a paste copies. */
  carry_selection(args: {
    cut: boolean;
  }): Promise<void>;
  /** Runs `carry_selection` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  carry_selectionMany(items: CarrySelectionArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Packs the entries the person chose into one archive beside them: `zip` picks a ZIP, and TAR.ZST otherwise. The archive is named after the entry alone, or after its folder when several go in. */
  compress_selection(args: {
    zip: boolean;
  }): Promise<void>;
  /** Runs `compress_selection` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  compress_selectionMany(items: CompressSelectionArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Pastes what the person carries into the place shown. A name the folder already holds is kept both: the copy lands as “name (2)”, and a cut entry is renamed before it moves, so nothing is overwritten. */
  paste(): Promise<void>;
  /** Runs `paste` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  pasteMany(items: PasteArgs[]): Promise<tilo.ActionAnswer[]>;
  /** A name `folder` does not hold yet, for an entry that would land as `name`: the name itself, or “name (2)”, “name (3)” before the extension, so a paste or a drop never replaces a file. */
  free_name(args: {
    folder: string;
    name: string;
  }): Promise<string>;
  /** A key pressed on the row `focused`: what Files does with a copy, a cut or a delete over the entries the person chose, or over that row when it is not among them. */
  key_on(args: {
    focused: string;
    key: string;
  }): Promise<void>;
  /** Runs `key_on` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  key_onMany(items: KeyOnArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Remembers what a copy from the row at `path` takes: the entries the person chose when that row is one of them, and the row alone otherwise. */
  carry_row(args: {
    path: string;
    cut: boolean;
  }): Promise<void>;
  /** Runs `carry_row` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  carry_rowMany(items: CarryRowArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Moves to Trash what a delete from the row at `path` takes: the entries the person chose when that row is one of them, and the row alone otherwise. */
  trash_row(args: {
    path: string;
  }): Promise<void>;
  /** Runs `trash_row` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  trash_rowMany(items: TrashRowArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Marks the place `place` in the sidebar. */
  add_favorite(args: {
    place: string;
  }): Promise<void>;
  /** Runs `add_favorite` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  add_favoriteMany(items: AddFavoriteArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes the favorite `favorite` out of the sidebar. */
  remove_favorite(args: {
    favorite: Favorite;
  }): Promise<void>;
  /** Runs `remove_favorite` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remove_favoriteMany(items: RemoveFavoriteArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Reads what the chosen entry is, so the inspector says its size, its kind and its path, and shows a picture file's picture or a text file's first words. */
  inspect(args: {
    e: Entry;
  }): Promise<void>;
  /** Runs `inspect` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  inspectMany(items: InspectArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Does what the name asked of the person is for: makes the folder or the file, or renames the entry. */
  name_it(args: {
    naming: string;
    typed: string;
  }): Promise<void>;
  /** Runs `name_it` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  name_itMany(items: NameItArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Opens the app `a`, as pressing its row does. */
  open_app(args: {
    a: unknown;
  }): Promise<void>;
}

/** The actions `Files` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const FILES_ACTIONS = ["read_listing", "read_listingMany", "changed", "changedMany", "go", "goMany", "show", "showMany", "back", "backMany", "forward", "forwardMany", "up", "upMany", "send_nearby", "dismiss_notice", "dismiss_noticeMany", "sort_by", "sort_byMany", "ascending", "ascendingMany", "show_grid", "show_gridMany", "choose_view", "choose_viewMany", "toggle_hidden", "toggle_hiddenMany", "open_entry", "open_entryMany", "find_openers", "find_openersMany", "open_with", "open_withMany", "close_openers", "close_openersMany", "open_terminal", "open_terminalMany", "compress_entry", "compress_entryMany", "extract_entry", "extract_entryMany", "open_recent", "open_recentMany", "touch_entry", "touch_entryMany", "choose", "chooseMany", "select_all", "select_allMany", "clear_selection", "clear_selectionMany", "note_recent", "note_recentMany", "new_folder", "new_folderMany", "start_new_file", "start_new_fileMany", "new_from_template", "new_from_templateMany", "new_file", "new_fileMany", "open_drive", "rename_entry", "rename_entryMany", "trash_entries", "trash_entriesMany", "restore_entry", "restore_entryMany", "delete_permanently", "delete_permanentlyMany", "empty_trash", "empty_trashMany", "carry", "carryMany", "carry_selection", "carry_selectionMany", "compress_selection", "compress_selectionMany", "paste", "pasteMany", "free_name", "key_on", "key_onMany", "carry_row", "carry_rowMany", "trash_row", "trash_rowMany", "add_favorite", "add_favoriteMany", "remove_favorite", "remove_favoriteMany", "inspect", "inspectMany", "name_it", "name_itMany", "open_app"] as const;

/** The `tl.so.tilo.file-explorer.Files` kind at `path`. */
export function files(ws: tilo.Workspace, path: string): Files {
  return resourceProxy(ws, path, SURFACE, FILES_ACTIONS) as unknown as Files;
}

/** Crumb — `tl.so.tilo.file-explorer.Crumb`. */
export interface Crumb extends tilo.Resource {
  lbeg1rhd: string;  // label
  sv3ucdrd: string;  // path
}

/** The actions `Crumb` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CRUMB_ACTIONS = [] as const;

/** The `tl.so.tilo.file-explorer.Crumb` kind at `path`. */
export function crumb(ws: tilo.Workspace, path: string): Crumb {
  return resourceProxy(ws, path, SURFACE, CRUMB_ACTIONS) as unknown as Crumb;
}

/** Files — `tl.so.tilo.file-explorer`. */
export interface FileExplorer extends tilo.Resource {
}

/** The actions `FileExplorer` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const FILEEXPLORER_ACTIONS = [] as const;

/** The `tl.so.tilo.file-explorer` kind at `path`. */
export function fileExplorer(ws: tilo.Workspace, path: string): FileExplorer {
  return resourceProxy(ws, path, SURFACE, FILEEXPLORER_ACTIONS) as unknown as FileExplorer;
}

