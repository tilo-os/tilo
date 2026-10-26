# Maps — source locale (en-US).

## History: what each change to the shared map did

maps-place-saved = { $new ->
    [true] Added { $item } to the saved places
   *[false] Removed { $item } from the saved places
}
maps-place-name = Renamed { $item }
maps-place-category = Changed what { $item } is
maps-place-address = Changed the address of { $item }
maps-workspace-places = Changed the places
maps-workspace-mode = Set directions to travel { $new ->
    [car] by car
    [transit] by public transport
    [bike] by bike
    [foot] on foot
   *[other] { $new }
}
maps-workspace-transit-time = Changed when transit journeys leave or arrive
maps-accept-places = Took in places a search found
maps-commit-search = Searched for { $query }
maps-clear-recents = Cleared the recent searches
maps-accept-route = Planned a route to { $dest_name }
maps-clear-route = Cleared the route
maps-accept-transit = Planned transit journeys to { $dest_name }
maps-clear-transit = Cleared the transit options

## What failed, said to the person who asked (the transport's own text is
## logged, never shown: a URL, an errno and an HTTP status are ours to read)

maps-failure = Couldn't finish { $what }. { $reason }
maps-failure-unreachable = There is no connection to the map service. Check your connection and try again.
maps-failure-timeout = The map service took too long to answer. Try again.
maps-failure-too-many = The map service is limiting how often this device may ask. Try again in a moment.
maps-failure-service = The map service is having trouble. Try again in a moment.
maps-failure-refused = The map service did not accept that request. Try a different place or route.
maps-failure-other = Something went wrong between here and the map service. Try again.

## Where you are, what a search found, and what the map is doing

maps-use-my-location = Use my location
maps-locating = Locating you…
maps-location-unavailable = Location unavailable
maps-search-results = { $count } { $count ->
    [one] result
   *[other] results
 } for “{ $query }”
maps-search-no-results = No results for “{ $query }”
maps-route-updated = Route updated
maps-route-choose-destination = Choose a destination to see the route
maps-route-another = Another route
maps-route-number = Route { $number } of { $count }
maps-departures-updated = Departures updated
maps-no-transit = No transit options found
maps-offline-downloading = Downloading { $count } tiles for offline use…
maps-offline-downloading-named = Downloading { $count } tiles for { $name }…
maps-offline-stopped = Stopped the download; the tiles already saved stay available.
