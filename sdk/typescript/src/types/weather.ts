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
export const SURFACE = "so.tilo.weather@3150b872b91bc7e1";

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

/** Weather — `weather.locations`. */
export interface Locations extends tilo.Resource {
  places: Place[];
  default_place: string;
  saveLocation(args: {
    place: PlaceDetails;
    make_default?: boolean;
  }): Promise<tilo.Run | void>;
  useAsDefault(args: {
    place: string;
  }): Promise<tilo.Run | void>;
  moveToTop(args: {
    place: string;
  }): Promise<tilo.Run | void>;
  // the app's `remove` action is the SDK's own: run it with `action("remove")`.
  // the app's `refresh` action is the SDK's own: run it with `action("refresh")`.
  refreshLocation(args: {
    location: string;
  }): Promise<void>;
  openLocation(args: {
    location: string;
  }): Promise<void>;
}

/** The `weather.locations` kind at `path`. */
export function locations(ws: tilo.Workspace, path: string): Locations {
  return resourceProxy(ws, path, SURFACE) as unknown as Locations;
}

