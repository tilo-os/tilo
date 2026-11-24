/**
 * Types for Feeds (`so.tilo.feeds`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Folder` is a `Resource` of the app's `tl.so.tilo.feeds.Folder`, made by its factory with the
 * resource's path (`feeds/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.feeds@f8c97eb153fc38b3";

/** The format a subscription is read in. One of "rss", "atom", "json", "gemini". */
export type FeedFormat = "rss" | "atom" | "json" | "gemini";

/** The arguments of `rename_folder`. */
export interface RenameFolderArgs {
  name: string;
}

/** The arguments of `rename_feed`. */
export interface RenameFeedArgs {
  name: string;
}

/** The arguments of `move_feed`. */
export interface MoveFeedArgs {
  folder?: Folder;
}

/** The arguments of `edit-block`. */
export interface EditBlockArgs {
  block: string;
  text: string;
}

/** The arguments of `insert-block`. */
export interface InsertBlockArgs {
  block: string;
  text: string;
}

/** The arguments of `delete-block`. */
export interface DeleteBlockArgs {
  block: string;
}

/** The arguments of `show_article`. */
export interface ShowArticleArgs {
}

/** The arguments of `set_read`. */
export interface SetReadArgs {
  read?: boolean;
}

/** The arguments of `star`. */
export interface StarArgs {
  starred?: boolean;
}

/** The arguments of `set_tags`. */
export interface SetTagsArgs {
  tags: string;
}

/** The arguments of `show_place`. */
export interface ShowPlaceArgs {
  place: string;
}

/** The arguments of `show_tag`. */
export interface ShowTagArgs {
  tag: string;
}

/** The arguments of `set_scale`. */
export interface SetScaleArgs {
  scale: number;
}

/** The arguments of `larger_text`. */
export interface LargerTextArgs {
}

/** The arguments of `smaller_text`. */
export interface SmallerTextArgs {
}

/** The arguments of `set_face`. */
export interface SetFaceArgs {
  face: string;
}

/** The arguments of `set_paper`. */
export interface SetPaperArgs {
  paper: string;
}

/** The arguments of `unsubscribe`. */
export interface UnsubscribeArgs {
  feed: Feed;
}

/** The arguments of `new_folder`. */
export interface NewFolderArgs {
  name: string;
}

/** The arguments of `delete_folder`. */
export interface DeleteFolderArgs {
  folder: Folder;
}

/** The arguments of `mark_read`. */
export interface MarkReadArgs {
  place?: string;
}

/** The arguments of `stop_refresh`. */
export interface StopRefreshArgs {
}

/** The arguments of `poll_refresh`. */
export interface PollRefreshArgs {
}

/** The arguments of `import_opml`. */
export interface ImportOpmlArgs {
  file: unknown;
}

/** Folder — `tl.so.tilo.feeds.Folder`. */
export interface Folder extends tilo.Resource {
  ygksbjy5: string;  // name
  /** Renames a folder. */
  rename_folder(args: {
    name: string;
  }): Promise<void>;
  /** Runs `rename_folder` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  rename_folderMany(items: RenameFolderArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Folder` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const FOLDER_ACTIONS = ["rename_folder", "rename_folderMany"] as const;

/** The `tl.so.tilo.feeds.Folder` kind at `path`. */
export function folder(ws: tilo.Workspace, path: string): Folder {
  return resourceProxy(ws, path, SURFACE, FOLDER_ACTIONS) as unknown as Folder;
}

/** Feed — `tl.so.tilo.feeds.Feed`. */
export interface Feed extends tilo.Resource {
  ruq4ynjw: string;  // name
  jdxu5vhb: string;  // url
  kitws4yh: string;  // site
  ecrfpxxe: Folder | null;  // folder
  lb5mig4n: FeedFormat;  // format
  "2wtntp4k": string;  // pin
  py34q4xk: string;  // trouble
  /** Renames a subscription, whatever its feed calls itself. */
  rename_feed(args: {
    name: string;
  }): Promise<void>;
  /** Runs `rename_feed` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  rename_feedMany(items: RenameFeedArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Moves a subscription into a folder, or out of every folder. */
  move_feed(args?: {
    folder?: Folder;
  }): Promise<void>;
  /** Runs `move_feed` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  move_feedMany(items: MoveFeedArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Feed` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const FEED_ACTIONS = ["rename_feed", "rename_feedMany", "move_feed", "move_feedMany"] as const;

/** The `tl.so.tilo.feeds.Feed` kind at `path`. */
export function feed(ws: tilo.Workspace, path: string): Feed {
  return resourceProxy(ws, path, SURFACE, FEED_ACTIONS) as unknown as Feed;
}

/** Article — `tl.so.tilo.feeds.Article`. */
export interface Article extends tilo.Resource {
  chbpw5pg: Feed;  // feed
  swmvsojn: string;  // title
  gvntfpn6: string;  // author
  p5lxkp7w: string;  // url
  s2wxy2go: string | null;  // published
  ok5mmsqb: string;  // summary
  "2yi7idis": string;  // body
  y7lqgqyr: boolean;  // read
  hqmrnoz5: boolean;  // starred
  "4nrqtndr": string[];  // tags
  /** Set one paragraph of a rich-text field to what a Markdown text says. */
  editBlock(args: {
    block: string;
    text: string;
  }): Promise<void>;
  /** Runs `edit-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  editBlockMany(items: EditBlockArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Put a paragraph before the one the address names. */
  insertBlock(args: {
    block: string;
    text: string;
  }): Promise<void>;
  /** Runs `insert-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  insertBlockMany(items: InsertBlockArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Remove the paragraph the address names, and the paragraphs it holds. */
  deleteBlock(args: {
    block: string;
  }): Promise<void>;
  /** Runs `delete-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deleteBlockMany(items: DeleteBlockArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shows the place an article's feed lists it in, so a search hit or an agent's `open` lands its row in the list beside it. */
  show_article(): Promise<void>;
  /** Runs `show_article` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  show_articleMany(items: ShowArticleArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Marks an article read, or unread. */
  set_read(args?: {
    read?: boolean;
  }): Promise<void>;
  /** Runs `set_read` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_readMany(items: SetReadArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Stars an article, or takes the star back. */
  star(args?: {
    starred?: boolean;
  }): Promise<void>;
  /** Runs `star` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  starMany(items: StarArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Gives an article the tags a person typed, comma-separated: each trimmed, lower case, without `#`, at most 24 of them. */
  set_tags(args: {
    tags: string;
  }): Promise<void>;
  /** Runs `set_tags` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_tagsMany(items: SetTagsArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Article` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ARTICLE_ACTIONS = ["editBlock", "editBlockMany", "insertBlock", "insertBlockMany", "deleteBlock", "deleteBlockMany", "show_article", "show_articleMany", "set_read", "set_readMany", "star", "starMany", "set_tags", "set_tagsMany"] as const;

/** The `tl.so.tilo.feeds.Article` kind at `path`. */
export function article(ws: tilo.Workspace, path: string): Article {
  return resourceProxy(ws, path, SURFACE, ARTICLE_ACTIONS) as unknown as Article;
}

/** Reader — `tl.so.tilo.feeds.Reader`. */
export interface Reader extends tilo.Resource {
  tqbu72hj: string;  // place
  lx6zg57h: number;  // scale
  um6nclsf: string;  // face
  to5unuga: string;  // paper
  /** Reads from `place` in the sidebar. The place is the reader's own, so it follows them to their other devices and no one else's reading moves with it. */
  show_place(args: {
    place: string;
  }): Promise<void>;
  /** Runs `show_place` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  show_placeMany(items: ShowPlaceArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shows the place a tag's articles stand in. */
  show_tag(args: {
    tag: string;
  }): Promise<void>;
  /** Runs `show_tag` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  show_tagMany(items: ShowTagArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sets how large the article's text is set: 0.8 to 2 times the reading base. */
  set_scale(args: {
    scale: number;
  }): Promise<void>;
  /** Runs `set_scale` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_scaleMany(items: SetScaleArgs[]): Promise<tilo.ActionAnswer[]>;
  /** A step larger in the reading itself. */
  larger_text(): Promise<void>;
  /** Runs `larger_text` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  larger_textMany(items: LargerTextArgs[]): Promise<tilo.ActionAnswer[]>;
  /** A step smaller. */
  smaller_text(): Promise<void>;
  /** Runs `smaller_text` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  smaller_textMany(items: SmallerTextArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The face the article is set in: `serif`, `sans` or `mono`. */
  set_face(args: {
    face: string;
  }): Promise<void>;
  /** Runs `set_face` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_faceMany(items: SetFaceArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The paper the article is printed on: `system` follows the theme, or a fixed page for long-form reading. */
  set_paper(args: {
    paper: string;
  }): Promise<void>;
  /** Runs `set_paper` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_paperMany(items: SetPaperArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Reader` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const READER_ACTIONS = ["show_place", "show_placeMany", "show_tag", "show_tagMany", "set_scale", "set_scaleMany", "larger_text", "larger_textMany", "smaller_text", "smaller_textMany", "set_face", "set_faceMany", "set_paper", "set_paperMany"] as const;

/** The `tl.so.tilo.feeds.Reader` kind at `path`. */
export function reader(ws: tilo.Workspace, path: string): Reader {
  return resourceProxy(ws, path, SURFACE, READER_ACTIONS) as unknown as Reader;
}

/** Refresh — `tl.so.tilo.feeds.Refresh`. */
export interface Refresh extends tilo.Resource {
  crwgoxd6: string;  // job
  dzn4ssl3: string;  // state
  "7d33zhis": number;  // done
  mq3vs354: number;  // total
  pvpr56lq: number;  // fresh
  gbspv2g7: number;  // failed
  dbqvtyzb: string;  // note
}

/** The actions `Refresh` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const REFRESH_ACTIONS = [] as const;

/** The `tl.so.tilo.feeds.Refresh` kind at `path`. */
export function refresh(ws: tilo.Workspace, path: string): Refresh {
  return resourceProxy(ws, path, SURFACE, REFRESH_ACTIONS) as unknown as Refresh;
}

/** Library — `tl.so.tilo.feeds.Library`. */
export interface Library extends tilo.Resource {
  urf3jsnz: Array<tilo.Entry<Folder>>;  // folders
  ba25drsv: Array<tilo.Entry<Feed>>;  // feeds
  mymgo65x: Array<tilo.Entry<Article>>;  // articles
  "53g3dh5y": Array<tilo.Entry<Reader>>;  // readers
  ptibzrny: Refresh;  // refresh
  /** Gives the person a reading of their own here, if they have none yet: the place they read in and how the article's page is set. */
  begin_reading(): Promise<Reader>;
  /** Subscribes to a feed's address, or shows the subscription this library already has. */
  add_subscription(args: {
    url: string;
    folder?: Folder;
  }): Promise<Feed>;
  /** Unsubscribes: the subscription leaves the sidebar, and what it brought in is taken away with it. Undo brings both back. */
  unsubscribe(args: {
    feed: Feed;
  }): Promise<void>;
  /** Runs `unsubscribe` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  unsubscribeMany(items: UnsubscribeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Starts a folder that keeps related subscriptions together; its subscriptions stay when it goes. */
  new_folder(args: {
    name: string;
  }): Promise<void>;
  /** Runs `new_folder` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  new_folderMany(items: NewFolderArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Deletes a folder; its subscriptions stay, at the top of the sidebar. */
  delete_folder(args: {
    folder: Folder;
  }): Promise<void>;
  /** Runs `delete_folder` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  delete_folderMany(items: DeleteFolderArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Marks everything the place `place` lists as read. */
  mark_read(args?: {
    place?: string;
  }): Promise<void>;
  /** Runs `mark_read` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  mark_readMany(items: MarkReadArgs[]): Promise<tilo.ActionAnswer[]>;
  // the app's `refresh` action is the SDK's own: run it with `action("refresh")`.
  /** Stops a refresh that is running. */
  stop_refresh(): Promise<void>;
  /** Runs `stop_refresh` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  stop_refreshMany(items: StopRefreshArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes what the device has read so far: each subscription's answer as it lands, and what the feed says of itself with it. What a refresh brought in is nobody's step of their own, so landing it says nothing and offers no undo of its own; an article the library already keeps is not added twice. Landing writes, so it stands here rather than in a call of its own: a run that calls another action is run twice, and the device's answers are not its to answer twice. */
  poll_refresh(): Promise<void>;
  /** Runs `poll_refresh` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  poll_refreshMany(items: PollRefreshArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Subscribes to what an OPML file lists: every address the library does not have yet, in the folders the file names, made where missing. */
  import_opml(args: {
    file: unknown;
  }): Promise<void>;
  /** Runs `import_opml` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  import_opmlMany(items: ImportOpmlArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The library's subscriptions as an OPML 2.0 file. */
  export_opml(): Promise<unknown>;
}

/** The actions `Library` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LIBRARY_ACTIONS = ["begin_reading", "add_subscription", "unsubscribe", "unsubscribeMany", "new_folder", "new_folderMany", "delete_folder", "delete_folderMany", "mark_read", "mark_readMany", "stop_refresh", "stop_refreshMany", "poll_refresh", "poll_refreshMany", "import_opml", "import_opmlMany", "export_opml"] as const;

/** The `tl.so.tilo.feeds.Library` kind at `path`. */
export function library(ws: tilo.Workspace, path: string): Library {
  return resourceProxy(ws, path, SURFACE, LIBRARY_ACTIONS) as unknown as Library;
}

/** Ask — `tl.so.tilo.feeds.Ask`. */
export interface Ask extends tilo.Resource {
  ajc3gacu: AskFeed[];  // feeds
}

/** The actions `Ask` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ASK_ACTIONS = [] as const;

/** The `tl.so.tilo.feeds.Ask` kind at `path`. */
export function ask(ws: tilo.Workspace, path: string): Ask {
  return resourceProxy(ws, path, SURFACE, ASK_ACTIONS) as unknown as Ask;
}

/** AskFeed — `tl.so.tilo.feeds.AskFeed`. */
export interface AskFeed extends tilo.Resource {
  wvgxfoj6: string;  // url
  rzd7dr5x: string;  // pin
}

/** The actions `AskFeed` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ASKFEED_ACTIONS = [] as const;

/** The `tl.so.tilo.feeds.AskFeed` kind at `path`. */
export function askFeed(ws: tilo.Workspace, path: string): AskFeed {
  return resourceProxy(ws, path, SURFACE, ASKFEED_ACTIONS) as unknown as AskFeed;
}

/** Job — `tl.so.tilo.feeds.Job`. */
export interface Job extends tilo.Resource {
  vxpbania: string;  // state
  qen3hx3f: number;  // done
  fm7ik3wg: number;  // total
  krtut5p6: string[];  // ready
}

/** The actions `Job` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const JOB_ACTIONS = [] as const;

/** The `tl.so.tilo.feeds.Job` kind at `path`. */
export function job(ws: tilo.Workspace, path: string): Job {
  return resourceProxy(ws, path, SURFACE, JOB_ACTIONS) as unknown as Job;
}

/** Taken — `tl.so.tilo.feeds.Taken`. */
export interface Taken extends tilo.Resource {
  karmfgtt: boolean;  // ok
  mkw76ot4: string;  // title
  vxvxtmdp: string;  // pin
  rcxpbdkb: string;  // reason
  tkh3vj2o: number;  // status
  "3oef7xt5": string;  // message
  in2no76u: string[];  // articles
}

/** The actions `Taken` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TAKEN_ACTIONS = [] as const;

/** The `tl.so.tilo.feeds.Taken` kind at `path`. */
export function taken(ws: tilo.Workspace, path: string): Taken {
  return resourceProxy(ws, path, SURFACE, TAKEN_ACTIONS) as unknown as Taken;
}

/** Pulled — `tl.so.tilo.feeds.Pulled`. */
export interface Pulled extends tilo.Resource {
  qablmehl: string;  // title
  "5sdtzscn": string;  // author
  ae5rtinu: string;  // url
  gmi4oufp: number;  // published
  pj5brijd: string;  // summary
  t2byonw5: string;  // body
}

/** The actions `Pulled` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PULLED_ACTIONS = [] as const;

/** The `tl.so.tilo.feeds.Pulled` kind at `path`. */
export function pulled(ws: tilo.Workspace, path: string): Pulled {
  return resourceProxy(ws, path, SURFACE, PULLED_ACTIONS) as unknown as Pulled;
}

/** TakeAsk — `tl.so.tilo.feeds.TakeAsk`. */
export interface TakeAsk extends tilo.Resource {
  "5mcvbcwk": string;  // job
  ixh37lm6: string;  // url
}

/** The actions `TakeAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TAKEASK_ACTIONS = [] as const;

/** The `tl.so.tilo.feeds.TakeAsk` kind at `path`. */
export function takeAsk(ws: tilo.Workspace, path: string): TakeAsk {
  return resourceProxy(ws, path, SURFACE, TAKEASK_ACTIONS) as unknown as TakeAsk;
}

/** Outline — `tl.so.tilo.feeds.Outline`. */
export interface Outline extends tilo.Resource {
  esmrzlts: string;  // title
  fhfn2dm2: string;  // url
  akah2ujo: string;  // site
  otz67kho: string;  // folder
}

/** The actions `Outline` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const OUTLINE_ACTIONS = [] as const;

/** The `tl.so.tilo.feeds.Outline` kind at `path`. */
export function outline(ws: tilo.Workspace, path: string): Outline {
  return resourceProxy(ws, path, SURFACE, OUTLINE_ACTIONS) as unknown as Outline;
}

/** Outlines — `tl.so.tilo.feeds.Outlines`. */
export interface Outlines extends tilo.Resource {
  z646rsds: string[];  // outlines
}

/** The actions `Outlines` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const OUTLINES_ACTIONS = [] as const;

/** The `tl.so.tilo.feeds.Outlines` kind at `path`. */
export function outlines(ws: tilo.Workspace, path: string): Outlines {
  return resourceProxy(ws, path, SURFACE, OUTLINES_ACTIONS) as unknown as Outlines;
}

/** OpmlAsk — `tl.so.tilo.feeds.OpmlAsk`. */
export interface OpmlAsk extends tilo.Resource {
  vg5idgn5: string;  // text
}

/** The actions `OpmlAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const OPMLASK_ACTIONS = [] as const;

/** The `tl.so.tilo.feeds.OpmlAsk` kind at `path`. */
export function opmlAsk(ws: tilo.Workspace, path: string): OpmlAsk {
  return resourceProxy(ws, path, SURFACE, OPMLASK_ACTIONS) as unknown as OpmlAsk;
}

/** OpmlWrite — `tl.so.tilo.feeds.OpmlWrite`. */
export interface OpmlWrite extends tilo.Resource {
  pd5lnepq: OpmlFeed[];  // feeds
}

/** The actions `OpmlWrite` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const OPMLWRITE_ACTIONS = [] as const;

/** The `tl.so.tilo.feeds.OpmlWrite` kind at `path`. */
export function opmlWrite(ws: tilo.Workspace, path: string): OpmlWrite {
  return resourceProxy(ws, path, SURFACE, OPMLWRITE_ACTIONS) as unknown as OpmlWrite;
}

/** OpmlFeed — `tl.so.tilo.feeds.OpmlFeed`. */
export interface OpmlFeed extends tilo.Resource {
  "5w7b7tgv": string;  // title
  "5u77j2s2": string;  // url
  igyb7lxx: string;  // site
  gjs6qx4q: string;  // format
  "367at7x3": string;  // folder
}

/** The actions `OpmlFeed` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const OPMLFEED_ACTIONS = [] as const;

/** The `tl.so.tilo.feeds.OpmlFeed` kind at `path`. */
export function opmlFeed(ws: tilo.Workspace, path: string): OpmlFeed {
  return resourceProxy(ws, path, SURFACE, OPMLFEED_ACTIONS) as unknown as OpmlFeed;
}

/** OpmlText — `tl.so.tilo.feeds.OpmlText`. */
export interface OpmlText extends tilo.Resource {
  q2mosawv: string;  // text
}

/** The actions `OpmlText` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const OPMLTEXT_ACTIONS = [] as const;

/** The `tl.so.tilo.feeds.OpmlText` kind at `path`. */
export function opmlText(ws: tilo.Workspace, path: string): OpmlText {
  return resourceProxy(ws, path, SURFACE, OPMLTEXT_ACTIONS) as unknown as OpmlText;
}

/** Feeds — `tl.so.tilo.feeds`. */
export interface Feeds extends tilo.Resource {
}

/** The actions `Feeds` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const FEEDS_ACTIONS = [] as const;

/** The `tl.so.tilo.feeds` kind at `path`. */
export function feeds(ws: tilo.Workspace, path: string): Feeds {
  return resourceProxy(ws, path, SURFACE, FEEDS_ACTIONS) as unknown as Feeds;
}

