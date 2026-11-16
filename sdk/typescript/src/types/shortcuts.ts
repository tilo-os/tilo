/**
 * Types for Shortcuts (`so.tilo.shortcuts`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Workbench` is a `Resource` of the app's `shortcuts.workbench`, made by its factory with the
 * resource's path (`shortcuts/<resource>`): its attributes are the app's fields
 * (read-only ones `readonly`) and its methods are the app's actions, named in camel
 * case and taking the arguments the wire takes them under. An action that takes
 * longer answers a `Run`. A record is the object the wire carries, and an enum whose
 * cases carry fields is an object whose tag the app's schema names. An action whose
 * name is one the interface already has is run with `action("…")`, which the file
 * says where it is. An undoable action that answers nothing also has a `<action>Many`,
which applies it to every argument object of a list in a few all-or-nothing steps,
all of them one run.
 */

import { resourceProxy } from "tilo-sdk";
import type * as tilo from "tilo-sdk";

/** The version of the surface these types were made from: a workspace whose app
 * publishes another refuses their actions rather than run what they no longer
 * describe. */
export const SURFACE = "so.tilo.shortcuts@720723bfb1ff289c";

export interface Draft {
  name: string;
  shortcut_id: string;
  inputs: unknown[];
  steps: StepNode[];
  output: unknown | null;
  triggers: unknown[];
  step_timeout_secs: number | null;
  replaces_saved: boolean;
}

/** One of invoke, if, for_each, set_var, ask, ai, task, read: an object whose case the app tags it with. */
export type Head = Record<string, unknown>;

export interface StepNode {
  parent: string;
  branch: string;
  id: string;
  head: Head | null;
  args: Record<string, unknown>;
  on_error: unknown;
}

/** The arguments of `draft-proposal`. */
export interface DraftProposalArgs {
  shortcut: unknown;
}

/** The arguments of `ignore-proposal`. */
export interface IgnoreProposalArgs {
  key: string;
}

/** The arguments of `allow-proposal`. */
export interface AllowProposalArgs {
  key: string;
}

/** The arguments of `new-draft`. */
export interface NewDraftArgs {
  name: string;
}

/** The arguments of `duplicate-draft`. */
export interface DuplicateDraftArgs {
  draft: string;
}

/** The arguments of `draft-shortcut`. */
export interface DraftShortcutArgs {
  shortcut: unknown;
}

/** The arguments of `open-shortcut`. */
export interface OpenShortcutArgs {
  shortcut: unknown;
}

/** The arguments of `duplicate-shortcut`. */
export interface DuplicateShortcutArgs {
  shortcut: unknown;
}

/** The arguments of `set-schedule`. */
export interface SetScheduleArgs {
  draft: string;
  enabled: boolean;
  hour: number;
  minute: number;
}

/** The arguments of `discard`. */
export interface DiscardArgs {
  draft: string;
}

/** The arguments of `add-step`. */
export interface AddStepArgs {
  draft: string;
  block: string;
  step: unknown;
}

/** The arguments of `replace-step`. */
export interface ReplaceStepArgs {
  draft: string;
  path: string;
  step: unknown;
}

/** The arguments of `bind-argument`. */
export interface BindArgumentArgs {
  draft: string;
  path: string;
  param: string;
  binding: string;
}

/** The arguments of `remove-step`. */
export interface RemoveStepArgs {
  draft: string;
  path: string;
}

/** The arguments of `move-step`. */
export interface MoveStepArgs {
  draft: string;
  path: string;
  earlier: boolean;
}

/** The arguments of `set-step-policy`. */
export interface SetStepPolicyArgs {
  draft: string;
  path: string;
  policy: string;
}

/** The arguments of `set-nested`. */
export interface SetNestedArgs {
  draft: string;
  path: string;
  block: string;
  index: number;
}

/** The arguments of `set-output`. */
export interface SetOutputArgs {
  draft: string;
  binding: string;
}

/** The arguments of `add-input`. */
export interface AddInputArgs {
  draft: string;
  name: string;
  kind: string;
}

/** The arguments of `remove-input`. */
export interface RemoveInputArgs {
  draft: string;
  name: string;
}

/** Shortcuts — `shortcuts.workbench`. */
export interface Workbench extends tilo.Resource {
  drafts: Draft[];
  ignored: string[];
  draftProposal(args: {
    shortcut: unknown;
  }): Promise<tilo.Run | void>;
  /** Runs `draft-proposal` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  draftProposalMany(items: DraftProposalArgs[]): Promise<tilo.ActionAnswer[]>;
  ignoreProposal(args: {
    key: string;
  }): Promise<tilo.Run | void>;
  /** Runs `ignore-proposal` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  ignoreProposalMany(items: IgnoreProposalArgs[]): Promise<tilo.ActionAnswer[]>;
  allowProposal(args: {
    key: string;
  }): Promise<tilo.Run | void>;
  /** Runs `allow-proposal` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  allowProposalMany(items: AllowProposalArgs[]): Promise<tilo.ActionAnswer[]>;
  newDraft(args: {
    name: string;
  }): Promise<tilo.Run | void>;
  /** Runs `new-draft` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  newDraftMany(items: NewDraftArgs[]): Promise<tilo.ActionAnswer[]>;
  duplicateDraft(args: {
    draft: string;
  }): Promise<tilo.Run | void>;
  /** Runs `duplicate-draft` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  duplicateDraftMany(items: DuplicateDraftArgs[]): Promise<tilo.ActionAnswer[]>;
  draftShortcut(args: {
    shortcut: unknown;
  }): Promise<tilo.Run | void>;
  /** Runs `draft-shortcut` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  draftShortcutMany(items: DraftShortcutArgs[]): Promise<tilo.ActionAnswer[]>;
  openShortcut(args: {
    shortcut: unknown;
  }): Promise<tilo.Run | void>;
  /** Runs `open-shortcut` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  openShortcutMany(items: OpenShortcutArgs[]): Promise<tilo.ActionAnswer[]>;
  duplicateShortcut(args: {
    shortcut: unknown;
  }): Promise<tilo.Run | void>;
  /** Runs `duplicate-shortcut` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  duplicateShortcutMany(items: DuplicateShortcutArgs[]): Promise<tilo.ActionAnswer[]>;
  setSchedule(args: {
    draft: string;
    enabled: boolean;
    hour: number;
    minute: number;
  }): Promise<tilo.Run | void>;
  /** Runs `set-schedule` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  setScheduleMany(items: SetScheduleArgs[]): Promise<tilo.ActionAnswer[]>;
  discard(args: {
    draft: string;
  }): Promise<tilo.Run | void>;
  /** Runs `discard` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  discardMany(items: DiscardArgs[]): Promise<tilo.ActionAnswer[]>;
  addStep(args: {
    draft: string;
    block: string;
    step: unknown;
  }): Promise<tilo.Run | void>;
  /** Runs `add-step` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  addStepMany(items: AddStepArgs[]): Promise<tilo.ActionAnswer[]>;
  replaceStep(args: {
    draft: string;
    path: string;
    step: unknown;
  }): Promise<tilo.Run | void>;
  /** Runs `replace-step` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  replaceStepMany(items: ReplaceStepArgs[]): Promise<tilo.ActionAnswer[]>;
  bindArgument(args: {
    draft: string;
    path: string;
    param: string;
    binding: string;
  }): Promise<tilo.Run | void>;
  /** Runs `bind-argument` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  bindArgumentMany(items: BindArgumentArgs[]): Promise<tilo.ActionAnswer[]>;
  removeStep(args: {
    draft: string;
    path: string;
  }): Promise<tilo.Run | void>;
  /** Runs `remove-step` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  removeStepMany(items: RemoveStepArgs[]): Promise<tilo.ActionAnswer[]>;
  moveStep(args: {
    draft: string;
    path: string;
    earlier: boolean;
  }): Promise<tilo.Run | void>;
  /** Runs `move-step` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  moveStepMany(items: MoveStepArgs[]): Promise<tilo.ActionAnswer[]>;
  setStepPolicy(args: {
    draft: string;
    path: string;
    policy: string;
  }): Promise<tilo.Run | void>;
  /** Runs `set-step-policy` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  setStepPolicyMany(items: SetStepPolicyArgs[]): Promise<tilo.ActionAnswer[]>;
  setNested(args: {
    draft: string;
    path: string;
    block: string;
    index: number;
  }): Promise<tilo.Run | void>;
  /** Runs `set-nested` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  setNestedMany(items: SetNestedArgs[]): Promise<tilo.ActionAnswer[]>;
  setOutput(args: {
    draft: string;
    binding: string;
  }): Promise<tilo.Run | void>;
  /** Runs `set-output` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  setOutputMany(items: SetOutputArgs[]): Promise<tilo.ActionAnswer[]>;
  addInput(args: {
    draft: string;
    name: string;
    kind: string;
  }): Promise<tilo.Run | void>;
  /** Runs `add-input` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  addInputMany(items: AddInputArgs[]): Promise<tilo.ActionAnswer[]>;
  removeInput(args: {
    draft: string;
    name: string;
  }): Promise<tilo.Run | void>;
  /** Runs `remove-input` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  removeInputMany(items: RemoveInputArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The `shortcuts.workbench` kind at `path`. */
export function workbench(ws: tilo.Workspace, path: string): Workbench {
  return resourceProxy(ws, path, SURFACE) as unknown as Workbench;
}

