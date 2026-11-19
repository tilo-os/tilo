/**
 * Types for Weather (`so.tilo.weather`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Locations` is a `Resource` of the app's `weather.locations`, made by its factory with the
 * resource's path (`weather/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.weather@24a1f909df8266d0";

export interface Place {
  name: string;
  region: string;
  country: string;
  country_code: string;
  latitude: number;
  longitude: number;
  timezone: string;
}

export interface PlaceDetails {
  name: string;
  country: string;
  latitude: number;
  longitude: number;
  region: string | null;
  country_code: string | null;
  timezone: string | null;
}

/** The arguments of `save-location`. */
export interface SaveLocationArgs {
  place: PlaceDetails;
  make_default?: boolean;
}

/** The arguments of `use-as-default`. */
export interface UseAsDefaultArgs {
  place: string;
}

/** The arguments of `move-to-top`. */
export interface MoveToTopArgs {
  place: string;
}

/** Weather — `weather.locations`. */
export interface Locations extends tilo.Resource {
  places: Array<tilo.Entry<Place>>;
  default_place: string;
  saveLocation(args: {
    place: PlaceDetails;
    make_default?: boolean;
  }): Promise<void>;
  /** Runs `save-location` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  saveLocationMany(items: SaveLocationArgs[]): Promise<tilo.ActionAnswer[]>;
  useAsDefault(args: {
    place: string;
  }): Promise<void>;
  /** Runs `use-as-default` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  useAsDefaultMany(items: UseAsDefaultArgs[]): Promise<tilo.ActionAnswer[]>;
  moveToTop(args: {
    place: string;
  }): Promise<void>;
  /** Runs `move-to-top` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  moveToTopMany(items: MoveToTopArgs[]): Promise<tilo.ActionAnswer[]>;
  // the app's `remove` action is the SDK's own: run it with `action("remove")`.
  refreshAll(): Promise<void>;
}

/** The actions `Locations` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LOCATIONS_ACTIONS = ["saveLocation", "saveLocationMany", "useAsDefault", "useAsDefaultMany", "moveToTop", "moveToTopMany", "refreshAll"] as const;

/** The `weather.locations` kind at `path`. */
export function locations(ws: tilo.Workspace, path: string): Locations {
  return resourceProxy(ws, path, SURFACE, LOCATIONS_ACTIONS) as unknown as Locations;
}

