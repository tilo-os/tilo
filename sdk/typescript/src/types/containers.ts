/**
 * Types for Containers (`so.tilo.containers`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Draft` is a `Resource` of the app's `tl.so.tilo.containers.Draft`, made by its factory with the
 * resource's path (`containers/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.containers@bd801114a22f5f9a";

/** The arguments of `clear_create`. */
export interface ClearCreateArgs {
}

/** The arguments of `fill`. */
export interface FillArgs {
  field: string;
  text: string;
}

/** The arguments of `begin_pull`. */
export interface BeginPullArgs {
  image: string;
}

/** The arguments of `poll_pull`. */
export interface PollPullArgs {
}

/** The arguments of `cancel_pull`. */
export interface CancelPullArgs {
  p: Pull;
}

/** The arguments of `dismiss_pull`. */
export interface DismissPullArgs {
  p: Pull;
}

/** Draft — `tl.so.tilo.containers.Draft`. */
export interface Draft extends tilo.Resource {
  mnoqvwjm: string;  // name
  kc5y7sq2: string;  // image
  gkz666im: string;  // ports
  "7khtp7xl": string;  // env
  utqr7b5l: string;  // volumes
  "3jkgyx2g": string;  // command
}

/** The actions `Draft` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DRAFT_ACTIONS = [] as const;

/** The `tl.so.tilo.containers.Draft` kind at `path`. */
export function draft(ws: tilo.Workspace, path: string): Draft {
  return resourceProxy(ws, path, SURFACE, DRAFT_ACTIONS) as unknown as Draft;
}

/** Pull — `tl.so.tilo.containers.Pull`. */
export interface Pull extends tilo.Resource {
  shb65p47: string;  // image
  gubc3b62: string;  // job
  hhqtcu4c: string[];  // lines
  jgmjny63: boolean;  // done
  wektgccg: string;  // failed
}

/** The actions `Pull` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PULL_ACTIONS = [] as const;

/** The `tl.so.tilo.containers.Pull` kind at `path`. */
export function pull(ws: tilo.Workspace, path: string): Pull {
  return resourceProxy(ws, path, SURFACE, PULL_ACTIONS) as unknown as Pull;
}

/** Containers — `tl.so.tilo.containers.Containers`. */
export interface Containers extends tilo.Resource {
  svwiioos: Draft;  // draft
  ilelnqwj: Array<tilo.Entry<Pull>>;  // pulls
  /** Clears the new-container form. */
  clear_create(): Promise<void>;
  /** Runs `clear_create` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  clear_createMany(items: ClearCreateArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Fills one field of the new-container form, as an agent or a script would: `name`, `image`, `ports`, `env`, `volumes` or `command`. */
  fill(args: {
    field: string;
    text: string;
  }): Promise<void>;
  /** Runs `fill` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  fillMany(items: FillArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Runs an image as a new container from the draft, and clears the form when the engine took it. */
  create_container(): Promise<void>;
  /** Shows what the engine holds of one kind. Each is the place's number key, pressed anywhere in the window. */
  show_containers(): Promise<void>;
  show_images(): Promise<void>;
  show_volumes(): Promise<void>;
  show_distrobox(): Promise<void>;
  /** Opens the new-container form, as `n` does from the window. */
  new_container_form(): Promise<void>;
  /** Makes a volume. */
  make_volume(args: {
    name: string;
  }): Promise<void>;
  /** Gives an image another reference. */
  tag_image(args: {
    image: string;
    tag: string;
  }): Promise<void>;
  /** Starts pulling an image, and keeps its output as it comes. */
  begin_pull(args: {
    image: string;
  }): Promise<void>;
  /** Runs `begin_pull` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  begin_pullMany(items: BeginPullArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Asks how far a pull has got; a finished or failed one says so and stays. */
  poll_pull(): Promise<void>;
  /** Runs `poll_pull` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  poll_pullMany(items: PollPullArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Ends a pull and the engine's process with it. */
  cancel_pull(args: {
    p: Pull;
  }): Promise<void>;
  /** Runs `cancel_pull` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  cancel_pullMany(items: CancelPullArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes a finished or failed pull out of the window. */
  dismiss_pull(args: {
    p: Pull;
  }): Promise<void>;
  /** Runs `dismiss_pull` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  dismiss_pullMany(items: DismissPullArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Containers` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CONTAINERS_ACTIONS = ["clear_create", "clear_createMany", "fill", "fillMany", "create_container", "show_containers", "show_images", "show_volumes", "show_distrobox", "new_container_form", "make_volume", "tag_image", "begin_pull", "begin_pullMany", "poll_pull", "poll_pullMany", "cancel_pull", "cancel_pullMany", "dismiss_pull", "dismiss_pullMany"] as const;

/** The `tl.so.tilo.containers.Containers` kind at `path`. */
export function containers(ws: tilo.Workspace, path: string): Containers {
  return resourceProxy(ws, path, SURFACE, CONTAINERS_ACTIONS) as unknown as Containers;
}

/** PullAsk — `tl.so.tilo.containers.PullAsk`. */
export interface PullAsk extends tilo.Resource {
  tukmkidh: string;  // image
}

/** The actions `PullAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PULLASK_ACTIONS = [] as const;

/** The `tl.so.tilo.containers.PullAsk` kind at `path`. */
export function pullAsk(ws: tilo.Workspace, path: string): PullAsk {
  return resourceProxy(ws, path, SURFACE, PULLASK_ACTIONS) as unknown as PullAsk;
}

/** JobAnswer — `tl.so.tilo.containers.JobAnswer`. */
export interface JobAnswer extends tilo.Resource {
  znalhivd: string;  // job
}

/** The actions `JobAnswer` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const JOBANSWER_ACTIONS = [] as const;

/** The `tl.so.tilo.containers.JobAnswer` kind at `path`. */
export function jobAnswer(ws: tilo.Workspace, path: string): JobAnswer {
  return resourceProxy(ws, path, SURFACE, JOBANSWER_ACTIONS) as unknown as JobAnswer;
}

/** JobState — `tl.so.tilo.containers.JobState`. */
export interface JobState extends tilo.Resource {
  h3ie7k7k: string;  // state
  bzkmqs4p: string[];  // lines
  a6sjrj6l: string;  // error
}

/** The actions `JobState` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const JOBSTATE_ACTIONS = [] as const;

/** The `tl.so.tilo.containers.JobState` kind at `path`. */
export function jobState(ws: tilo.Workspace, path: string): JobState {
  return resourceProxy(ws, path, SURFACE, JOBSTATE_ACTIONS) as unknown as JobState;
}

/** Containers — `tl.so.tilo.containers`. */
export interface TiloContainers extends tilo.Resource {
  /** Opens a shell in a running container: podman's, in the Tilo terminal. */
  open_shell(args: {
    container: string;
  }): Promise<void>;
  /** Opens a shell in a distrobox. */
  enter_box(args: {
    box: string;
  }): Promise<void>;
  /** Runs what the prompt asked, once the person pressed its verb. */
  prompt_run(args: {
    prompt: string;
    typed: string;
  }): Promise<void>;
  /** Does what the confirmation named. */
  confirm_run(args: {
    confirm: string;
  }): Promise<void>;
}

/** The actions `TiloContainers` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TILOCONTAINERS_ACTIONS = ["open_shell", "enter_box", "prompt_run", "confirm_run"] as const;

/** The `tl.so.tilo.containers` kind at `path`. */
export function tiloContainers(ws: tilo.Workspace, path: string): TiloContainers {
  return resourceProxy(ws, path, SURFACE, TILOCONTAINERS_ACTIONS) as unknown as TiloContainers;
}

