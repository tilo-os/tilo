/**
 * Types for Shell (`so.tilo.shell`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Notice` is a `Resource` of the app's `tl.tilo.host.Notice`, made by its factory with the
 * resource's path (`shell/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.shell@b2d6fcd11eac3433";

/** One part a person can put on the top bar, in their order. The case's icon is the glyph the settings and the drag ghost show for it. One of "menu", "workspace", "app_menu", "strip", "tasks", "clock", "people", "status", "power", "agent", "spacer". */
export type BarPartKind = "menu" | "workspace" | "app_menu" | "strip" | "tasks" | "clock" | "people" | "status" | "power" | "agent" | "spacer";

/** The corner of the display the cards of what is live dock in. One of "top_leading", "top_trailing", "bottom_leading", "bottom_trailing". */
export type CardCorner = "top_leading" | "top_trailing" | "bottom_leading" | "bottom_trailing";

/** A named set of the settings, which the person applies in one action: a chrome preset. `Viewing.style` (layout.tl) is the *view* — how the same places are shown — and is a different setting; this one names the bar and the dock. One of "tilo", "macos", "windows", "gnome", "minimal", "windows_desktop". */
export type ChromePreset = "tilo" | "macos" | "windows" | "gnome" | "minimal" | "windows_desktop";

/** The person's shell: which parts the top bar and the dock have, in what order, on which edge, how they look and when they show. It is the person's own document, synced to their devices with history and undo; every change is one of the actions below, so an agent acting for the person changes it exactly as they do, and undo takes any of it back. shell.md's model: a shell is a view of the person's layout. This document is the layout's chrome — the bar and the dock — and the rest of the person's layout (windows, strips, canvas places) is the shell's own layout beside it. The edge of the screen a part rides. One of "top", "bottom", "left", "right". */
export type Edge = "top" | "bottom" | "left" | "right";

/** What a bar or dock is made of: the theme's translucent material over what is behind it, or its opaque bar ground. One of "translucent", "solid". */
export type Finish = "translucent" | "solid";

/** How a bar or dock sits in its edge: inset with rounded corners, floating over what it covers, or edge to edge and square. One of "floating", "full". */
export type Fit = "floating" | "full";

/** When a bar or dock shows. One of "always", "no_overlap", "edge_reveal", "auto_hide". */
export type Showing = "always" | "no_overlap" | "edge_reveal" | "auto_hide";

/** How large the parts are. One of "small", "regular", "large". */
export type Size = "small" | "regular" | "large";

/** The widgets a person can keep on their shell. One of "music", "team", "calendar", "system_monitor". */
export type WidgetKind = "music" | "team" | "calendar" | "system_monitor";

/** The arguments of `open_folder`. */
export interface OpenFolderArgs {
  path: string;
}

/** The arguments of `close_folder`. */
export interface CloseFolderArgs {
}

/** The arguments of `asked_on_canvas`. */
export interface AskedOnCanvasArgs {
}

/** The arguments of `apply`. */
export interface ApplyArgs {
  layout: ChromePreset;
}

/** The arguments of `show_app_menus`. */
export interface ShowAppMenusArgs {
}

/** The arguments of `hide_app_menus`. */
export interface HideAppMenusArgs {
}

/** The arguments of `reset`. */
export interface ResetArgs {
}

/** The arguments of `bar_on`. */
export interface BarOnArgs {
  edge: Edge;
}

/** The arguments of `set_bar_fit`. */
export interface SetBarFitArgs {
  fit: Fit;
}

/** The arguments of `set_bar_finish`. */
export interface SetBarFinishArgs {
  finish: Finish;
}

/** The arguments of `set_bar_showing`. */
export interface SetBarShowingArgs {
  showing: Showing;
}

/** The arguments of `set_bar_size`. */
export interface SetBarSizeArgs {
  size: Size;
}

/** The arguments of `show_bar`. */
export interface ShowBarArgs {
  on?: boolean;
}

/** The arguments of `dock_on`. */
export interface DockOnArgs {
  edge: Edge;
}

/** The arguments of `dock_cards`. */
export interface DockCardsArgs {
  corner: CardCorner;
}

/** The arguments of `set_dock_fit`. */
export interface SetDockFitArgs {
  fit: Fit;
}

/** The arguments of `set_dock_finish`. */
export interface SetDockFinishArgs {
  finish: Finish;
}

/** The arguments of `set_dock_showing`. */
export interface SetDockShowingArgs {
  showing: Showing;
}

/** The arguments of `set_dock_size`. */
export interface SetDockSizeArgs {
  size: Size;
}

/** The arguments of `show_dock`. */
export interface ShowDockArgs {
  on?: boolean;
}

/** The arguments of `set_dock_running`. */
export interface SetDockRunningArgs {
  on: boolean;
}

/** The arguments of `set_dock_launcher`. */
export interface SetDockLauncherArgs {
  on: boolean;
}

/** The arguments of `set_dock_zoom`. */
export interface SetDockZoomArgs {
  on: boolean;
}

/** The arguments of `move_part`. */
export interface MovePartArgs {
  part: BarPart;
  before: BarPart;
}

/** The arguments of `place_part`. */
export interface PlacePartArgs {
  part: BarPart;
  position: number;
}

/** The arguments of `show_part`. */
export interface ShowPartArgs {
  part: BarPart;
  on: boolean;
}

/** The arguments of `move_item`. */
export interface MoveItemArgs {
  item: DockItem;
  before: DockItem;
}

/** The arguments of `place_item`. */
export interface PlaceItemArgs {
  item: DockItem;
  position: number;
}

/** The arguments of `pin`. */
export interface PinArgs {
  key: string;
  title: string;
  icon: string;
  next?: string;
}

/** The arguments of `unpin`. */
export interface UnpinArgs {
  item: DockItem;
}

/** The arguments of `report_problem`. */
export interface ReportProblemArgs {
}

/** The arguments of `cancel_report`. */
export interface CancelReportArgs {
}

/** The arguments of `open_style_settings`. */
export interface OpenStyleSettingsArgs {
}

/** The arguments of `close_style_settings`. */
export interface CloseStyleSettingsArgs {
}

/** The arguments of `show_style_settings`. */
export interface ShowStyleSettingsArgs {
  on?: boolean;
}

/** The arguments of `set_strips_tabs`. */
export interface SetStripsTabsArgs {
  on: boolean;
}

/** The arguments of `set_frames_snap`. */
export interface SetFramesSnapArgs {
  on: boolean;
}

/** The arguments of `recents_of_an_afternoon`. */
export interface RecentsOfAnAfternoonArgs {
}

/** The arguments of `tab_place_here`. */
export interface TabPlaceHereArgs {
  s: LayoutStrip;
  target: unknown;
}

/** The arguments of `reorder_place`. */
export interface ReorderPlaceArgs {
  s: LayoutStrip;
  place: unknown;
  at: number;
}

/** The arguments of `bring_forward`. */
export interface BringForwardArgs {
  s: LayoutStrip;
}

/** The arguments of `show_style`. */
export interface ShowStyleArgs {
  style: string;
}

/** The arguments of `size_place`. */
export interface SizePlaceArgs {
  key: string;
  size: number[];
}

/** The arguments of `place_dropped`. */
export interface PlaceDroppedArgs {
  target: unknown;
  at: number[];
}

/** The arguments of `query_show`. */
export interface QueryShowArgs {
  place: string;
  found: unknown;
}

/** The arguments of `query_retry`. */
export interface QueryRetryArgs {
  place: string;
}

/** The arguments of `show_place`. */
export interface ShowPlaceArgs {
  place: unknown;
}

/** The arguments of `equal_widths`. */
export interface EqualWidthsArgs {
  w: unknown;
}

/** The arguments of `place_widget`. */
export interface PlaceWidgetArgs {
  kind: WidgetKind;
  at: number[];
  room: number[];
  display: number;
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

/** Picking — `tl.so.tilo.shell.Picking`. */
export interface Picking extends tilo.Resource {
  "5xjx7oct": string;  // folder
  "5esxwq2o": unknown[];  // entries
  xifz6fny: unknown[];  // places
  /** Show the folder `path` to choose files in, as this device reads it now, with removable media as a place the picker offers. */
  open_folder(args: {
    path: string;
  }): Promise<void>;
  /** Runs `open_folder` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  open_folderMany(items: OpenFolderArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Stop showing a folder: the question that asked for files is answered. */
  close_folder(): Promise<void>;
  /** Runs `close_folder` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  close_folderMany(items: CloseFolderArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Picking` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PICKING_ACTIONS = ["open_folder", "open_folderMany", "close_folder", "close_folderMany"] as const;

/** The `tl.so.tilo.shell.Picking` kind at `path`. */
export function picking(ws: tilo.Workspace, path: string): Picking {
  return resourceProxy(ws, path, SURFACE, PICKING_ACTIONS) as unknown as Picking;
}

/** CallEdit — `tl.so.tilo.shell.CallEdit`. */
export interface CallEdit extends tilo.Resource {
  r6c6zjdg: string[];  // path
  "5fhfjzcm": string;  // text
}

/** The actions `CallEdit` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CALLEDIT_ACTIONS = [] as const;

/** The `tl.so.tilo.shell.CallEdit` kind at `path`. */
export function callEdit(ws: tilo.Workspace, path: string): CallEdit {
  return resourceProxy(ws, path, SURFACE, CALLEDIT_ACTIONS) as unknown as CallEdit;
}

/** Shell — `tl.so.tilo.shell.Shell`. */
export interface Shell extends tilo.Resource {
  qsrjrjkd: ChromePreset;  // preset
  "2se66uzq": ChromePreset;  // based_on
  nt6y5bk3: boolean;  // tuned
  kqnswwzm: boolean;  // bar_shown
  mi3yni2m: Edge;  // bar_edge
  la4yrobd: Fit;  // bar_fit
  "6ujrnpej": Finish;  // bar_finish
  b2fcmpwg: Showing;  // bar_showing
  "773tn644": Size;  // bar_size
  voqg5seo: Array<tilo.Entry<BarPart>>;  // bar_parts
  b7grsgnv: boolean;  // dock_shown
  o6tdqxq3: Edge;  // dock_edge
  dq6kzv2v: Fit;  // dock_fit
  y5rlj3v3: Finish;  // dock_finish
  nz74n3iu: Showing;  // dock_showing
  wx25jsef: Size;  // dock_size
  hx5alotd: Array<tilo.Entry<DockItem>>;  // dock_items
  ftcph3ii: CardCorner;  // cards_corner
  ffa2ohzm: boolean;  // dock_running
  h5oo7js2: boolean;  // dock_launcher
  "5n4s55tz": boolean;  // dock_zoom
  "7xqpekfu": boolean;  // settings_open
  ayatksvb: boolean;  // strips_tabs
  fyquhn4g: boolean;  // frames_snap
  "6bamt22e": boolean;  // canvas_asked
  dpb7af4j: boolean;  // reporting
  "7bxopjro": ReportPick[];  // report_picks
  r6satdro: number[];  // report_area
  /** The person asked on the canvas: the hint the empty canvas says until then has been said (`Shell.canvas_asked`), and stays away from now on. */
  asked_on_canvas(): Promise<void>;
  /** Runs `asked_on_canvas` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  asked_on_canvasMany(items: AskedOnCanvasArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Uses the preset `layout`: every setting it names at once. The result is exactly that preset, so the picker shows it chosen and 'custom' is off. */
  apply(args: {
    layout: ChromePreset;
  }): Promise<void>;
  /** Runs `apply` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  applyMany(items: ApplyArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Puts the focused app's menus on the bar, after the system's: what the app in front can do now, on its resource. */
  show_app_menus(): Promise<void>;
  /** Runs `show_app_menus` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  show_app_menusMany(items: ShowAppMenusArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes the focused app's menus off the bar. */
  hide_app_menus(): Promise<void>;
  /** Runs `hide_app_menus` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  hide_app_menusMany(items: HideAppMenusArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Returns the shell to the preset it was based on, dropping the person's tuning. */
  reset(): Promise<void>;
  /** Runs `reset` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  resetMany(items: ResetArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Puts the bar on `edge`. */
  bar_on(args: {
    edge: Edge;
  }): Promise<void>;
  /** Runs `bar_on` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  bar_onMany(items: BarOnArgs[]): Promise<tilo.ActionAnswer[]>;
  set_bar_fit(args: {
    fit: Fit;
  }): Promise<void>;
  /** Runs `set_bar_fit` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_bar_fitMany(items: SetBarFitArgs[]): Promise<tilo.ActionAnswer[]>;
  set_bar_finish(args: {
    finish: Finish;
  }): Promise<void>;
  /** Runs `set_bar_finish` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_bar_finishMany(items: SetBarFinishArgs[]): Promise<tilo.ActionAnswer[]>;
  set_bar_showing(args: {
    showing: Showing;
  }): Promise<void>;
  /** Runs `set_bar_showing` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_bar_showingMany(items: SetBarShowingArgs[]): Promise<tilo.ActionAnswer[]>;
  set_bar_size(args: {
    size: Size;
  }): Promise<void>;
  /** Runs `set_bar_size` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_bar_sizeMany(items: SetBarSizeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shows or hides the top bar: the one control that turns it on and off, so a person sees whether it is on where they press it. */
  show_bar(args?: {
    on?: boolean;
  }): Promise<void>;
  /** Runs `show_bar` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  show_barMany(items: ShowBarArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Moves the dock to `edge`, keeping how it is built. */
  dock_on(args: {
    edge: Edge;
  }): Promise<void>;
  /** Runs `dock_on` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  dock_onMany(items: DockOnArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Docks the cards of what is live in the corner `corner` of the display in use. */
  dock_cards(args: {
    corner: CardCorner;
  }): Promise<void>;
  /** Runs `dock_cards` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  dock_cardsMany(items: DockCardsArgs[]): Promise<tilo.ActionAnswer[]>;
  set_dock_fit(args: {
    fit: Fit;
  }): Promise<void>;
  /** Runs `set_dock_fit` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_dock_fitMany(items: SetDockFitArgs[]): Promise<tilo.ActionAnswer[]>;
  set_dock_finish(args: {
    finish: Finish;
  }): Promise<void>;
  /** Runs `set_dock_finish` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_dock_finishMany(items: SetDockFinishArgs[]): Promise<tilo.ActionAnswer[]>;
  set_dock_showing(args: {
    showing: Showing;
  }): Promise<void>;
  /** Runs `set_dock_showing` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_dock_showingMany(items: SetDockShowingArgs[]): Promise<tilo.ActionAnswer[]>;
  set_dock_size(args: {
    size: Size;
  }): Promise<void>;
  /** Runs `set_dock_size` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_dock_sizeMany(items: SetDockSizeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shows or hides the dock: the one control that turns it on and off, so a person sees whether it is on where they press it. */
  show_dock(args?: {
    on?: boolean;
  }): Promise<void>;
  /** Runs `show_dock` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  show_dockMany(items: ShowDockArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Whether the dock shows the apps running that nobody pinned. */
  set_dock_running(args: {
    on: boolean;
  }): Promise<void>;
  /** Runs `set_dock_running` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_dock_runningMany(items: SetDockRunningArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Whether the dock has a launcher entry. */
  set_dock_launcher(args: {
    on: boolean;
  }): Promise<void>;
  /** Runs `set_dock_launcher` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_dock_launcherMany(items: SetDockLauncherArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Whether the dock magnifies the item under the pointer. */
  set_dock_zoom(args: {
    on: boolean;
  }): Promise<void>;
  /** Runs `set_dock_zoom` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_dock_zoomMany(items: SetDockZoomArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Puts a bar part with the one it comes before, wherever the part stands now: the one action a drag commits, and the same change from the settings, a shortcut or an agent. */
  move_part(args: {
    part: BarPart;
    before: BarPart;
  }): Promise<void>;
  /** Runs `move_part` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  move_partMany(items: MovePartArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Puts a bar part at `position`, counted from the leading edge: what a drag that names a place gives. */
  place_part(args: {
    part: BarPart;
    position: number;
  }): Promise<void>;
  /** Runs `place_part` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  place_partMany(items: PlacePartArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shows or hides one part of the bar. */
  show_part(args: {
    part: BarPart;
    on: boolean;
  }): Promise<void>;
  /** Runs `show_part` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  show_partMany(items: ShowPartArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Puts a dock item with the one it comes before: the one action a drag commits. */
  move_item(args: {
    item: DockItem;
    before: DockItem;
  }): Promise<void>;
  /** Runs `move_item` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  move_itemMany(items: MoveItemArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Puts a dock item at `position`, counted from the leading edge: what a drag that names a place gives. An item the person's document does not hold yet — an app running on the dock that they never pinned, which the dock drew from the shell — is pinned where it lands, so moving an app they already run keeps it on the dock after it closes. */
  place_item(args: {
    item: DockItem;
    position: number;
  }): Promise<void>;
  /** Runs `place_item` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  place_itemMany(items: PlaceItemArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Pins a resource or an app to the dock, before `next` (or at the end when no neighbour is named): what dragging one onto the dock does, and what the person does from a menu. */
  pin(args: {
    key: string;
    title: string;
    icon: string;
    next?: string;
  }): Promise<void>;
  /** Runs `pin` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  pinMany(items: PinArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes an item off the dock. */
  unpin(args: {
    item: DockItem;
  }): Promise<void>;
  /** Runs `unpin` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  unpinMany(items: UnpinArgs[]): Promise<tilo.ActionAnswer[]>;
  /** What a press on a dock item does, by the app the item stands for: a window of that app on this screen comes to the front, so the shell says which window the person is in and the top bar's commands are its own; the window is the one the item was drawn for, so a press never opens a second copy. An app with no window open is opened (`session.open_app`), and the launcher shows. It takes the app's key and not the item: an item the shell's document holds reaches an action as a document reference, and the press names the one thing it needs, so a run's item reaches it without a `DockItem` parameter to fit. */
  open_item(args: {
    key: string;
  }): Promise<void>;
  /** Start a pick: what a report about a problem is about, chosen on the whole screen. The windows stand as cards; one is picked, and the report's box opens about it. */
  report_problem(): Promise<void>;
  /** Runs `report_problem` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  report_problemMany(items: ReportProblemArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The window `key` is what the report is about: it joins the report, the picker closes, and the report's box opens about the window, the person free to point at the part of it that is wrong. */
  pick_report_window(args: {
    key: string;
  }): Promise<void>;
  /** The rectangle `area` the person dragged over the picker — its x, y, width and height in canvas pixels — picks the window it covers first, as a click on that card does. A rectangle over none of them keeps the picker up, its outline saying what was covered. */
  pick_report_region(args: {
    area: number[];
  }): Promise<void>;
  /** Leave the pick with nothing attached: no window, no report. */
  cancel_report(): Promise<void>;
  /** Runs `cancel_report` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  cancel_reportMany(items: CancelReportArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The style's own settings, and the two ways of changing a style (`docs/designs/app-language/ shell.md` §6): without an agent, in the one place a person's settings live, and with an agent, by the same actions the person's controls run. Every setting here is plain tilolang state with a default (`chrome.tl`'s `Shell` document), so a person who never touches it gets the style as it ships, and the values live in the layout's own document, synced with the person's devices, with history and undo. Each setting is one action, so an agent — the canvas conversation or the REPL — changes it with the same call a press does, attributed and undoable: `shell.set_dock_edge(shell.shell, edge: shell.Edge.left)`. The style's settings stand over the shell (`style_panel`), opened by the bar's style menu (`style_menu`, `strips.tl`): a person changes the shell they look at without leaving it, as `shell.md` §6 asks. Opens the chosen style's settings over the shell: what the bar's style menu's entry runs, and what the REPL or the canvas agent calls too. */
  open_style_settings(): Promise<void>;
  /** Runs `open_style_settings` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  open_style_settingsMany(items: OpenStyleSettingsArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Closes them. */
  close_style_settings(): Promise<void>;
  /** Runs `close_style_settings` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  close_style_settingsMany(items: CloseStyleSettingsArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shows or hides their being open: one control for both, so a person sees the state where they press it. */
  show_style_settings(args?: {
    on?: boolean;
  }): Promise<void>;
  /** Runs `show_style_settings` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  show_style_settingsMany(items: ShowStyleSettingsArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Whether the strip of several places shows its tab bar: the strips style's own setting. */
  set_strips_tabs(args: {
    on: boolean;
  }): Promise<void>;
  /** Runs `set_strips_tabs` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_strips_tabsMany(items: SetStripsTabsArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Whether a place let go on the framed canvas lands on the grid: the frames style's own setting. */
  set_frames_snap(args: {
    on: boolean;
  }): Promise<void>;
  /** Runs `set_frames_snap` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_frames_snapMany(items: SetFramesSnapArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Shell` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SHELL_ACTIONS = ["asked_on_canvas", "asked_on_canvasMany", "apply", "applyMany", "show_app_menus", "show_app_menusMany", "hide_app_menus", "hide_app_menusMany", "reset", "resetMany", "bar_on", "bar_onMany", "set_bar_fit", "set_bar_fitMany", "set_bar_finish", "set_bar_finishMany", "set_bar_showing", "set_bar_showingMany", "set_bar_size", "set_bar_sizeMany", "show_bar", "show_barMany", "dock_on", "dock_onMany", "dock_cards", "dock_cardsMany", "set_dock_fit", "set_dock_fitMany", "set_dock_finish", "set_dock_finishMany", "set_dock_showing", "set_dock_showingMany", "set_dock_size", "set_dock_sizeMany", "show_dock", "show_dockMany", "set_dock_running", "set_dock_runningMany", "set_dock_launcher", "set_dock_launcherMany", "set_dock_zoom", "set_dock_zoomMany", "move_part", "move_partMany", "place_part", "place_partMany", "show_part", "show_partMany", "move_item", "move_itemMany", "place_item", "place_itemMany", "pin", "pinMany", "unpin", "unpinMany", "open_item", "report_problem", "report_problemMany", "pick_report_window", "pick_report_region", "cancel_report", "cancel_reportMany", "open_style_settings", "open_style_settingsMany", "close_style_settings", "close_style_settingsMany", "show_style_settings", "show_style_settingsMany", "set_strips_tabs", "set_strips_tabsMany", "set_frames_snap", "set_frames_snapMany"] as const;

/** The `tl.so.tilo.shell.Shell` kind at `path`. */
export function shell(ws: tilo.Workspace, path: string): Shell {
  return resourceProxy(ws, path, SURFACE, SHELL_ACTIONS) as unknown as Shell;
}

/** BarPart — `tl.so.tilo.shell.BarPart`. */
export interface BarPart extends tilo.Resource {
  fjzbw5un: BarPartKind;  // kind
  pbly7ymy: boolean;  // shown
}

/** The actions `BarPart` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const BARPART_ACTIONS = [] as const;

/** The `tl.so.tilo.shell.BarPart` kind at `path`. */
export function barPart(ws: tilo.Workspace, path: string): BarPart {
  return resourceProxy(ws, path, SURFACE, BARPART_ACTIONS) as unknown as BarPart;
}

/** DockItem — `tl.so.tilo.shell.DockItem`. */
export interface DockItem extends tilo.Resource {
  rfsefuzv: string;  // key
  qsnrl2gk: string;  // title
  "6gyb7csc": string;  // icon
  k376lylv: boolean;  // pinned
  "5vpgvzqb": boolean;  // running
  mgcmvxjx: number;  // badge
  e7ft4mly: boolean;  // attention
}

/** The actions `DockItem` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DOCKITEM_ACTIONS = [] as const;

/** The `tl.so.tilo.shell.DockItem` kind at `path`. */
export function dockItem(ws: tilo.Workspace, path: string): DockItem {
  return resourceProxy(ws, path, SURFACE, DOCKITEM_ACTIONS) as unknown as DockItem;
}

/** ReportPick — `tl.so.tilo.shell.ReportPick`. */
export interface ReportPick extends tilo.Resource {
  hb2dxuji: string;  // key
  hu4bokd4: string;  // title
  abe7lbj4: string;  // app_name
}

/** The actions `ReportPick` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const REPORTPICK_ACTIONS = [] as const;

/** The `tl.so.tilo.shell.ReportPick` kind at `path`. */
export function reportPick(ws: tilo.Workspace, path: string): ReportPick {
  return resourceProxy(ws, path, SURFACE, REPORTPICK_ACTIONS) as unknown as ReportPick;
}

/** Layout — `tl.so.tilo.shell.Layout`. */
export interface Layout extends tilo.Resource {
  uykuo63i: string;  // person
  k4p3z4ee: string;  // front
  yb7sokzs: Array<tilo.Entry<LayoutPerson>>;  // people
  "7n7jrcg3": Array<tilo.Entry<LayoutWorkspace>>;  // workspaces
  hqul2vwj: Array<tilo.Entry<LayoutRow>>;  // rows
  "7lq6xkzz": Array<tilo.Entry<LayoutStrip>>;  // strips
  "2ug5q5cq": Array<tilo.Entry<LayoutPlace>>;  // places
  dt4vnrqa: Array<tilo.Entry<LayoutQuery>>;  // queries
  fqirvfzw: Array<tilo.Entry<LayoutRef>>;  // refs
  cswol6ca: Array<tilo.Entry<LayoutRecent>>;  // recents
  /** Adds a workspace, with one row, to `a`, and returns the row's key. */
  a_workspace(args: {
    name: string;
  }): Promise<string>;
  /** Adds the workspace in front, with its row in front, to `a`, and returns the row's key. */
  a_front_workspace(args: {
    name: string;
  }): Promise<string>;
  /** Adds a strip named `name` to `row` at `width`, and returns its key. */
  a_strip(args: {
    row: string;
    name: string;
    width: number;
  }): Promise<string>;
  /** Adds a strip named `name` to `row` at `width`, shown on `display`, and returns its key. */
  a_strip_on(args: {
    row: string;
    name: string;
    width: number;
    display: number;
  }): Promise<string>;
  /** Adds a place showing the note at `index` of the notebook to `strip`, and returns its key. */
  a_note_place(args: {
    strip: string;
    index: number;
  }): Promise<string>;
  /** A place of a kind this build has no document for: it stands as it is, which is what a place another shell made looks like. */
  a_foreign_place(args: {
    strip: string;
    index: number;
    title: string;
  }): Promise<string>;
  /** Adds a place showing a search for `text` to `strip`, with the two matches the notes app answered, and returns the place's key: the state a place stands in until the person chooses one of the resources it found (shell.md §8). */
  a_query_place(args: {
    strip: string;
    text: string;
  }): Promise<string>;
  /** Ada's opens of an afternoon and two of Mia's, newest first, as the session lists them. */
  recents_of_an_afternoon(): Promise<void>;
  /** Runs `recents_of_an_afternoon` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  recents_of_an_afternoonMany(items: RecentsOfAnAfternoonArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Tab the place whose window's bar was dragged onto the strip `s` into that strip: a person drags one window's bar onto another's to make its tabs, and the window's menu's 'Move into … as a tab' runs the same arranging action. The place lands after the strip's other tabs and comes to the front of them, so the window shows what was dropped on it. A place already in this strip is only brought to the front, so dropping a window on itself raises it rather than opening a second place. The strip comes forward with it. */
  tab_place_here(args: {
    s: LayoutStrip;
    target: unknown;
  }): Promise<void>;
  /** Runs `tab_place_here` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  tab_place_hereMany(items: TabPlaceHereArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Rank the place `place` at `at` among its strip's tabs: what dragging a pill within the window's bar does, as the shell's own `move` with the place it goes before. The strip reports the pill's place among the tabs with the moved pill taken out, so the tab it lands before is that index of the others; the last place goes last. */
  reorder_place(args: {
    s: LayoutStrip;
    place: unknown;
    at: number;
  }): Promise<void>;
  /** Runs `reorder_place` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  reorder_placeMany(items: ReorderPlaceArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Brings the window of the strip `s` to the front, the keyboard in it: the session brings its place in front forward, as it does for a press, a script or an agent alike. A strip with no place has no window to bring. */
  bring_forward(args: {
    s: LayoutStrip;
  }): Promise<void>;
  /** Runs `bring_forward` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  bring_forwardMany(items: BringForwardArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Layout` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LAYOUT_ACTIONS = ["a_workspace", "a_front_workspace", "a_strip", "a_strip_on", "a_note_place", "a_foreign_place", "a_query_place", "recents_of_an_afternoon", "recents_of_an_afternoonMany", "tab_place_here", "tab_place_hereMany", "reorder_place", "reorder_placeMany", "bring_forward", "bring_forwardMany"] as const;

/** The `tl.so.tilo.shell.Layout` kind at `path`. */
export function layout(ws: tilo.Workspace, path: string): Layout {
  return resourceProxy(ws, path, SURFACE, LAYOUT_ACTIONS) as unknown as Layout;
}

/** LayoutRecent — `tl.so.tilo.shell.LayoutRecent`. */
export interface LayoutRecent extends tilo.Resource {
  zyqmwz53: string;  // key
  g5lvltn4: string;  // resource
  "33ygyyvo": string;  // title
  rgnzsrl5: string;  // origin_app
  qegnrgvk: string;  // app_title
  wpv5ccaw: string;  // kind
  nk3qn5vg: string;  // by
  n77p6zrw: string;  // at
  "5xciib7g": boolean;  // mine
}

/** The actions `LayoutRecent` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LAYOUTRECENT_ACTIONS = [] as const;

/** The `tl.so.tilo.shell.LayoutRecent` kind at `path`. */
export function layoutRecent(ws: tilo.Workspace, path: string): LayoutRecent {
  return resourceProxy(ws, path, SURFACE, LAYOUTRECENT_ACTIONS) as unknown as LayoutRecent;
}

/** LayoutQuery — `tl.so.tilo.shell.LayoutQuery`. */
export interface LayoutQuery extends tilo.Resource {
  "6jget5cg": string;  // key
  iwuvfj66: string;  // text
  ybpst64r: string;  // in_app
  ibasoiis: string;  // kind
  a6ewjjom: string;  // state
  xsjcdedm: string;  // reason
}

/** The actions `LayoutQuery` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LAYOUTQUERY_ACTIONS = [] as const;

/** The `tl.so.tilo.shell.LayoutQuery` kind at `path`. */
export function layoutQuery(ws: tilo.Workspace, path: string): LayoutQuery {
  return resourceProxy(ws, path, SURFACE, LAYOUTQUERY_ACTIONS) as unknown as LayoutQuery;
}

/** LayoutRef — `tl.so.tilo.shell.LayoutRef`. */
export interface LayoutRef extends tilo.Resource {
  k652vouu: string;  // key
  vdsinb5l: string;  // query
  vj5mgxi6: string;  // address
  pnom4n55: string;  // title
  "7giiatvv": string;  // kind
}

/** The actions `LayoutRef` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LAYOUTREF_ACTIONS = [] as const;

/** The `tl.so.tilo.shell.LayoutRef` kind at `path`. */
export function layoutRef(ws: tilo.Workspace, path: string): LayoutRef {
  return resourceProxy(ws, path, SURFACE, LAYOUTREF_ACTIONS) as unknown as LayoutRef;
}

/** LayoutWorkspace — `tl.so.tilo.shell.LayoutWorkspace`. */
export interface LayoutWorkspace extends tilo.Resource {
  "5zibpqs6": string;  // key
  b2vzzczj: string;  // name
  hwfwvs3f: boolean;  // current
  jbtiqjt4: string;  // home
}

/** The actions `LayoutWorkspace` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LAYOUTWORKSPACE_ACTIONS = [] as const;

/** The `tl.so.tilo.shell.LayoutWorkspace` kind at `path`. */
export function layoutWorkspace(ws: tilo.Workspace, path: string): LayoutWorkspace {
  return resourceProxy(ws, path, SURFACE, LAYOUTWORKSPACE_ACTIONS) as unknown as LayoutWorkspace;
}

/** LayoutPerson — `tl.so.tilo.shell.LayoutPerson`. */
export interface LayoutPerson extends tilo.Resource {
  "3rpmhtjf": string;  // key
  "6a7urcxy": string;  // name
}

/** The actions `LayoutPerson` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LAYOUTPERSON_ACTIONS = [] as const;

/** The `tl.so.tilo.shell.LayoutPerson` kind at `path`. */
export function layoutPerson(ws: tilo.Workspace, path: string): LayoutPerson {
  return resourceProxy(ws, path, SURFACE, LAYOUTPERSON_ACTIONS) as unknown as LayoutPerson;
}

/** LayoutRow — `tl.so.tilo.shell.LayoutRow`. */
export interface LayoutRow extends tilo.Resource {
  "3xc5xbon": string;  // key
  v5ef7tkv: string;  // workspace
  iqgu3ggi: string;  // name
  sm4scdu2: boolean;  // current
  bowkspu3: string;  // shell
}

/** The actions `LayoutRow` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LAYOUTROW_ACTIONS = [] as const;

/** The `tl.so.tilo.shell.LayoutRow` kind at `path`. */
export function layoutRow(ws: tilo.Workspace, path: string): LayoutRow {
  return resourceProxy(ws, path, SURFACE, LAYOUTROW_ACTIONS) as unknown as LayoutRow;
}

/** LayoutStrip — `tl.so.tilo.shell.LayoutStrip`. */
export interface LayoutStrip extends tilo.Resource {
  dufcke3u: string;  // key
  x42tiiqe: string;  // workspace
  dvberkpa: string;  // row
  ssbg6dg6: number;  // display
  "7ekq6qgi": number;  // width
  "2cbikm5m": boolean;  // maximized
  yk2oxjrj: string;  // active
  "44lq6lon": boolean;  // placed
  vbv2zrob: number;  // canvas_x
  jvrwsckz: number;  // canvas_y
  m75kjoue: number;  // canvas_width
  s3lpes2e: number;  // canvas_height
  s6rivl3a: boolean;  // canvas_hugs
  pznsxbgq: string;  // shell
}

/** The actions `LayoutStrip` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LAYOUTSTRIP_ACTIONS = [] as const;

/** The `tl.so.tilo.shell.LayoutStrip` kind at `path`. */
export function layoutStrip(ws: tilo.Workspace, path: string): LayoutStrip {
  return resourceProxy(ws, path, SURFACE, LAYOUTSTRIP_ACTIONS) as unknown as LayoutStrip;
}

/** LayoutPlace — `tl.so.tilo.shell.LayoutPlace`. */
export interface LayoutPlace extends tilo.Resource {
  ba3kyokq: string;  // key
  "7dno3n47": string;  // strip
  jpzk4cli: string;  // shows
  nprpusuo: string;  // shown_as
  ofkshr4v: string;  // resource
  qqcejiim: string;  // title
  jeatudax: string;  // name
  vkbhstef: boolean;  // pinned
  soy4vimv: string;  // shell
}

/** The actions `LayoutPlace` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LAYOUTPLACE_ACTIONS = [] as const;

/** The `tl.so.tilo.shell.LayoutPlace` kind at `path`. */
export function layoutPlace(ws: tilo.Workspace, path: string): LayoutPlace {
  return resourceProxy(ws, path, SURFACE, LAYOUTPLACE_ACTIONS) as unknown as LayoutPlace;
}

/** Viewing — `tl.so.tilo.shell.Viewing`. */
export interface Viewing extends tilo.Resource {
  kna5sco4: string;  // style
  /** Shows the shell through the style `style`: a package and a view (`so.tilo.shell:strips`). Switching keeps the layout: the same places stand where they were, drawn another way. */
  show_style(args: {
    style: string;
  }): Promise<void>;
  /** Runs `show_style` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  show_styleMany(items: ShowStyleArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Viewing` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const VIEWING_ACTIONS = ["show_style", "show_styleMany"] as const;

/** The `tl.so.tilo.shell.Viewing` kind at `path`. */
export function viewing(ws: tilo.Workspace, path: string): Viewing {
  return resourceProxy(ws, path, SURFACE, VIEWING_ACTIONS) as unknown as Viewing;
}

/** Shell — `tl.so.tilo.shell`. */
export interface TiloShell extends tilo.Resource {
  /** Name the workspace `key` what the person typed: the workspace's own published `rename`, so the name reaches every device and every list of it. A blank name is nothing to do. */
  rename_workspace(args: {
    key: string;
    name: string;
  }): Promise<void>;
  /** Name the strip `key` what the person typed: one arranging action of the session's, so a script and an agent rename as a press does. A blank name is nothing to do. */
  rename_strip(args: {
    key: string;
    name: string;
  }): Promise<void>;
  /** Decline the question at `key`: its asked record's own `resolve`, called by its address as any caller calls it. */
  decline(args: {
    key: string;
  }): Promise<void>;
  /** Gives the place `key` the size a drag of its edge or corner left it at, `size` its width and height in canvas pixels: one arranging step of the person's, its corner where it stood. */
  size_place(args: {
    key: string;
    size: number[];
  }): Promise<void>;
  /** Runs `size_place` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  size_placeMany(items: SizePlaceArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Keeps how tall the hugging place `key` is drawn now, `size` carrying its height in canvas pixels as the map lays it out: the free area under it on the canvas is what the person sees there (canvas.md §1, §7), though the place could grow into it. View state the person's own canvas reports, never a step of arranging: Undo leaves it alone. */
  drew_place(args: {
    key: string;
    size: number[];
  }): Promise<void>;
  /** Puts a resource dropped on the canvas where it was let go. A place that already stands on the canvas — the place whose handle was dragged — moves to the point it was let go at, through its own `position` action, so a drag that ends on the canvas moves the place and never stands a second one. Any other resource — a message's attachment, a search hit, a row of another view — is placed at that point through the system's own `place`, which is what an agent's `place_at` runs. Either is one arranging step of the person's, attributed and undoable. */
  place_dropped(args: {
    target: unknown;
    at: number[];
  }): Promise<void>;
  /** Runs `place_dropped` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  place_droppedMany(items: PlaceDroppedArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Puts the surface away once a recent resource is opened again beside the window in front, by the same `open` an agent and a reference's press run. */
  put_history_away(): Promise<void>;
  /** Steps the value of the row `step` names (`1` up, `-1` down), as Left and Right do on an adjustable row: the volume or the brightness in front, by a twentieth. */
  step_level(args: {
    step: string;
  }): Promise<void>;
  /** What choosing a row with a modifier held does: a row the search found opens the way `here` (a tab in front), `beside` (a window beside it) or `more` (the row's own way) names, and every other row opens as it does plainly. */
  choose_launcher_row_as(args: {
    packed: string;
    current: boolean;
  }): Promise<void>;
  /** What choosing the row the person reached does, read off the row's value: opening an application or a window here, running a row the search found, or one of the shell's own surfaces. A row is only ever one of those, so a raw choice — what is typed with no row under it — opens the first row the text finds, or searches everywhere for it. */
  choose_launcher_row(args: {
    choice: string;
    current: boolean;
  }): Promise<void>;
  /** Open the shell's surface `surface` (`overview`, `workspaces`, `centre`), as the bar's own controls do. */
  open_surface(args: {
    surface: string;
  }): Promise<void>;
  /** Leave the launcher: the surface stack goes back to the shell at rest. */
  close_launcher(): Promise<void>;
  /** Turn the VPN on through its first profile, or off through the one connected: the tile's one press, whichever way it flips. */
  toggle_vpn(args: {
    v: unknown;
  }): Promise<void>;
  /** A shell of one note in the style named, with that place in front, and the style's own settings open over it: what a person sees who opens them from the bar's style menu. */
  a_style_settings(args: {
    style: string;
  }): Promise<Layout>;
  /** Show `found` in the place `place` while it shows a query: the same place, now the found resource (shell.md §8). */
  query_show(args: {
    place: string;
    found: unknown;
  }): Promise<void>;
  /** Runs `query_show` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  query_showMany(items: QueryShowArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Look for what the place `place`'s search names again. */
  query_retry(args: {
    place: string;
  }): Promise<void>;
  /** Runs `query_retry` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  query_retryMany(items: QueryRetryArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Give the place `place` a window of its own: what dragging a tab out of its window's bar does, as the shell's own `split-right` for the window in front. The place stands in front of its new window, and that window comes to the front of the shell. */
  detach_place(args: {
    place: unknown;
  }): Promise<void>;
  /** Shows the place `place` and brings its window forward with it, the keyboard in it: what choosing a tab does, and the same arranging action a script or an agent runs. */
  show_place(args: {
    place: unknown;
  }): Promise<void>;
  /** Runs `show_place` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  show_placeMany(items: ShowPlaceArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Move the window `w` before the window `before` of its strip: one place along it. */
  move_before(args: {
    w: unknown;
    before: string;
  }): Promise<void>;
  /** Move the window `w` one place to the trailing side: before the window after the next, or last in its strip while none stands there. */
  move_right(args: {
    w: unknown;
  }): Promise<void>;
  /** Give every window of the row the same share of it: what 'every window the same' does where a person sees their windows side by side in a band. */
  equal_widths(args: {
    w: unknown;
  }): Promise<void>;
  /** Runs `equal_widths` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  equal_widthsMany(items: EqualWidthsArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Puts the widget `kind` where a drag let go, at `at` on this display: the place the person left it at, as fractions of the room it can move in there. */
  place_widget(args: {
    kind: WidgetKind;
    at: number[];
    room: number[];
    display: number;
  }): Promise<void>;
  /** Runs `place_widget` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  place_widgetMany(items: PlaceWidgetArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `TiloShell` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TILOSHELL_ACTIONS = ["rename_workspace", "rename_strip", "decline", "size_place", "size_placeMany", "drew_place", "place_dropped", "place_droppedMany", "put_history_away", "step_level", "choose_launcher_row_as", "choose_launcher_row", "open_surface", "close_launcher", "toggle_vpn", "a_style_settings", "query_show", "query_showMany", "query_retry", "query_retryMany", "detach_place", "show_place", "show_placeMany", "move_before", "move_right", "equal_widths", "equal_widthsMany", "place_widget", "place_widgetMany"] as const;

/** The `tl.so.tilo.shell` kind at `path`. */
export function tiloShell(ws: tilo.Workspace, path: string): TiloShell {
  return resourceProxy(ws, path, SURFACE, TILOSHELL_ACTIONS) as unknown as TiloShell;
}

