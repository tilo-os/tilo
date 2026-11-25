/**
 * Types for Git (`so.tilo.git-client`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.json`; write
 * it again rather than editing it.
 *
 * `Notice` is a `Resource` of the app's `tl.tilo.host.Notice`, made by its factory with the
 * resource's path (`git-client/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.git-client@5d95e00cdcb58504";

/** Where a change stands: in the next commit, still in the working tree, or not tracked yet. One of "staged", "unstaged", "untracked". */
export type ChangeArea = "staged" | "unstaged" | "untracked";

/** What became of a file. One of "added", "modified", "deleted", "renamed", "untracked", "conflicted". */
export type ChangeKind = "added" | "modified" | "deleted" | "renamed" | "untracked" | "conflicted";

/** What a ref on a commit is. One of "head", "local_branch", "remote_branch", "tag". */
export type RefKind = "head" | "local_branch" | "remote_branch" | "tag";

/** How a diff reads. One of "unified", "split". */
export type DiffLayout = "unified" | "split";

/** Where the last approved check stands: none ran yet (or it was put away), the host runs it now, the host observed how its commands ended, it was stopped, or the workspace or the host refused it. One of "idle", "running", "observed", "stopped", "refused". */
export type CheckState = "idle" | "running" | "observed" | "stopped" | "refused";

/** The arguments of `read_folder`. */
export interface ReadFolderArgs {
  folder: string;
}

/** The arguments of `open_repository`. */
export interface OpenRepositoryArgs {
  path: string;
}

/** The arguments of `choose_repository`. */
export interface ChooseRepositoryArgs {
}

/** The arguments of `open_browsed`. */
export interface OpenBrowsedArgs {
}

/** The arguments of `close_picker`. */
export interface ClosePickerArgs {
}

/** The arguments of `stage_file`. */
export interface StageFileArgs {
  c: Change;
}

/** The arguments of `unstage_file`. */
export interface UnstageFileArgs {
  c: Change;
}

/** The arguments of `change_on_key`. */
export interface ChangeOnKeyArgs {
  c: Change;
  key: string;
}

/** The arguments of `discard_file`. */
export interface DiscardFileArgs {
  c: Change;
}

/** The arguments of `stage_all`. */
export interface StageAllArgs {
}

/** The arguments of `unstage_all`. */
export interface UnstageAllArgs {
}

/** The arguments of `stage_hunk`. */
export interface StageHunkArgs {
  c: Change;
  hunk: number;
}

/** The arguments of `unstage_hunk`. */
export interface UnstageHunkArgs {
  c: Change;
  hunk: number;
}

/** The arguments of `discard_hunk`. */
export interface DiscardHunkArgs {
  c: Change;
  hunk: number;
}

/** The arguments of `commit`. */
export interface CommitArgs {
  message: string;
}

/** The arguments of `resolve_conflict`. */
export interface ResolveConflictArgs {
  c: Change;
  ours: boolean;
}

/** The arguments of `create_branch`. */
export interface CreateBranchArgs {
  name: string;
}

/** The arguments of `checkout_branch`. */
export interface CheckoutBranchArgs {
  name: string;
}

/** The arguments of `switch_stashing`. */
export interface SwitchStashingArgs {
}

/** The arguments of `switch_carrying`. */
export interface SwitchCarryingArgs {
}

/** The arguments of `cancel_switch`. */
export interface CancelSwitchArgs {
}

/** The arguments of `switch_to`. */
export interface SwitchToArgs {
  name: string;
  stash: boolean;
}

/** The arguments of `delete_branch`. */
export interface DeleteBranchArgs {
  name: string;
}

/** The arguments of `branch_on_key`. */
export interface BranchOnKeyArgs {
  name: string;
  is_remote: boolean;
  key: string;
}

/** The arguments of `save_stash`. */
export interface SaveStashArgs {
  message?: string;
}

/** The arguments of `apply_stash`. */
export interface ApplyStashArgs {
  index: number;
}

/** The arguments of `create_worktree`. */
export interface CreateWorktreeArgs {
  path: string;
  branch: string;
  start: string;
}

/** The arguments of `fetch`. */
export interface FetchArgs {
  remote?: string;
}

/** The arguments of `pull`. */
export interface PullArgs {
  remote?: string;
}

/** The arguments of `push`. */
export interface PushArgs {
  remote?: string;
}

/** The arguments of `run-approved-check`. */
export interface RunApprovedCheckArgs {
  check_id: string;
}

/** The arguments of `poll_check`. */
export interface PollCheckArgs {
}

/** The arguments of `cancel_check`. */
export interface CancelCheckArgs {
}

/** The arguments of `clear_check`. */
export interface ClearCheckArgs {
}

/** The arguments of `read_commit`. */
export interface ReadCommitArgs {
  id: string;
}

/** The arguments of `commit_on_key`. */
export interface CommitOnKeyArgs {
  id: string;
  key: string;
}

/** The arguments of `toggle_layout`. */
export interface ToggleLayoutArgs {
  s: Session;
  layout?: DiffLayout;
}

/** The arguments of `toggle_syntax`. */
export interface ToggleSyntaxArgs {
  s: Session;
  on?: boolean;
}

/** The arguments of `toggle_whitespace`. */
export interface ToggleWhitespaceArgs {
  s: Session;
  on?: boolean;
}

/** The arguments of `toggle_wrap`. */
export interface ToggleWrapArgs {
  s: Session;
  wrap?: boolean;
}

/** The arguments of `clear_trouble`. */
export interface ClearTroubleArgs {
}

/** The arguments of `clear_notice`. */
export interface ClearNoticeArgs {
}

/** The arguments of `asks`. */
export interface AsksArgs {
  s: Session;
  op: string;
  ask: string;
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

/** Hunk — `tl.so.tilo.git-client.Hunk`. */
export interface Hunk extends tilo.Resource {
  a7uhrt52: number;  // index
  "5ccadzg6": string;  // header
  dfethllq: number;  // old_start
  pigkwwm6: number;  // new_start
}

/** The actions `Hunk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const HUNK_ACTIONS = [] as const;

/** The `tl.so.tilo.git-client.Hunk` kind at `path`. */
export function hunk(ws: tilo.Workspace, path: string): Hunk {
  return resourceProxy(ws, path, SURFACE, HUNK_ACTIONS) as unknown as Hunk;
}

/** Change — `tl.so.tilo.git-client.Change`. */
export interface Change extends tilo.Resource {
  ulyz6e5m: string;  // path
  pyq3ne6a: string | null;  // old_path
  gyf3ucvv: ChangeArea;  // area
  a64nnfrl: ChangeKind;  // kind
  pgbhcfmd: number;  // additions
  cb3hp5ts: number;  // deletions
  ddlu6eiw: boolean;  // binary
  "547agrds": number;  // ours
  gcyh3qk6: number;  // theirs
  "5bywp6ty": string;  // language
  "2ejdsikb": string;  // unified
  bdjzphf3: string;  // left
  qmisixex: string;  // right
  qo6hyuiv: string;  // hunks
}

/** The actions `Change` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CHANGE_ACTIONS = [] as const;

/** The `tl.so.tilo.git-client.Change` kind at `path`. */
export function change(ws: tilo.Workspace, path: string): Change {
  return resourceProxy(ws, path, SURFACE, CHANGE_ACTIONS) as unknown as Change;
}

/** RefLabel — `tl.so.tilo.git-client.RefLabel`. */
export interface RefLabel extends tilo.Resource {
  zl57j56i: string;  // name
  "7lddqvmy": RefKind;  // kind
}

/** The actions `RefLabel` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const REFLABEL_ACTIONS = [] as const;

/** The `tl.so.tilo.git-client.RefLabel` kind at `path`. */
export function refLabel(ws: tilo.Workspace, path: string): RefLabel {
  return resourceProxy(ws, path, SURFACE, REFLABEL_ACTIONS) as unknown as RefLabel;
}

/** Commit — `tl.so.tilo.git-client.Commit`. */
export interface Commit extends tilo.Resource {
  ge4jaage: string;  // id
  ktcweris: string;  // summary
  qkodo2um: string;  // body
  mbs2zev2: string;  // author_name
  "2rplzpux": string;  // author_email
  g5wqmtlu: number;  // time
  i5ezibch: number;  // age
  ae7mbonn: string;  // date
  vngsr7bo: string[];  // parents
  "3k7gvvan": WireRef[];  // refs
  okssmwxo: number;  // column
}

/** The actions `Commit` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const COMMIT_ACTIONS = [] as const;

/** The `tl.so.tilo.git-client.Commit` kind at `path`. */
export function commit(ws: tilo.Workspace, path: string): Commit {
  return resourceProxy(ws, path, SURFACE, COMMIT_ACTIONS) as unknown as Commit;
}

/** Branch — `tl.so.tilo.git-client.Branch`. */
export interface Branch extends tilo.Resource {
  mmht373l: string;  // name
  "2xd7n6ck": boolean;  // is_head
  ejqxjylh: boolean;  // is_remote
  vkfjbs4a: string | null;  // upstream
  uvswvf53: number;  // ahead
  awh54bp4: number;  // behind
  fkwngzoz: string;  // tip
}

/** The actions `Branch` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const BRANCH_ACTIONS = [] as const;

/** The `tl.so.tilo.git-client.Branch` kind at `path`. */
export function branch(ws: tilo.Workspace, path: string): Branch {
  return resourceProxy(ws, path, SURFACE, BRANCH_ACTIONS) as unknown as Branch;
}

/** Tag — `tl.so.tilo.git-client.Tag`. */
export interface Tag extends tilo.Resource {
  eimgqjmn: string;  // name
  rvt4lxwy: string;  // target
  qpus5rj2: string | null;  // message
}

/** The actions `Tag` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TAG_ACTIONS = [] as const;

/** The `tl.so.tilo.git-client.Tag` kind at `path`. */
export function tag(ws: tilo.Workspace, path: string): Tag {
  return resourceProxy(ws, path, SURFACE, TAG_ACTIONS) as unknown as Tag;
}

/** Stash — `tl.so.tilo.git-client.Stash`. */
export interface Stash extends tilo.Resource {
  plxxvefl: number;  // index
  npo2d33u: string;  // message
  kr54kttq: string;  // branch
}

/** The actions `Stash` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const STASH_ACTIONS = [] as const;

/** The `tl.so.tilo.git-client.Stash` kind at `path`. */
export function stash(ws: tilo.Workspace, path: string): Stash {
  return resourceProxy(ws, path, SURFACE, STASH_ACTIONS) as unknown as Stash;
}

/** Remote — `tl.so.tilo.git-client.Remote`. */
export interface Remote extends tilo.Resource {
  fi4gej3v: string;  // name
  ug544zky: string;  // url
}

/** The actions `Remote` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const REMOTE_ACTIONS = [] as const;

/** The `tl.so.tilo.git-client.Remote` kind at `path`. */
export function remote(ws: tilo.Workspace, path: string): Remote {
  return resourceProxy(ws, path, SURFACE, REMOTE_ACTIONS) as unknown as Remote;
}

/** Worktree — `tl.so.tilo.git-client.Worktree`. */
export interface Worktree extends tilo.Resource {
  syhlyvff: string;  // name
  s43oholm: string;  // path
  zumfqfnq: string;  // head
  rcy5osnr: boolean;  // current
}

/** The actions `Worktree` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const WORKTREE_ACTIONS = [] as const;

/** The `tl.so.tilo.git-client.Worktree` kind at `path`. */
export function worktree(ws: tilo.Workspace, path: string): Worktree {
  return resourceProxy(ws, path, SURFACE, WORKTREE_ACTIONS) as unknown as Worktree;
}

/** CommitFile — `tl.so.tilo.git-client.CommitFile`. */
export interface CommitFile extends tilo.Resource {
  elliscls: string;  // path
  tlnyi6uz: string | null;  // old_path
  "65rizway": ChangeKind;  // kind
  "7u7ngqkr": number;  // additions
  qdtrudtf: number;  // deletions
  "4pwix6ke": boolean;  // binary
  vgnn6ynm: string;  // language
  j2nczx73: string;  // unified
  kgmcwjbi: string;  // left
  xtgtu5ad: string;  // right
}

/** The actions `CommitFile` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const COMMITFILE_ACTIONS = [] as const;

/** The `tl.so.tilo.git-client.CommitFile` kind at `path`. */
export function commitFile(ws: tilo.Workspace, path: string): CommitFile {
  return resourceProxy(ws, path, SURFACE, COMMITFILE_ACTIONS) as unknown as CommitFile;
}

/** BrowseEntry — `tl.so.tilo.git-client.BrowseEntry`. */
export interface BrowseEntry extends tilo.Resource {
  mazzivt3: string;  // name
  "67eqnwql": string;  // path
  amedesig: boolean;  // folder
}

/** The actions `BrowseEntry` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const BROWSEENTRY_ACTIONS = [] as const;

/** The `tl.so.tilo.git-client.BrowseEntry` kind at `path`. */
export function browseEntry(ws: tilo.Workspace, path: string): BrowseEntry {
  return resourceProxy(ws, path, SURFACE, BROWSEENTRY_ACTIONS) as unknown as BrowseEntry;
}

/** Session — `tl.so.tilo.git-client.Session`. */
export interface Session extends tilo.Resource {
  vrfkfgub: string;  // path
  qtrusa64: Array<tilo.Entry<Change>>;  // changes
  qakm3prq: DiffLayout;  // layout
  "5pb4bgxw": boolean;  // highlight
  m5eoofqc: boolean;  // whitespace
  mk3evbqh: boolean;  // wrap
  ma5zpjxi: string;  // pending_switch
  taytqzee: boolean;  // browsing
  cxuhuns5: string;  // folder
  ermpuo4w: Array<tilo.Entry<BrowseEntry>>;  // entries
  gpp2fllc: string;  // commit_id
  "7ihfqmuw": string;  // commit_files
  mr62vl75: string;  // notice
  hjgppyqi: string;  // trouble
  /** Lists `folder` in the repository picker. */
  read_folder(args: {
    folder: string;
  }): Promise<void>;
  /** Runs `read_folder` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  read_folderMany(items: ReadFolderArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Session` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SESSION_ACTIONS = ["read_folder", "read_folderMany"] as const;

/** The `tl.so.tilo.git-client.Session` kind at `path`. */
export function session(ws: tilo.Workspace, path: string): Session {
  return resourceProxy(ws, path, SURFACE, SESSION_ACTIONS) as unknown as Session;
}

/** Repository — `tl.so.tilo.git-client.Repository`. */
export interface Repository extends tilo.Resource {
  n4wepl5s: string;  // path
  "6ju4ea2u": string;  // name
  ori4pxhp: string;  // head
  nqllokpk: string;  // workdir
  ev43omls: string;  // author_name
  "6ejc2liq": string;  // author_email
  os3blsxu: string;  // snapshot
  vioio4dz: Array<tilo.Entry<Session>>;  // sessions
  "6lsikkhk": ApprovedCheck;  // check
  // the app's `refresh` action is the SDK's own: run it with `action("refresh")`.
  /** Opens the repository at `path` on this device. */
  open_repository(args: {
    path: string;
  }): Promise<void>;
  /** Runs `open_repository` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  open_repositoryMany(items: OpenRepositoryArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Starts choosing a repository's folder, listing this device's home folder. */
  choose_repository(): Promise<void>;
  /** Runs `choose_repository` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  choose_repositoryMany(items: ChooseRepositoryArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Opens the folder the picker shows as a repository. */
  open_browsed(): Promise<void>;
  /** Runs `open_browsed` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  open_browsedMany(items: OpenBrowsedArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Puts the picker away. */
  close_picker(): Promise<void>;
  /** Runs `close_picker` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  close_pickerMany(items: ClosePickerArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Stages one changed file. */
  stage_file(args: {
    c: Change;
  }): Promise<void>;
  /** Runs `stage_file` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  stage_fileMany(items: StageFileArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes one staged file back out of the next commit. */
  unstage_file(args: {
    c: Change;
  }): Promise<void>;
  /** Runs `unstage_file` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  unstage_fileMany(items: UnstageFileArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The key pressed on a file's row: `s` stages it or takes it back out of the next commit, and on a conflicted file `o` or `t` keeps our side or theirs. */
  change_on_key(args: {
    c: Change;
    key: string;
  }): Promise<void>;
  /** Runs `change_on_key` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  change_on_keyMany(items: ChangeOnKeyArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Discards the working tree's edits to one file, or deletes it when it was never tracked. */
  discard_file(args: {
    c: Change;
  }): Promise<void>;
  /** Runs `discard_file` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  discard_fileMany(items: DiscardFileArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Stages every change. */
  stage_all(): Promise<void>;
  /** Runs `stage_all` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  stage_allMany(items: StageAllArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes every staged change back out of the next commit. */
  unstage_all(): Promise<void>;
  /** Runs `unstage_all` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  unstage_allMany(items: UnstageAllArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Stages one hunk of a file. */
  stage_hunk(args: {
    c: Change;
    hunk: number;
  }): Promise<void>;
  /** Runs `stage_hunk` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  stage_hunkMany(items: StageHunkArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes one staged hunk back out of the next commit. */
  unstage_hunk(args: {
    c: Change;
    hunk: number;
  }): Promise<void>;
  /** Runs `unstage_hunk` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  unstage_hunkMany(items: UnstageHunkArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Discards one hunk of a file's working tree edits. */
  discard_hunk(args: {
    c: Change;
    hunk: number;
  }): Promise<void>;
  /** Runs `discard_hunk` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  discard_hunkMany(items: DiscardHunkArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Creates a commit from what is staged. */
  commit(args: {
    message: string;
  }): Promise<void>;
  /** Runs `commit` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  commitMany(items: CommitArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Keeps one side of a conflicted file and stages it as resolved. */
  resolve_conflict(args: {
    c: Change;
    ours: boolean;
  }): Promise<void>;
  /** Runs `resolve_conflict` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  resolve_conflictMany(items: ResolveConflictArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Creates a branch and switches to it. */
  create_branch(args: {
    name: string;
  }): Promise<void>;
  /** Runs `create_branch` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  create_branchMany(items: CreateBranchArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Switches to a branch, asking what becomes of uncommitted work when there is any. */
  checkout_branch(args: {
    name: string;
  }): Promise<void>;
  /** Runs `checkout_branch` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  checkout_branchMany(items: CheckoutBranchArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Answers the switch question by parking the uncommitted work on the stash first. */
  switch_stashing(): Promise<void>;
  /** Runs `switch_stashing` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  switch_stashingMany(items: SwitchStashingArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Answers the switch question by carrying the uncommitted work along. */
  switch_carrying(): Promise<void>;
  /** Runs `switch_carrying` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  switch_carryingMany(items: SwitchCarryingArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Puts the switch question away. */
  cancel_switch(): Promise<void>;
  /** Runs `cancel_switch` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  cancel_switchMany(items: CancelSwitchArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Switches to `name`, parking the uncommitted work when `stash` says so. */
  switch_to(args: {
    name: string;
    stash: boolean;
  }): Promise<void>;
  /** Runs `switch_to` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  switch_toMany(items: SwitchToArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Deletes a local branch. */
  delete_branch(args: {
    name: string;
  }): Promise<void>;
  /** Runs `delete_branch` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  delete_branchMany(items: DeleteBranchArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The key pressed on a branch's row: `Enter` switches to it, and `Delete` deletes it when it is a local branch that is not the one checked out. */
  branch_on_key(args: {
    name: string;
    is_remote: boolean;
    key: string;
  }): Promise<void>;
  /** Runs `branch_on_key` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  branch_on_keyMany(items: BranchOnKeyArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Stashes the working tree's changes, with a message. */
  save_stash(args?: {
    message?: string;
  }): Promise<void>;
  /** Runs `save_stash` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  save_stashMany(items: SaveStashArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Applies a stash, leaving it in the list. */
  apply_stash(args: {
    index: number;
  }): Promise<void>;
  /** Runs `apply_stash` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  apply_stashMany(items: ApplyStashArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Makes a new branch at `start` and a working directory of its own. */
  create_worktree(args: {
    path: string;
    branch: string;
    start: string;
  }): Promise<void>;
  /** Runs `create_worktree` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  create_worktreeMany(items: CreateWorktreeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Fetches from a remote. */
  fetch(args?: {
    remote?: string;
  }): Promise<void>;
  /** Runs `fetch` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  fetchMany(items: FetchArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Fetches, then fast-forwards the current branch to its upstream. */
  pull(args?: {
    remote?: string;
  }): Promise<void>;
  /** Runs `pull` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  pullMany(items: PullArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Pushes the current branch to a like-named branch on a remote. */
  push(args?: {
    remote?: string;
  }): Promise<void>;
  /** Runs `push` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  pushMany(items: PushArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Runs a check this project's host approved, by its id: the host picks the commands, the sources and the bounds, reports how far it has got, and says what it observed once it ends. */
  runApprovedCheck(args: {
    check_id: string;
  }): Promise<void>;
  /** Runs `run-approved-check` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  runApprovedCheckMany(items: RunApprovedCheckArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Asks the device how the running check is doing, and keeps what the host reported. */
  poll_check(): Promise<void>;
  /** Runs `poll_check` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  poll_checkMany(items: PollCheckArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Stops the running check; the host ends its commands. */
  cancel_check(): Promise<void>;
  /** Runs `cancel_check` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  cancel_checkMany(items: CancelCheckArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Puts the last check's result away. */
  clear_check(): Promise<void>;
  /** Runs `clear_check` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  clear_checkMany(items: ClearCheckArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Reads a commit's changed files and their diffs when it is opened. */
  read_commit(args: {
    id: string;
  }): Promise<void>;
  /** Runs `read_commit` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  read_commitMany(items: ReadCommitArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The key pressed on a commit's row: `Enter` opens its changed files, and `Backspace` or `Escape` steps back out of them, leaving the list of commits as it was. */
  commit_on_key(args: {
    id: string;
    key: string;
  }): Promise<void>;
  /** Runs `commit_on_key` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  commit_on_keyMany(items: CommitOnKeyArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Switches how this person reads diffs: one column or two. */
  toggle_layout(args: {
    s: Session;
    layout?: DiffLayout;
  }): Promise<void>;
  /** Runs `toggle_layout` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  toggle_layoutMany(items: ToggleLayoutArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Turns syntax colouring on or off for this person. */
  toggle_syntax(args: {
    s: Session;
    on?: boolean;
  }): Promise<void>;
  /** Runs `toggle_syntax` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  toggle_syntaxMany(items: ToggleSyntaxArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shows or hides the whitespace at the end of diff lines for this person. */
  toggle_whitespace(args: {
    s: Session;
    on?: boolean;
  }): Promise<void>;
  /** Runs `toggle_whitespace` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  toggle_whitespaceMany(items: ToggleWhitespaceArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Wraps long diff lines or lets them run wider than the pane, for this person. */
  toggle_wrap(args: {
    s: Session;
    wrap?: boolean;
  }): Promise<void>;
  /** Runs `toggle_wrap` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  toggle_wrapMany(items: ToggleWrapArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Puts the trouble banner away. */
  clear_trouble(): Promise<void>;
  /** Runs `clear_trouble` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  clear_troubleMany(items: ClearTroubleArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Puts the notice banner away. */
  clear_notice(): Promise<void>;
  /** Runs `clear_notice` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  clear_noticeMany(items: ClearNoticeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Runs `op` with `ask` and keeps the answer: the repository as it now stands, and the working tree as the device read it. */
  asks(args: {
    s: Session;
    op: string;
    ask: string;
  }): Promise<void>;
  /** Runs `asks` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  asksMany(items: AsksArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Repository` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const REPOSITORY_ACTIONS = ["open_repository", "open_repositoryMany", "choose_repository", "choose_repositoryMany", "open_browsed", "open_browsedMany", "close_picker", "close_pickerMany", "stage_file", "stage_fileMany", "unstage_file", "unstage_fileMany", "change_on_key", "change_on_keyMany", "discard_file", "discard_fileMany", "stage_all", "stage_allMany", "unstage_all", "unstage_allMany", "stage_hunk", "stage_hunkMany", "unstage_hunk", "unstage_hunkMany", "discard_hunk", "discard_hunkMany", "commit", "commitMany", "resolve_conflict", "resolve_conflictMany", "create_branch", "create_branchMany", "checkout_branch", "checkout_branchMany", "switch_stashing", "switch_stashingMany", "switch_carrying", "switch_carryingMany", "cancel_switch", "cancel_switchMany", "switch_to", "switch_toMany", "delete_branch", "delete_branchMany", "branch_on_key", "branch_on_keyMany", "save_stash", "save_stashMany", "apply_stash", "apply_stashMany", "create_worktree", "create_worktreeMany", "fetch", "fetchMany", "pull", "pullMany", "push", "pushMany", "runApprovedCheck", "runApprovedCheckMany", "poll_check", "poll_checkMany", "cancel_check", "cancel_checkMany", "clear_check", "clear_checkMany", "read_commit", "read_commitMany", "commit_on_key", "commit_on_keyMany", "toggle_layout", "toggle_layoutMany", "toggle_syntax", "toggle_syntaxMany", "toggle_whitespace", "toggle_whitespaceMany", "toggle_wrap", "toggle_wrapMany", "clear_trouble", "clear_troubleMany", "clear_notice", "clear_noticeMany", "asks", "asksMany"] as const;

/** The `tl.so.tilo.git-client.Repository` kind at `path`. */
export function repository(ws: tilo.Workspace, path: string): Repository {
  return resourceProxy(ws, path, SURFACE, REPOSITORY_ACTIONS) as unknown as Repository;
}

/** ApprovedCheck — `tl.so.tilo.git-client.ApprovedCheck`. */
export interface ApprovedCheck extends tilo.Resource {
  rmw3ex57: string;  // check_id
  nsdsyb5s: string;  // job
  ui5llo7z: CheckState;  // state
  d5rqal3v: string;  // phase
  poxyr3cm: number;  // completed
  eezoev2x: number;  // total
  u3i4t7tf: string;  // outcome
  dl6kn2wc: number | null;  // exit_code
  pllfspge: number;  // elapsed_ms
  vmj5rkri: string;  // output
  qmhx4rnw: string;  // errors
  rclt3d5w: string;  // refusal
}

/** The actions `ApprovedCheck` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const APPROVEDCHECK_ACTIONS = [] as const;

/** The `tl.so.tilo.git-client.ApprovedCheck` kind at `path`. */
export function approvedCheck(ws: tilo.Workspace, path: string): ApprovedCheck {
  return resourceProxy(ws, path, SURFACE, APPROVEDCHECK_ACTIONS) as unknown as ApprovedCheck;
}

/** Loaded — `tl.so.tilo.git-client.Loaded`. */
export interface Loaded extends tilo.Resource {
  u66layc7: string;  // answer
  dbwyzz4f: string;  // name
  bhabfybv: string;  // head
  ifdrbibf: string;  // workdir
  eptgnppa: string;  // author_name
  zqo4qf2p: string;  // author_email
  dijqjokd: string[];  // changes
  ylfrksiq: string[];  // commits
  lypvtyhb: string[];  // branches
  g4pu5gfp: string[];  // tags
  cvskkv3u: string[];  // stashes
  sq36vhyn: string[];  // remotes
  izkg5khe: string[];  // worktrees
  t3ptayvw: string | null;  // worktree_error
}

/** The actions `Loaded` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LOADED_ACTIONS = [] as const;

/** The `tl.so.tilo.git-client.Loaded` kind at `path`. */
export function loaded(ws: tilo.Workspace, path: string): Loaded {
  return resourceProxy(ws, path, SURFACE, LOADED_ACTIONS) as unknown as Loaded;
}

/** WireChange — `tl.so.tilo.git-client.WireChange`. */
export interface WireChange extends tilo.Resource {
  "37sxq5bs": string;  // path
  dvptb6yi: string | null;  // old_path
  m6m22ctl: ChangeArea;  // area
  rck5ywq4: ChangeKind;  // kind
  "7anyihpj": number;  // additions
  kvrcmb3v: number;  // deletions
  "7dddbyyj": boolean;  // binary
  zzjs6njo: number;  // ours
  ej3kja37: number;  // theirs
  tlea3vto: string;  // language
  hr5ogyyj: string;  // unified
  edfwguh5: string;  // left
  "2kp7i4ab": string;  // right
  h6xzvamk: string[];  // hunks
}

/** The actions `WireChange` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const WIRECHANGE_ACTIONS = [] as const;

/** The `tl.so.tilo.git-client.WireChange` kind at `path`. */
export function wireChange(ws: tilo.Workspace, path: string): WireChange {
  return resourceProxy(ws, path, SURFACE, WIRECHANGE_ACTIONS) as unknown as WireChange;
}

/** WireCommit — `tl.so.tilo.git-client.WireCommit`. */
export interface WireCommit extends tilo.Resource {
  ktz2fyrf: string;  // sha
  ugphvrb2: string;  // summary
  hg67fbg2: string;  // body
  cjxgeuao: string;  // author_name
  ypiqlu3b: string;  // author_email
  "3mge2zfz": number;  // time
  uyueykhv: number;  // age
  o4wtnyc7: string;  // date
  o5fnmdlp: string[];  // parents
  oz6i3qfk: string[];  // refs
  firuj3kf: number;  // column
}

/** The actions `WireCommit` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const WIRECOMMIT_ACTIONS = [] as const;

/** The `tl.so.tilo.git-client.WireCommit` kind at `path`. */
export function wireCommit(ws: tilo.Workspace, path: string): WireCommit {
  return resourceProxy(ws, path, SURFACE, WIRECOMMIT_ACTIONS) as unknown as WireCommit;
}

/** WireRef — `tl.so.tilo.git-client.WireRef`. */
export interface WireRef extends tilo.Resource {
  cgcdv4xh: string;  // name
  wlrzlclq: RefKind;  // kind
}

/** The actions `WireRef` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const WIREREF_ACTIONS = [] as const;

/** The `tl.so.tilo.git-client.WireRef` kind at `path`. */
export function wireRef(ws: tilo.Workspace, path: string): WireRef {
  return resourceProxy(ws, path, SURFACE, WIREREF_ACTIONS) as unknown as WireRef;
}

/** WireBranch — `tl.so.tilo.git-client.WireBranch`. */
export interface WireBranch extends tilo.Resource {
  iwhrqigk: string;  // name
  lnt6kiuo: boolean;  // is_head
  i4erkio3: boolean;  // is_remote
  ej7g53n3: string | null;  // upstream
  vupfshru: number;  // ahead
  koq4jc7k: number;  // behind
  mfbh6kyh: string;  // tip
}

/** The actions `WireBranch` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const WIREBRANCH_ACTIONS = [] as const;

/** The `tl.so.tilo.git-client.WireBranch` kind at `path`. */
export function wireBranch(ws: tilo.Workspace, path: string): WireBranch {
  return resourceProxy(ws, path, SURFACE, WIREBRANCH_ACTIONS) as unknown as WireBranch;
}

/** WireTag — `tl.so.tilo.git-client.WireTag`. */
export interface WireTag extends tilo.Resource {
  nsgs47is: string;  // name
  yn67zzls: string;  // target
  uju3pds5: string | null;  // message
}

/** The actions `WireTag` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const WIRETAG_ACTIONS = [] as const;

/** The `tl.so.tilo.git-client.WireTag` kind at `path`. */
export function wireTag(ws: tilo.Workspace, path: string): WireTag {
  return resourceProxy(ws, path, SURFACE, WIRETAG_ACTIONS) as unknown as WireTag;
}

/** WireStash — `tl.so.tilo.git-client.WireStash`. */
export interface WireStash extends tilo.Resource {
  zyrzfh5t: number;  // index
  bd32e2q6: string;  // message
  f3lmbvrn: string;  // branch
}

/** The actions `WireStash` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const WIRESTASH_ACTIONS = [] as const;

/** The `tl.so.tilo.git-client.WireStash` kind at `path`. */
export function wireStash(ws: tilo.Workspace, path: string): WireStash {
  return resourceProxy(ws, path, SURFACE, WIRESTASH_ACTIONS) as unknown as WireStash;
}

/** WireRemote — `tl.so.tilo.git-client.WireRemote`. */
export interface WireRemote extends tilo.Resource {
  d577aqii: string;  // name
  ihlupgbv: string;  // url
}

/** The actions `WireRemote` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const WIREREMOTE_ACTIONS = [] as const;

/** The `tl.so.tilo.git-client.WireRemote` kind at `path`. */
export function wireRemote(ws: tilo.Workspace, path: string): WireRemote {
  return resourceProxy(ws, path, SURFACE, WIREREMOTE_ACTIONS) as unknown as WireRemote;
}

/** WireWorktree — `tl.so.tilo.git-client.WireWorktree`. */
export interface WireWorktree extends tilo.Resource {
  htbitpma: string;  // name
  f3a3kvnf: string;  // path
  cqc2kufj: string;  // head
  dvbe2xkw: boolean;  // current
}

/** The actions `WireWorktree` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const WIREWORKTREE_ACTIONS = [] as const;

/** The `tl.so.tilo.git-client.WireWorktree` kind at `path`. */
export function wireWorktree(ws: tilo.Workspace, path: string): WireWorktree {
  return resourceProxy(ws, path, SURFACE, WIREWORKTREE_ACTIONS) as unknown as WireWorktree;
}

/** WireFile — `tl.so.tilo.git-client.WireFile`. */
export interface WireFile extends tilo.Resource {
  "44l2jnvk": string;  // path
  fr7rebu2: string | null;  // old_path
  qggzylu7: ChangeKind;  // kind
  b4js3rjp: number;  // additions
  ivsbutw6: number;  // deletions
  f4hq5otl: boolean;  // binary
  bjqxfixo: string;  // language
  o6jj727v: string;  // unified
  oc5lrtao: string;  // left
  vmcjzzz5: string;  // right
}

/** The actions `WireFile` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const WIREFILE_ACTIONS = [] as const;

/** The `tl.so.tilo.git-client.WireFile` kind at `path`. */
export function wireFile(ws: tilo.Workspace, path: string): WireFile {
  return resourceProxy(ws, path, SURFACE, WIREFILE_ACTIONS) as unknown as WireFile;
}

/** PathAsk — `tl.so.tilo.git-client.PathAsk`. */
export interface PathAsk extends tilo.Resource {
  aqyg5aef: string;  // path
  dioumhuy: boolean;  // whitespace
}

/** The actions `PathAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PATHASK_ACTIONS = [] as const;

/** The `tl.so.tilo.git-client.PathAsk` kind at `path`. */
export function pathAsk(ws: tilo.Workspace, path: string): PathAsk {
  return resourceProxy(ws, path, SURFACE, PATHASK_ACTIONS) as unknown as PathAsk;
}

/** FileAsk — `tl.so.tilo.git-client.FileAsk`. */
export interface FileAsk extends tilo.Resource {
  qahiprkz: string;  // path
  vywcliib: string;  // file
}

/** The actions `FileAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const FILEASK_ACTIONS = [] as const;

/** The `tl.so.tilo.git-client.FileAsk` kind at `path`. */
export function fileAsk(ws: tilo.Workspace, path: string): FileAsk {
  return resourceProxy(ws, path, SURFACE, FILEASK_ACTIONS) as unknown as FileAsk;
}

/** HunkAsk — `tl.so.tilo.git-client.HunkAsk`. */
export interface HunkAsk extends tilo.Resource {
  nh7yqayy: string;  // path
  vfjaobn2: string;  // file
  tprahdo6: number;  // hunk
  "7pgxgdw3": boolean;  // unstage
}

/** The actions `HunkAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const HUNKASK_ACTIONS = [] as const;

/** The `tl.so.tilo.git-client.HunkAsk` kind at `path`. */
export function hunkAsk(ws: tilo.Workspace, path: string): HunkAsk {
  return resourceProxy(ws, path, SURFACE, HUNKASK_ACTIONS) as unknown as HunkAsk;
}

/** CommitAsk — `tl.so.tilo.git-client.CommitAsk`. */
export interface CommitAsk extends tilo.Resource {
  ofstus7y: string;  // path
  bxihpw7v: string;  // message
}

/** The actions `CommitAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const COMMITASK_ACTIONS = [] as const;

/** The `tl.so.tilo.git-client.CommitAsk` kind at `path`. */
export function commitAsk(ws: tilo.Workspace, path: string): CommitAsk {
  return resourceProxy(ws, path, SURFACE, COMMITASK_ACTIONS) as unknown as CommitAsk;
}

/** BranchAsk — `tl.so.tilo.git-client.BranchAsk`. */
export interface BranchAsk extends tilo.Resource {
  fi4zi42q: string;  // path
  c4fbjyom: string;  // name
  l7meu2gh: boolean;  // checkout
}

/** The actions `BranchAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const BRANCHASK_ACTIONS = [] as const;

/** The `tl.so.tilo.git-client.BranchAsk` kind at `path`. */
export function branchAsk(ws: tilo.Workspace, path: string): BranchAsk {
  return resourceProxy(ws, path, SURFACE, BRANCHASK_ACTIONS) as unknown as BranchAsk;
}

/** CheckoutAsk — `tl.so.tilo.git-client.CheckoutAsk`. */
export interface CheckoutAsk extends tilo.Resource {
  klf3gt64: string;  // path
  rpchrkvk: string;  // name
  cuml3cz6: boolean;  // stash
}

/** The actions `CheckoutAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CHECKOUTASK_ACTIONS = [] as const;

/** The `tl.so.tilo.git-client.CheckoutAsk` kind at `path`. */
export function checkoutAsk(ws: tilo.Workspace, path: string): CheckoutAsk {
  return resourceProxy(ws, path, SURFACE, CHECKOUTASK_ACTIONS) as unknown as CheckoutAsk;
}

/** CommitFilesAsk — `tl.so.tilo.git-client.CommitFilesAsk`. */
export interface CommitFilesAsk extends tilo.Resource {
  "3w5wb2ew": string;  // path
  vbqr55tj: string;  // commit
}

/** The actions `CommitFilesAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const COMMITFILESASK_ACTIONS = [] as const;

/** The `tl.so.tilo.git-client.CommitFilesAsk` kind at `path`. */
export function commitFilesAsk(ws: tilo.Workspace, path: string): CommitFilesAsk {
  return resourceProxy(ws, path, SURFACE, COMMITFILESASK_ACTIONS) as unknown as CommitFilesAsk;
}

/** WorktreeAsk — `tl.so.tilo.git-client.WorktreeAsk`. */
export interface WorktreeAsk extends tilo.Resource {
  ukwgoqln: string;  // path
  py6thmfz: string;  // destination
  rwocyieo: string;  // branch
  ibmiafbe: string;  // start
}

/** The actions `WorktreeAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const WORKTREEASK_ACTIONS = [] as const;

/** The `tl.so.tilo.git-client.WorktreeAsk` kind at `path`. */
export function worktreeAsk(ws: tilo.Workspace, path: string): WorktreeAsk {
  return resourceProxy(ws, path, SURFACE, WORKTREEASK_ACTIONS) as unknown as WorktreeAsk;
}

/** StashSaveAsk — `tl.so.tilo.git-client.StashSaveAsk`. */
export interface StashSaveAsk extends tilo.Resource {
  yeg5b74s: string;  // path
  igkwc6uc: string;  // message
}

/** The actions `StashSaveAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const STASHSAVEASK_ACTIONS = [] as const;

/** The `tl.so.tilo.git-client.StashSaveAsk` kind at `path`. */
export function stashSaveAsk(ws: tilo.Workspace, path: string): StashSaveAsk {
  return resourceProxy(ws, path, SURFACE, STASHSAVEASK_ACTIONS) as unknown as StashSaveAsk;
}

/** StashApplyAsk — `tl.so.tilo.git-client.StashApplyAsk`. */
export interface StashApplyAsk extends tilo.Resource {
  y3nby6jr: string;  // path
  eoe3gfz5: number;  // index
}

/** The actions `StashApplyAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const STASHAPPLYASK_ACTIONS = [] as const;

/** The `tl.so.tilo.git-client.StashApplyAsk` kind at `path`. */
export function stashApplyAsk(ws: tilo.Workspace, path: string): StashApplyAsk {
  return resourceProxy(ws, path, SURFACE, STASHAPPLYASK_ACTIONS) as unknown as StashApplyAsk;
}

/** ResolveAsk — `tl.so.tilo.git-client.ResolveAsk`. */
export interface ResolveAsk extends tilo.Resource {
  yltk3frx: string;  // path
  o2dbxhal: string;  // file
  qvivrkzl: boolean;  // ours
}

/** The actions `ResolveAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const RESOLVEASK_ACTIONS = [] as const;

/** The `tl.so.tilo.git-client.ResolveAsk` kind at `path`. */
export function resolveAsk(ws: tilo.Workspace, path: string): ResolveAsk {
  return resourceProxy(ws, path, SURFACE, RESOLVEASK_ACTIONS) as unknown as ResolveAsk;
}

/** CheckStatus — `tl.so.tilo.git-client.CheckStatus`. */
export interface CheckStatus extends tilo.Resource {
  uygk57xo: string;  // state
  "55lyz5up": string;  // phase
  pl44hgyv: number | null;  // completed
  "5qgxxmf2": number | null;  // total
  jdbite5b: string;  // error
  lseynsql: string;  // outcome
  unt7o3nt: number | null;  // exit_code
  oo44f2s2: number;  // elapsed_ms
  phjhwilm: string;  // output
  hmbphto7: string;  // errors
  z5xx44zw: string;  // failure
}

/** The actions `CheckStatus` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CHECKSTATUS_ACTIONS = [] as const;

/** The `tl.so.tilo.git-client.CheckStatus` kind at `path`. */
export function checkStatus(ws: tilo.Workspace, path: string): CheckStatus {
  return resourceProxy(ws, path, SURFACE, CHECKSTATUS_ACTIONS) as unknown as CheckStatus;
}

/** RemoteAsk — `tl.so.tilo.git-client.RemoteAsk`. */
export interface RemoteAsk extends tilo.Resource {
  gifph57t: string;  // path
  tz3p3qeb: string;  // remote
}

/** The actions `RemoteAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const REMOTEASK_ACTIONS = [] as const;

/** The `tl.so.tilo.git-client.RemoteAsk` kind at `path`. */
export function remoteAsk(ws: tilo.Workspace, path: string): RemoteAsk {
  return resourceProxy(ws, path, SURFACE, REMOTEASK_ACTIONS) as unknown as RemoteAsk;
}

/** Git — `tl.so.tilo.git-client`. */
export interface GitClient extends tilo.Resource {
}

/** The actions `GitClient` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const GITCLIENT_ACTIONS = [] as const;

/** The `tl.so.tilo.git-client` kind at `path`. */
export function gitClient(ws: tilo.Workspace, path: string): GitClient {
  return resourceProxy(ws, path, SURFACE, GITCLIENT_ACTIONS) as unknown as GitClient;
}

