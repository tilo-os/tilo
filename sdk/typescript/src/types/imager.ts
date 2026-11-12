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
 * longer answers a `Run`. A record is the object the wire carries, and an enum whose
 * cases carry fields is an object whose tag the app's schema names. An action whose
 * name is one the interface already has is run with `action("…")`, which the file
 * says where it is.
 */

import { resourceProxy } from "tilo-sdk";
import type * as tilo from "tilo-sdk";

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

/** One of "iso_file", "tilo_image". */
export type SourceKind = "iso_file" | "tilo_image";

export interface UserAccount {
  full_name: string;
  username: string;
}

/** Image sources — `imager.sources`. */
export interface Sources extends tilo.Resource {
  readonly source: Source | null;
  readonly recent: RecentSource[];
  profile: InstallProfile;
  selectSource(args: {
    source: Source;
  }): Promise<tilo.Run | void>;
  clearSource(): Promise<tilo.Run | void>;
  status(): Promise<void>;
  exportConfig(): Promise<void>;
  rescan(): Promise<void>;
  chooseDrive(args: {
    drive: string;
  }): Promise<void>;
}

/** The `imager.sources` kind at `path`. */
export function sources(ws: tilo.Workspace, path: string): Sources {
  return resourceProxy(ws, path) as unknown as Sources;
}

