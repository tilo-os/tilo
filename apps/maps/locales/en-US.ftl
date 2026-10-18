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
