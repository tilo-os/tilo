/**
 * Types for Tilo Installer (`so.tilo.imager`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Sources` is a `Resource` of the app's `imager.sources`, made by its factory with the
 * resource's path (`imager/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.imager@4a37f2987b3e5ed3";

/** One of "btrfs", "ext4", "xfs". */
export type Filesystem = "btrfs" | "ext4" | "xfs";

export interface InstallProfile {
  purpose: string;
  hostname: string;
  user: UserAccount;
  locale: string;
  keyboard: string;
  timezone: string;
  filesystem: Filesystem;
  wifi_ssid: string;
  device_label: string;
  enroll_local_identity: boolean;
  prepared_setup: PreparedSetupExpectation | null;
}

export interface PreparedSetupExpectation {
  schema: number;
  sealed_bytes: number;
  ciphertext_sha256: string;
}

export interface RecentSource {
  id: string;
  rank: number;
  source: Source;
}

export interface Source {
  kind: SourceKind;
  path: string;
  name: string;
  size: number;
  label: string;
  installable: boolean;
  identity: SourceIdentity | null;
  expected_sha256: string | null;
}

export interface SourceIdentity {
  volume: number;
  file: number;
}

/** One of "IsoFile", "TiloImage". */
export type SourceKind = "IsoFile" | "TiloImage";

export interface UserAccount {
  full_name: string;
  username: string;
}

/** The arguments of `select-source`. */
export interface SelectSourceArgs {
  source: Source;
}

/** The arguments of `clear-source`. */
export interface ClearSourceArgs {
}

/** Image sources — `imager.sources`. */
export interface Sources extends tilo.Resource {
  readonly source: Source | null;
  readonly recent: Array<tilo.Entry<RecentSource>>;
  profile: InstallProfile;
  selectSource(args: {
    source: Source;
  }): Promise<void>;
  /** Runs `select-source` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  selectSourceMany(items: SelectSourceArgs[]): Promise<tilo.ActionAnswer[]>;
  clearSource(): Promise<void>;
  /** Runs `clear-source` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  clearSourceMany(items: ClearSourceArgs[]): Promise<tilo.ActionAnswer[]>;
  status(): Promise<void>;
  exportConfig(): Promise<void>;
  rescan(): Promise<void>;
  chooseDrive(args: {
    drive: string;
  }): Promise<void>;
}

/** The actions `Sources` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SOURCES_ACTIONS = ["selectSource", "selectSourceMany", "clearSource", "clearSourceMany", "status", "exportConfig", "rescan", "chooseDrive"] as const;

/** The `imager.sources` kind at `path`. */
export function sources(ws: tilo.Workspace, path: string): Sources {
  return resourceProxy(ws, path, SURFACE, SOURCES_ACTIONS) as unknown as Sources;
}

