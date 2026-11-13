/**
 * Types for Git (`so.tilo.git-client`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Repo` is a `Resource` of the app's `git_client.repo`, made by its factory with the
 * resource's path (`git-client/<resource>`): its attributes are the app's fields
 * (read-only ones `readonly`) and its methods are the app's actions, named in camel
 * case and taking the arguments the wire takes them under. An action that takes
 * longer answers a `Run`. A record is the object the wire carries, and an enum whose
 * cases carry fields is an object whose tag the app's schema names. An action whose
 * name is one the interface already has is run with `action("…")`, which the file
 * says where it is.
 */

import { resourceProxy } from "tilo-sdk";
import type * as tilo from "tilo-sdk";

/** The version of the surface these types were made from: a workspace whose app
 * publishes another refuses their actions rather than run what they no longer
 * describe. */
export const SURFACE = "so.tilo.git-client@34cdca9d43811cee";

export interface Branch {
  name: string;
  is_head: boolean;
  is_remote: boolean;
  upstream: string | null;
  ahead: number;
  behind: number;
  tip: string;
}

/** One of "Staged", "Unstaged", "Untracked". */
export type ChangeArea = "Staged" | "Unstaged" | "Untracked";

/** One of "Added", "Modified", "Deleted", "Renamed", "Untracked", "Conflicted". */
export type ChangeKind = "Added" | "Modified" | "Deleted" | "Renamed" | "Untracked" | "Conflicted";

export interface Commit {
  id: string;
  summary: string;
  body: string;
  author_name: string;
  author_email: string;
  when: string;
  time: number;
  parents: string[];
  refs: RefLabel[];
  lane: number;
  files: FileChange[];
}

export interface FileChange {
  path: string;
  old_path: string | null;
  area: ChangeArea;
  kind: ChangeKind;
  additions: number;
  deletions: number;
}

/** One of "Head", "LocalBranch", "RemoteBranch", "Tag". */
export type RefKind = "Head" | "LocalBranch" | "RemoteBranch" | "Tag";

export interface RefLabel {
  name: string;
  kind: RefKind;
}

export interface Remote {
  name: string;
  url: string;
}

export interface SharedRepo {
  name: string;
  head: string;
  workdir: string;
  commits: Commit[];
  branches: Branch[];
  tags: Tag[];
  stashes: Stash[];
  remotes: Remote[];
  graph_width: number;
  author_name: string;
  author_email: string;
}

export interface Stash {
  index: number;
  message: string;
  branch: string;
}

export interface Tag {
  name: string;
  target: string;
  message: string | null;
}

/** Repository — `git_client.repo`. */
export interface Repo extends tilo.Resource {
  repository: string;
  readonly shared: unknown;
  recordRepository(args: {
    shared: SharedRepo;
  }): Promise<tilo.Run | void>;
  diffLayout(): Promise<void>;
  syntaxHighlight(): Promise<void>;
  showWhitespace(): Promise<void>;
  wrapLines(): Promise<void>;
}

/** The `git_client.repo` kind at `path`. */
export function repo(ws: tilo.Workspace, path: string): Repo {
  return resourceProxy(ws, path, SURFACE) as unknown as Repo;
}

