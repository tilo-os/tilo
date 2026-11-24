/**
 * Types for App Store (`so.tilo.store`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Notice` is a `Resource` of the app's `tl.tilo.host.Notice`, made by its factory with the
 * resource's path (`store/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.store@b68a7236639baac4";

/** The arguments of `get`. */
export interface GetArgs {
}

/** The arguments of `update`. */
export interface UpdateArgs {
}

/** The arguments of `install_runtime`. */
export interface InstallRuntimeArgs {
}

/** The arguments of `pin`. */
export interface PinArgs {
}

/** The arguments of `unpin`. */
export interface UnpinArgs {
}

/** The arguments of `cancel_change`. */
export interface CancelChangeArgs {
}

/** The arguments of `submit_report`. */
export interface SubmitReportArgs {
}

/** The arguments of `appeal`. */
export interface AppealArgs {
}

/** The arguments of `poll`. */
export interface PollArgs {
}

/** The arguments of `follow`. */
export interface FollowArgs {
  a: App;
}

/** The arguments of `begin_reading`. */
export interface BeginReadingArgs {
  op: string;
  ask: string;
}

/** The arguments of `begin`. */
export interface BeginArgs {
  a: App;
  op: string;
  ask: string;
}

/** The arguments of `read_app`. */
export interface ReadAppArgs {
  a: App;
}

/** The arguments of `show_details`. */
export interface ShowDetailsArgs {
  a: App;
}

/** The arguments of `take_catalogue`. */
export interface TakeCatalogueArgs {
  status: string;
}

/** The arguments of `read_flathub`. */
export interface ReadFlathubArgs {
}

/** The arguments of `read_flathub_details`. */
export interface ReadFlathubDetailsArgs {
  a: App;
}

/** The arguments of `absorb_remote_text`. */
export interface AbsorbRemoteTextArgs {
  text: string;
}

/** The arguments of `refresh_remotes`. */
export interface RefreshRemotesArgs {
}

/** The arguments of `add_remote`. */
export interface AddRemoteArgs {
  name: string;
  url: string;
}

/** The arguments of `enable_remote`. */
export interface EnableRemoteArgs {
  r: Remote;
}

/** The arguments of `disable_remote`. */
export interface DisableRemoteArgs {
  r: Remote;
}

/** The arguments of `remote_change`. */
export interface RemoteChangeArgs {
  r: Remote;
  enabled: boolean;
}

/** The arguments of `remove_remote`. */
export interface RemoveRemoteArgs {
  r: Remote;
}

/** The arguments of `browse`. */
export interface BrowseArgs {
  r: Remote;
}

/** The arguments of `update_all`. */
export interface UpdateAllArgs {
}

/** The arguments of `add_bot`. */
export interface AddBotArgs {
  a: App;
}

/** The arguments of `block_app`. */
export interface BlockAppArgs {
  a: App;
}

/** The arguments of `unblock_app`. */
export interface UnblockAppArgs {
  a: App;
}

/** The arguments of `install_reviewed`. */
export interface InstallReviewedArgs {
}

/** The arguments of `open_reference`. */
export interface OpenReferenceArgs {
  file: unknown;
}

/** The arguments of `record_moderation_case`. */
export interface RecordModerationCaseArgs {
  identity: string;
  case: string;
  status: string;
}

/** The arguments of `refresh_status`. */
export interface RefreshStatusArgs {
  r: Report;
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

/** Version — `tl.so.tilo.store.Version`. */
export interface Version extends tilo.Resource {
  tv4yjmd7: string;  // version
  "2jsas2xh": string;  // released
  xzuri67c: string;  // root
  pryqr455: string;  // surface
  j7j23dzx: number;  // size
  t73ddwug: string;  // runtime
  a6avsz6m: string;  // download
}

/** The actions `Version` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const VERSION_ACTIONS = [] as const;

/** The `tl.so.tilo.store.Version` kind at `path`. */
export function version(ws: tilo.Workspace, path: string): Version {
  return resourceProxy(ws, path, SURFACE, VERSION_ACTIONS) as unknown as Version;
}

/** Permission — `tl.so.tilo.store.Permission`. */
export interface Permission extends tilo.Resource {
  yrcnogu3: string;  // what
  znsz4dgp: boolean;  // broad
}

/** The actions `Permission` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PERMISSION_ACTIONS = [] as const;

/** The `tl.so.tilo.store.Permission` kind at `path`. */
export function permission(ws: tilo.Workspace, path: string): Permission {
  return resourceProxy(ws, path, SURFACE, PERMISSION_ACTIONS) as unknown as Permission;
}

/** Release — `tl.so.tilo.store.Release`. */
export interface Release extends tilo.Resource {
  dbblppxc: string;  // version
  sil2634m: string;  // notes
}

/** The actions `Release` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const RELEASE_ACTIONS = [] as const;

/** The `tl.so.tilo.store.Release` kind at `path`. */
export function release(ws: tilo.Workspace, path: string): Release {
  return resourceProxy(ws, path, SURFACE, RELEASE_ACTIONS) as unknown as Release;
}

/** App — `tl.so.tilo.store.App`. */
export interface App extends tilo.Resource {
  bsz5kepr: string;  // identity
  z7s7wy7i: string;  // name
  "7mugzbgk": string;  // summary
  xmhc7rzm: string;  // description
  "6igcsadc": string;  // developer
  y6tadyrs: string;  // license
  s2h3qmpk: string[];  // categories
  z2ymswmt: string;  // icon
  fbg5gcf6: string[];  // screenshots
  iuswhjxs: string;  // homepage
  dmpicgut: string;  // source_code
  dyqo2u55: string;  // origin
  gkbbivsc: string;  // registry
  febktfoe: string;  // branch
  qay7cvdi: boolean;  // user_managed
  hptnn5eg: string;  // trust
  kqqhh6as: Array<tilo.Entry<Version>>;  // versions
  sx2epvkk: string;  // available
  rssj2htx: string;  // installed
  qqimyoka: boolean;  // pinned
  ev27rmap: string;  // fetched
  lmny564e: boolean;  // runnable
  "54f6mqrf": string;  // processors
  qbk2t2ii: boolean;  // awaiting_developer
  fmc644bz: number;  // featured
  "5vq3mjxe": string;  // updated
  aeiz77yf: string;  // publisher
  h6fnaygl: string;  // digest
  "7tdammi6": boolean;  // bot
  rnazc2e5: string[];  // bot_triggers
  su65i54b: string[];  // bot_apps
  "4qzdclse": string[];  // bot_connectors
  ckhofqel: boolean;  // bot_appearance
  ehbiqgtb: Array<tilo.Entry<Permission>>;  // permissions
  foea7x37: Array<tilo.Entry<Release>>;  // releases
  fngfv6n4: number;  // size
  cifjx7ck: number;  // installed_size
  w626nsxj: string[];  // languages
  "25c7mbdv": string;  // reference
  tdkfnlcz: string;  // runtime
  qnnzjwd4: string;  // runtime_registry
  babs5eju: string;  // job
  "66hvm5fy": boolean;  // details_read
  "7l5m7ka6": boolean;  // updatable
  awr7kqfn: string;  // trouble
  /** Gets an app the workspace does not run yet: the newest version its source offers. */
  get(): Promise<void>;
  /** Runs `get` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  getMany(items: GetArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Updates an app the workspace runs to the newest version its source offers. */
  update(): Promise<void>;
  /** Runs `update` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  updateMany(items: UpdateArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Installs the runtime an app needs, once its page reviewed the exact signed offer. */
  install_runtime(): Promise<void>;
  /** Runs `install_runtime` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  install_runtimeMany(items: InstallRuntimeArgs[]): Promise<tilo.ActionAnswer[]>;
  // the app's `remove` action is the SDK's own: run it with `action("remove")`.
  /** Keeps this workspace's version of an app, so it is never cleaned up when unused. */
  pin(): Promise<void>;
  /** Runs `pin` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  pinMany(items: PinArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Lets this workspace's unused version of an app be cleaned up again. */
  unpin(): Promise<void>;
  /** Runs `unpin` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  unpinMany(items: UnpinArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Stops the change running for an app, keeping what it did until then. */
  cancel_change(): Promise<void>;
  /** Runs `cancel_change` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  cancel_changeMany(items: CancelChangeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Opens an app this computer has, fetching it first when the workspace runs it and this computer has not. */
  open_app(): Promise<void>;
  /** The action a page's one button runs, by the name the catalogue decided. */
  apply_action(args: {
    verb: string;
  }): Promise<void>;
}

/** The actions `App` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const APP_ACTIONS = ["get", "getMany", "update", "updateMany", "install_runtime", "install_runtimeMany", "pin", "pinMany", "unpin", "unpinMany", "cancel_change", "cancel_changeMany", "open_app", "apply_action"] as const;

/** The `tl.so.tilo.store.App` kind at `path`. */
export function app(ws: tilo.Workspace, path: string): App {
  return resourceProxy(ws, path, SURFACE, APP_ACTIONS) as unknown as App;
}

/** Remote — `tl.so.tilo.store.Remote`. */
export interface Remote extends tilo.Resource {
  q6znklmy: string;  // name
  e74rqhdi: string;  // title
  yimcob4p: string;  // url
  "7o2l4sfb": string;  // scope
  gbitboxx: boolean;  // enabled
  ixnjbq5u: string;  // trouble
  mdalqw5g: number;  // count
  pag3getc: boolean;  // loaded
}

/** The actions `Remote` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const REMOTE_ACTIONS = [] as const;

/** The `tl.so.tilo.store.Remote` kind at `path`. */
export function remote(ws: tilo.Workspace, path: string): Remote {
  return resourceProxy(ws, path, SURFACE, REMOTE_ACTIONS) as unknown as Remote;
}

/** Block — `tl.so.tilo.store.Block`. */
export interface Block extends tilo.Resource {
  qexvvrxm: string;  // identity
}

/** The actions `Block` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const BLOCK_ACTIONS = [] as const;

/** The `tl.so.tilo.store.Block` kind at `path`. */
export function block(ws: tilo.Workspace, path: string): Block {
  return resourceProxy(ws, path, SURFACE, BLOCK_ACTIONS) as unknown as Block;
}

/** Report — `tl.so.tilo.store.Report`. */
export interface Report extends tilo.Resource {
  i2w5sqnt: string;  // identity
  tmhftkva: string;  // note
  riwxceey: string;  // status
  edrxm3vo: string;  // case
  sv5euoyy: string;  // operation
  sez3dgh3: string;  // registry
  /** Signs a report with the workspace's identity and sends it to the app's repository. */
  submit_report(): Promise<void>;
  /** Runs `submit_report` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  submit_reportMany(items: SubmitReportArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Appeals a decision about an app. */
  appeal(): Promise<void>;
  /** Runs `appeal` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  appealMany(items: AppealArgs[]): Promise<tilo.ActionAnswer[]>;
  /** A report's evidence as the file a person saves and sends with it. */
  report_file(): Promise<unknown>;
}

/** The actions `Report` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const REPORT_ACTIONS = ["submit_report", "submit_reportMany", "appeal", "appealMany", "report_file"] as const;

/** The `tl.so.tilo.store.Report` kind at `path`. */
export function report(ws: tilo.Workspace, path: string): Report {
  return resourceProxy(ws, path, SURFACE, REPORT_ACTIONS) as unknown as Report;
}

/** Store — `tl.so.tilo.store.Store`. */
export interface Store extends tilo.Resource {
  t6lkc6fh: Array<tilo.Entry<App>>;  // apps
  gvveyw3n: Array<tilo.Entry<Remote>>;  // remotes
  "5g4uijlp": Array<tilo.Entry<Report>>;  // reports
  a6ibqgbw: Array<tilo.Entry<Block>>;  // blocks
  "72gq3jue": string;  // status
  sotfrvum: string;  // trouble
  eubzl4qx: string;  // reading
  e22zgwb6: boolean;  // flatpak
  uxwwy2fl: string;  // flatpak_trouble
  yut35juc: string[];  // unanswered
  y32wfrks: string[];  // expired
  "452zgqdl": string;  // reviewed_root
  "5ynamzdl": string;  // reviewed_app
  vihbdrl7: unknown | null;  // reviewed_file
  // the app's `refresh` action is the SDK's own: run it with `action("refresh")`.
  /** Asks how far the read or the change running has got, and puts what it found into the workspace. Runs while the window is open. */
  poll(): Promise<void>;
  /** Runs `poll` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  pollMany(items: PollArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The job an app's change is, as far as the engine has got: its row says where it stands, and what it made is read once it ends. */
  follow(args: {
    a: App;
  }): Promise<void>;
  /** Runs `follow` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  followMany(items: FollowArgs[]): Promise<tilo.ActionAnswer[]>;
  /** A job that changes the catalogue itself rather than one app, followed as the read is: its end reads the sources again, so a new repository or an update of everything lands. */
  begin_reading(args: {
    op: string;
    ask: string;
  }): Promise<void>;
  /** Runs `begin_reading` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  begin_readingMany(items: BeginReadingArgs[]): Promise<tilo.ActionAnswer[]>;
  /** A job the engine started for `a`, or why it did not start. */
  begin(args: {
    a: App;
    op: string;
    ask: string;
  }): Promise<void>;
  /** Runs `begin` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  beginMany(items: BeginArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Reads one app as it stands now: what the workspace runs, what the computer fetched, and the newest offer its source makes. */
  read_app(args: {
    a: App;
  }): Promise<void>;
  /** Runs `read_app` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  read_appMany(items: ReadAppArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Reads one app's page: what its source says about it. */
  show_details(args: {
    a: App;
  }): Promise<void>;
  /** Runs `show_details` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  show_detailsMany(items: ShowDetailsArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The catalogue one read found, put into the workspace: apps in the order the sources gave them, each replaced where it stands, and the ones its sources stopped listing taken out. */
  take_catalogue(args: {
    status: string;
  }): Promise<void>;
  /** Runs `take_catalogue` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  take_catalogueMany(items: TakeCatalogueArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Reads Flathub's catalogue over its own search API, a page at a time, and keeps every app beside the registry's. A person's language names the rows, as every other source's does. */
  read_flathub(): Promise<void>;
  /** Runs `read_flathub` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  read_flathubMany(items: ReadFlathubArgs[]): Promise<tilo.ActionAnswer[]>;
  /** What one Flathub app's page reads: its own AppStream document and what its build asks for. */
  read_flathub_details(args: {
    a: App;
  }): Promise<void>;
  /** Runs `read_flathub_details` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  read_flathub_detailsMany(items: ReadFlathubDetailsArgs[]): Promise<tilo.ActionAnswer[]>;
  /** One app as a source wrote it, read and kept where it stands: an app this workspace already knew keeps what the person looked at of it. */
  absorb_text(args: {
    text: string;
  }): Promise<App | null>;
  /** One repository as a source wrote it, kept where it stands. */
  absorb_remote_text(args: {
    text: string;
  }): Promise<void>;
  /** Runs `absorb_remote_text` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  absorb_remote_textMany(items: AbsorbRemoteTextArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Reads this computer's Flatpak repositories again. */
  refresh_remotes(): Promise<void>;
  /** Runs `refresh_remotes` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  refresh_remotesMany(items: RefreshRemotesArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Adds a Flatpak repository to this workspace. */
  add_remote(args: {
    name: string;
    url: string;
  }): Promise<void>;
  /** Runs `add_remote` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  add_remoteMany(items: AddRemoteArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Enables one of this workspace's repositories. */
  enable_remote(args: {
    r: Remote;
  }): Promise<void>;
  /** Runs `enable_remote` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  enable_remoteMany(items: EnableRemoteArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Disables one of this workspace's repositories. */
  disable_remote(args: {
    r: Remote;
  }): Promise<void>;
  /** Runs `disable_remote` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  disable_remoteMany(items: DisableRemoteArgs[]): Promise<tilo.ActionAnswer[]>;
  remote_change(args: {
    r: Remote;
    enabled: boolean;
  }): Promise<void>;
  /** Runs `remote_change` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remote_changeMany(items: RemoteChangeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Removes one of this workspace's repositories, and its apps from the catalogue. */
  remove_remote(args: {
    r: Remote;
  }): Promise<void>;
  /** Runs `remove_remote` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remove_remoteMany(items: RemoveRemoteArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Browses one repository's apps, through the system Flatpak metadata on this computer. */
  browse(args: {
    r: Remote;
  }): Promise<void>;
  /** Runs `browse` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  browseMany(items: BrowseArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Updates every app the workspace runs and its sources offer a newer version of. */
  update_all(): Promise<void>;
  /** Runs `update_all` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  update_allMany(items: UpdateAllArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Adds a directory bot to this workspace as the person's own shortcut: its program is verified against the signed listing and saved as the shortcut the workspace's host runs for them, under the triggers the program declares. */
  add_bot(args: {
    a: App;
  }): Promise<void>;
  /** Runs `add_bot` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  add_botMany(items: AddBotArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Blocks an app: it then appears nowhere in the Store or in search, and cannot be opened or installed from them. Only the person who blocked it can unblock it. */
  block_app(args: {
    a: App;
  }): Promise<void>;
  /** Runs `block_app` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  block_appMany(items: BlockAppArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Unblocks an app this person blocked. A device's or a workspace's block stays. */
  unblock_app(args: {
    a: App;
  }): Promise<void>;
  /** Runs `unblock_app` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  unblock_appMany(items: UnblockAppArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Reviews a package file someone hands the Store. Opening one never installs it: its identity must match a verified offer, or an attestor this computer trusts. */
  import_package(args: {
    file: unknown;
  }): Promise<unknown>;
  /** Installs the package someone just reviewed, after the person read its details. */
  install_reviewed(): Promise<void>;
  /** Runs `install_reviewed` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  install_reviewedMany(items: InstallReviewedArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Opens a `.flatpakref`: it joins the catalogue as a card, and installing it stays a separate step the person takes. Opens a `.flatpakref`: a repository's app, which the source installs itself. The package review reaches it through `import_package`, which opens both kinds of file. */
  open_reference(args: {
    file: unknown;
  }): Promise<void>;
  /** Runs `open_reference` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  open_referenceMany(items: OpenReferenceArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Opens a `tilo-store:` link: the app (`tilo-store:app/<identity>`), or runtime (`tilo-store:runtime/<identity>`) it names, or the catalogue. A shell hands a link here whether or not a window of the Store is open, and what this returns opens in the window; a link this catalogue holds nothing for shows the catalogue with why. */
  open_link(args: {
    link: string;
  }): Promise<unknown>;
  /** Reads a runtime's exact signed offer, so its page says what installing it would run. */
  review_runtime(args: {
    runtime: string;
    registry?: string;
  }): Promise<App | null>;
  draft_report(args: {
    a: App;
  }): Promise<Report>;
  /** Records the registry's case for an app, as a refresh read it. */
  record_moderation_case(args: {
    identity: string;
    case: string;
    status: string;
  }): Promise<void>;
  /** Runs `record_moderation_case` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  record_moderation_caseMany(items: RecordModerationCaseArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Prepares the exact signed operation a report or an appeal sends, without sending it. */
  prepare_report(args: {
    r: Report;
    appeal: boolean;
  }): Promise<Prepared>;
  /** Reads the registry's verified status for the report's case again. A draft stays a draft until the registry answers. */
  refresh_status(args: {
    r: Report;
  }): Promise<void>;
  /** Runs `refresh_status` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  refresh_statusMany(items: RefreshStatusArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Store` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const STORE_ACTIONS = ["poll", "pollMany", "follow", "followMany", "begin_reading", "begin_readingMany", "begin", "beginMany", "read_app", "read_appMany", "show_details", "show_detailsMany", "take_catalogue", "take_catalogueMany", "read_flathub", "read_flathubMany", "read_flathub_details", "read_flathub_detailsMany", "absorb_text", "absorb_remote_text", "absorb_remote_textMany", "refresh_remotes", "refresh_remotesMany", "add_remote", "add_remoteMany", "enable_remote", "enable_remoteMany", "disable_remote", "disable_remoteMany", "remote_change", "remote_changeMany", "remove_remote", "remove_remoteMany", "browse", "browseMany", "update_all", "update_allMany", "add_bot", "add_botMany", "block_app", "block_appMany", "unblock_app", "unblock_appMany", "import_package", "install_reviewed", "install_reviewedMany", "open_reference", "open_referenceMany", "open_link", "review_runtime", "draft_report", "record_moderation_case", "record_moderation_caseMany", "prepare_report", "refresh_status", "refresh_statusMany"] as const;

/** The `tl.so.tilo.store.Store` kind at `path`. */
export function store(ws: tilo.Workspace, path: string): Store {
  return resourceProxy(ws, path, SURFACE, STORE_ACTIONS) as unknown as Store;
}

/** Item — `tl.so.tilo.store.Item`. */
export interface Item extends tilo.Resource {
  epxtvzuf: string;  // identity
  "6a7tsprp": string;  // name
  ea2bjyc3: string;  // summary
  gpwdasim: string;  // description
  tns2iluv: string;  // developer
  "2zltc7rw": string;  // license
  ukzeb4vu: string[];  // categories
  uqkifo7y: string;  // icon
  "5kd5ubmn": string[];  // screenshots
  elvkwauw: string;  // homepage
  okmosbps: string;  // source_code
  mke4chcm: string;  // origin
  xbvh2wz3: string;  // registry
  cpannwt7: string;  // branch
  wikfckou: boolean;  // user_managed
  nwfophjp: string;  // trust
  i4hnlx7u: string[];  // versions
  ehs57hul: string;  // available
  x3qagtfz: string;  // installed
  "36fsx2jx": boolean;  // pinned
  cnrxvmkp: string;  // fetched
  "7jxbeqay": boolean;  // runnable
  nyj6afge: string;  // processors
  jsqjxd7b: boolean;  // awaiting_developer
  taengdq6: number;  // featured
  cvzbzfol: string;  // updated
  px6eamz7: string;  // publisher
  rga5kuoh: string;  // digest
  na6yqlcj: boolean;  // bot
  lb5drfvf: string[];  // bot_triggers
  chtwyqcq: string[];  // bot_apps
  jcezfbrh: string[];  // bot_connectors
  cj5tnnei: boolean;  // bot_appearance
  ojgbghet: string;  // runtime
  j5umq2va: boolean;  // updatable
  "2nmtgjmg": string;  // trouble
  czhobryy: string[] | null;  // supported_languages
}

/** The actions `Item` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ITEM_ACTIONS = [] as const;

/** The `tl.so.tilo.store.Item` kind at `path`. */
export function item(ws: tilo.Workspace, path: string): Item {
  return resourceProxy(ws, path, SURFACE, ITEM_ACTIONS) as unknown as Item;
}

/** Catalogue — `tl.so.tilo.store.Catalogue`. */
export interface Catalogue extends tilo.Resource {
  mr6dwxv6: string[];  // items
  onje62ck: string[];  // remotes
  ncdaffvy: string;  // status
  kntjub7s: string;  // trouble
  av6ojjws: boolean;  // flatpak
  xe2u37td: string;  // flatpak_trouble
  elx5dznp: string[];  // unanswered
  dhwcxbjv: string[];  // expired
}

/** The actions `Catalogue` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CATALOGUE_ACTIONS = [] as const;

/** The `tl.so.tilo.store.Catalogue` kind at `path`. */
export function catalogue(ws: tilo.Workspace, path: string): Catalogue {
  return resourceProxy(ws, path, SURFACE, CATALOGUE_ACTIONS) as unknown as Catalogue;
}

/** RemoteRead — `tl.so.tilo.store.RemoteRead`. */
export interface RemoteRead extends tilo.Resource {
  rz4t6wig: string;  // name
  roitw26x: string;  // title
  "635v2xjh": string;  // url
  xkljozjv: string;  // scope
  e4ylyjav: boolean;  // enabled
  rk5vdazr: string;  // trouble
}

/** The actions `RemoteRead` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const REMOTEREAD_ACTIONS = [] as const;

/** The `tl.so.tilo.store.RemoteRead` kind at `path`. */
export function remoteRead(ws: tilo.Workspace, path: string): RemoteRead {
  return resourceProxy(ws, path, SURFACE, REMOTEREAD_ACTIONS) as unknown as RemoteRead;
}

/** Details — `tl.so.tilo.store.Details`. */
export interface Details extends tilo.Resource {
  "6cb7jl47": string;  // description
  "7cwn55x3": string;  // developer
  mptnhu62: string;  // license
  a37qv2oa: string;  // homepage
  uba5apij: string;  // source_code
  nt6mule6: number;  // size
  lrlxwbcb: number;  // installed_size
  oj36amou: string[];  // screenshots
  g3nhe33n: string[];  // permissions
  lygslquo: string[];  // releases
  whmuitft: string;  // runtime
  uijcroeo: string;  // runtime_registry
  it2pk5w4: string;  // digest
  gmjvqfez: string[] | null;  // supported_languages
}

/** The actions `Details` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DETAILS_ACTIONS = [] as const;

/** The `tl.so.tilo.store.Details` kind at `path`. */
export function details(ws: tilo.Workspace, path: string): Details {
  return resourceProxy(ws, path, SURFACE, DETAILS_ACTIONS) as unknown as Details;
}

/** Review — `tl.so.tilo.store.Review`. */
export interface Review extends tilo.Resource {
  mzd6fvgn: string;  // item
  hxcclid7: string;  // root
  oiutpkg7: string;  // trouble
}

/** The actions `Review` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const REVIEW_ACTIONS = [] as const;

/** The `tl.so.tilo.store.Review` kind at `path`. */
export function review(ws: tilo.Workspace, path: string): Review {
  return resourceProxy(ws, path, SURFACE, REVIEW_ACTIONS) as unknown as Review;
}

/** Case — `tl.so.tilo.store.Case`. */
export interface Case extends tilo.Resource {
  zyv4ydev: string;  // case
  hf6ogyp3: string;  // status
  o4zyd7r3: string;  // situation
  zznn4lla: string;  // trouble
}

/** The actions `Case` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CASE_ACTIONS = [] as const;

/** The `tl.so.tilo.store.Case` kind at `path`. */
export function storeCase(ws: tilo.Workspace, path: string): Case {
  return resourceProxy(ws, path, SURFACE, CASE_ACTIONS) as unknown as Case;
}

/** Prepared — `tl.so.tilo.store.Prepared`. */
export interface Prepared extends tilo.Resource {
  wywxs5oj: string;  // operation
  zenjiysp: string;  // registry
  owkrdhjj: string;  // trouble
}

/** The actions `Prepared` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PREPARED_ACTIONS = [] as const;

/** The `tl.so.tilo.store.Prepared` kind at `path`. */
export function prepared(ws: tilo.Workspace, path: string): Prepared {
  return resourceProxy(ws, path, SURFACE, PREPARED_ACTIONS) as unknown as Prepared;
}

/** Job — `tl.so.tilo.store.Job`. */
export interface Job extends tilo.Resource {
  mqizmue3: string;  // state
  ygdo3z6c: number;  // done
  msrdztlm: number;  // total
  fatgfp52: string;  // error
  "35ydyeno": string;  // status
}

/** The actions `Job` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const JOB_ACTIONS = [] as const;

/** The `tl.so.tilo.store.Job` kind at `path`. */
export function job(ws: tilo.Workspace, path: string): Job {
  return resourceProxy(ws, path, SURFACE, JOB_ACTIONS) as unknown as Job;
}

/** CaseRead — `tl.so.tilo.store.CaseRead`. */
export interface CaseRead extends tilo.Resource {
  wi7dxv7v: string;  // case
  f67cduxy: string;  // status
  "5cmcfoao": string;  // situation
}

/** The actions `CaseRead` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CASEREAD_ACTIONS = [] as const;

/** The `tl.so.tilo.store.CaseRead` kind at `path`. */
export function caseRead(ws: tilo.Workspace, path: string): CaseRead {
  return resourceProxy(ws, path, SURFACE, CASEREAD_ACTIONS) as unknown as CaseRead;
}

/** Change — `tl.so.tilo.store.Change`. */
export interface Change extends tilo.Resource {
  sr6eixi6: string;  // identity
  rkd5rikz: string;  // kind
  wfeji5lc: string;  // version
  gawnzinv: string;  // root
  xz347kth: string;  // surface
  wa7a4ebr: string;  // publisher
  jalnpzfr: string;  // flatpak
  uuhlsrfk: string;  // reference
  "2usub7jq": string;  // file
  smpzmxa2: number;  // size
}

/** The actions `Change` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CHANGE_ACTIONS = [] as const;

/** The `tl.so.tilo.store.Change` kind at `path`. */
export function change(ws: tilo.Workspace, path: string): Change {
  return resourceProxy(ws, path, SURFACE, CHANGE_ACTIONS) as unknown as Change;
}

/** Ask — `tl.so.tilo.store.Ask`. */
export interface Ask extends tilo.Resource {
  k33jf5d6: boolean;  // force
}

/** The actions `Ask` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ASK_ACTIONS = [] as const;

/** The `tl.so.tilo.store.Ask` kind at `path`. */
export function ask(ws: tilo.Workspace, path: string): Ask {
  return resourceProxy(ws, path, SURFACE, ASK_ACTIONS) as unknown as Ask;
}

/** FlathubAsk — `tl.so.tilo.store.FlathubAsk`. */
export interface FlathubAsk extends tilo.Resource {
  yk5x3lyl: string;  // query
  jyhorffp: number;  // hits_per_page
  n6ivjlst: number;  // page
}

/** The actions `FlathubAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const FLATHUBASK_ACTIONS = [] as const;

/** The `tl.so.tilo.store.FlathubAsk` kind at `path`. */
export function flathubAsk(ws: tilo.Workspace, path: string): FlathubAsk {
  return resourceProxy(ws, path, SURFACE, FLATHUBASK_ACTIONS) as unknown as FlathubAsk;
}

/** Started — `tl.so.tilo.store.Started`. */
export interface Started extends tilo.Resource {
  omjipbxx: string;  // job
  haex7z3y: string;  // error
}

/** The actions `Started` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const STARTED_ACTIONS = [] as const;

/** The `tl.so.tilo.store.Started` kind at `path`. */
export function started(ws: tilo.Workspace, path: string): Started {
  return resourceProxy(ws, path, SURFACE, STARTED_ACTIONS) as unknown as Started;
}

/** OneAsk — `tl.so.tilo.store.OneAsk`. */
export interface OneAsk extends tilo.Resource {
  bzdopfzn: string;  // identity
  kz4m2q7x: string;  // registry
}

/** The actions `OneAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ONEASK_ACTIONS = [] as const;

/** The `tl.so.tilo.store.OneAsk` kind at `path`. */
export function oneAsk(ws: tilo.Workspace, path: string): OneAsk {
  return resourceProxy(ws, path, SURFACE, ONEASK_ACTIONS) as unknown as OneAsk;
}

/** One — `tl.so.tilo.store.One`. */
export interface One extends tilo.Resource {
  ibr65ngi: string | null;  // item
}

/** The actions `One` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ONE_ACTIONS = [] as const;

/** The `tl.so.tilo.store.One` kind at `path`. */
export function one(ws: tilo.Workspace, path: string): One {
  return resourceProxy(ws, path, SURFACE, ONE_ACTIONS) as unknown as One;
}

/** OneRead — `tl.so.tilo.store.OneRead`. */
export interface OneRead extends tilo.Resource {
  gwl2nm5a: Item;  // item
}

/** The actions `OneRead` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ONEREAD_ACTIONS = [] as const;

/** The `tl.so.tilo.store.OneRead` kind at `path`. */
export function oneRead(ws: tilo.Workspace, path: string): OneRead {
  return resourceProxy(ws, path, SURFACE, ONEREAD_ACTIONS) as unknown as OneRead;
}

/** RemoteReads — `tl.so.tilo.store.RemoteReads`. */
export interface RemoteReads extends tilo.Resource {
  "6476ldq6": string[];  // remotes
  jr4k76wg: boolean;  // flatpak
  zt4zikpc: string;  // trouble
}

/** The actions `RemoteReads` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const REMOTEREADS_ACTIONS = [] as const;

/** The `tl.so.tilo.store.RemoteReads` kind at `path`. */
export function remoteReads(ws: tilo.Workspace, path: string): RemoteReads {
  return resourceProxy(ws, path, SURFACE, REMOTEREADS_ACTIONS) as unknown as RemoteReads;
}

/** RemoteAsk — `tl.so.tilo.store.RemoteAsk`. */
export interface RemoteAsk extends tilo.Resource {
  "7ltvsddp": string;  // name
  p4q2b4s7: string;  // url
  vmyl5m7m: boolean;  // enabled
}

/** The actions `RemoteAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const REMOTEASK_ACTIONS = [] as const;

/** The `tl.so.tilo.store.RemoteAsk` kind at `path`. */
export function remoteAsk(ws: tilo.Workspace, path: string): RemoteAsk {
  return resourceProxy(ws, path, SURFACE, REMOTEASK_ACTIONS) as unknown as RemoteAsk;
}

/** BlockAsk — `tl.so.tilo.store.BlockAsk`. */
export interface BlockAsk extends tilo.Resource {
  dlrecm5k: string;  // identity
  xjtxb4gh: boolean;  // blocked
}

/** The actions `BlockAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const BLOCKASK_ACTIONS = [] as const;

/** The `tl.so.tilo.store.BlockAsk` kind at `path`. */
export function blockAsk(ws: tilo.Workspace, path: string): BlockAsk {
  return resourceProxy(ws, path, SURFACE, BLOCKASK_ACTIONS) as unknown as BlockAsk;
}

/** PackageAsk — `tl.so.tilo.store.PackageAsk`. */
export interface PackageAsk extends tilo.Resource {
  fxwybrap: string;  // file
  qaju5cxd: string;  // name
  bkjyjpcr: number;  // size
}

/** The actions `PackageAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PACKAGEASK_ACTIONS = [] as const;

/** The `tl.so.tilo.store.PackageAsk` kind at `path`. */
export function packageAsk(ws: tilo.Workspace, path: string): PackageAsk {
  return resourceProxy(ws, path, SURFACE, PACKAGEASK_ACTIONS) as unknown as PackageAsk;
}

/** RuntimeAsk — `tl.so.tilo.store.RuntimeAsk`. */
export interface RuntimeAsk extends tilo.Resource {
  vpcuziom: string;  // runtime
  snmv7oek: string;  // registry
}

/** The actions `RuntimeAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const RUNTIMEASK_ACTIONS = [] as const;

/** The `tl.so.tilo.store.RuntimeAsk` kind at `path`. */
export function runtimeAsk(ws: tilo.Workspace, path: string): RuntimeAsk {
  return resourceProxy(ws, path, SURFACE, RUNTIMEASK_ACTIONS) as unknown as RuntimeAsk;
}

/** ReportAsk — `tl.so.tilo.store.ReportAsk`. */
export interface ReportAsk extends tilo.Resource {
  "3x7qwbnh": string;  // identity
  "4ibtuh5s": string;  // note
  gea2szrq: string;  // case
  "6qwyarvg": string;  // registry
  jjlocy5a: boolean;  // appeal
}

/** The actions `ReportAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const REPORTASK_ACTIONS = [] as const;

/** The `tl.so.tilo.store.ReportAsk` kind at `path`. */
export function reportAsk(ws: tilo.Workspace, path: string): ReportAsk {
  return resourceProxy(ws, path, SURFACE, REPORTASK_ACTIONS) as unknown as ReportAsk;
}

/** SendAsk — `tl.so.tilo.store.SendAsk`. */
export interface SendAsk extends tilo.Resource {
  mn7ndrul: string;  // identity
  rq53vgig: string;  // operation
  f72wtnsy: string;  // note
}

/** The actions `SendAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SENDASK_ACTIONS = [] as const;

/** The `tl.so.tilo.store.SendAsk` kind at `path`. */
export function sendAsk(ws: tilo.Workspace, path: string): SendAsk {
  return resourceProxy(ws, path, SURFACE, SENDASK_ACTIONS) as unknown as SendAsk;
}

/** StatusAsk — `tl.so.tilo.store.StatusAsk`. */
export interface StatusAsk extends tilo.Resource {
  "5xifqzkr": string;  // identity
  pdu4bxlq: string;  // case
  dnrxsz6b: string;  // registry
}

/** The actions `StatusAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const STATUSASK_ACTIONS = [] as const;

/** The `tl.so.tilo.store.StatusAsk` kind at `path`. */
export function statusAsk(ws: tilo.Workspace, path: string): StatusAsk {
  return resourceProxy(ws, path, SURFACE, STATUSASK_ACTIONS) as unknown as StatusAsk;
}

/** Changes — `tl.so.tilo.store.Changes`. */
export interface Changes extends tilo.Resource {
  zxtxkidh: Change[];  // list
}

/** The actions `Changes` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CHANGES_ACTIONS = [] as const;

/** The `tl.so.tilo.store.Changes` kind at `path`. */
export function changes(ws: tilo.Workspace, path: string): Changes {
  return resourceProxy(ws, path, SURFACE, CHANGES_ACTIONS) as unknown as Changes;
}

/** Items — `tl.so.tilo.store.Items`. */
export interface Items extends tilo.Resource {
  r4t6n2uh: string[];  // items
}

/** The actions `Items` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ITEMS_ACTIONS = [] as const;

/** The `tl.so.tilo.store.Items` kind at `path`. */
export function items(ws: tilo.Workspace, path: string): Items {
  return resourceProxy(ws, path, SURFACE, ITEMS_ACTIONS) as unknown as Items;
}

/** Evidence — `tl.so.tilo.store.Evidence`. */
export interface Evidence extends tilo.Resource {
  j6n5hmtr: string;  // identity
  bnqubb5u: string;  // note
  cbhojiz7: string;  // status
  t7a6wmgq: string;  // case
  ra5kasg4: string;  // registry
  dvwnjvtj: string;  // operation
}

/** The actions `Evidence` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const EVIDENCE_ACTIONS = [] as const;

/** The `tl.so.tilo.store.Evidence` kind at `path`. */
export function evidence(ws: tilo.Workspace, path: string): Evidence {
  return resourceProxy(ws, path, SURFACE, EVIDENCE_ACTIONS) as unknown as Evidence;
}

/** App Store — `tl.so.tilo.store`. */
export interface TiloStore extends tilo.Resource {
}

/** The actions `TiloStore` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TILOSTORE_ACTIONS = [] as const;

/** The `tl.so.tilo.store` kind at `path`. */
export function tiloStore(ws: tilo.Workspace, path: string): TiloStore {
  return resourceProxy(ws, path, SURFACE, TILOSTORE_ACTIONS) as unknown as TiloStore;
}

