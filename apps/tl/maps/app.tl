app so.tilo.maps "Maps" (
    icon: .map_pin,
    category: .utility,
    tagline: "Finds places and plans routes on OpenStreetMap",
    keywords: ["map", "navigation", "directions", "route", "openstreetmap", "osm", "location", "places", "navigate", "apple maps", "gnome maps", "marble", "karte", "karten", "wegbeschreibung", "routenplaner", "stadtplan", "ort", "orte", "standort", "navi", "wege", "landkarte", "anfahrt"],
    web: ["tile.openstreetmap.org", "nominatim.openstreetmap.org", "overpass-api.de", "router.project-osrm.org", "api.transitous.org"],
    schemes: "geo",
)

/// What a place is: the marker's ink, the card's glyph, and the OpenStreetMap class it maps from.
enum Category {
    /// A landmark, a monument, a sight.
    landmark (icon: .compass)
    /// A café.
    cafe (icon: .coffee)
    /// A restaurant, a fast food place.
    restaurant (icon: .utensils)
    /// A bar or a pub.
    bar (icon: .coffee)
    /// A museum or a gallery.
    museum (icon: .image)
    /// A park or a garden.
    park (icon: .trees)
    /// A stop, a station, a bus stop.
    transit (icon: .bus)
    /// A hotel or a hostel.
    hotel (icon: .bed_double)
    /// A shop.
    shop (icon: .shopping_bag)
    /// A hospital, a clinic, a doctor.
    hospital (icon: .hospital)
    /// Anything else.
    other (icon: .map_pin)
}

/// How directions travel.
enum TravelMode {
    /// By car.
    car (icon: .car)
    /// By public transport.
    transit (icon: .bus)
    /// By bike.
    bike (icon: .route)
    /// On foot.
    foot (icon: .navigation)
}

/// A turn of a route.
enum Maneuver {
    depart
    continue_on
    slight_left
    left
    sharp_left
    slight_right
    right
    sharp_right
    uturn
    roundabout
    merging
    arrive
}

/// The way one leg of a public transport journey travels.
enum TransitMode {
    walk (icon: .navigation)
    bus (icon: .bus)
    tram (icon: .train_front)
    subway (icon: .train_front)
    rail (icon: .train_front)
    ferry (icon: .navigation)
}

/// A place the map knows: one a search found, one a person kept, one an agent accepted.
type Place (title: name, untitled: "Place") {
    /// What people read it as.
    name: Text
    /// The name the map service gives it, so what a search finds twice is one place.
    key: Text = ""
    category: Category = .other
    /// Its address, as the service writes it.
    address: Text = ""
    latitude: Float = 0.0
    longitude: Float = 0.0
    /// Whether the person kept it in their places.
    saved: Bool = false
    /// Whether the last search found it, rather than the person having kept it.
    found: Bool = false

    search {
        filter saved
        filter category
        sort name a_z
    }
}

/// One turn of a route, in the order the person takes them.
type Step (title: instruction) {
    instruction: Text
    distance_m: Float = 0.0
    maneuver: Maneuver = .continue_on
}

/// A route as directions found it: how far, how long, the line to draw, and its turns.
type Route (title: to_name) {
    mode: TravelMode = .car
    distance_m: Float = 0.0
    duration_s: Float = 0.0
    /// The places the line runs through, latitude then longitude in turn.
    geometry: List<Float>
    from_name: Text = ""
    to_name: Text = ""
    from_latitude: Float = 0.0
    from_longitude: Float = 0.0
    to_latitude: Float = 0.0
    to_longitude: Float = 0.0
    steps: List<Step>
}

/// One leg of a public transport journey.
type Leg (title: line) {
    mode: TransitMode = .walk
    line: Text = ""
    headsign: Text = ""
    from_name: Text = ""
    to_name: Text = ""
    dep: Text = ""
    arr: Text = ""
    duration_min: Int = 0
    num_stops: Int = 0
    colour: Text = ""
    text_colour: Text = ""
    agency: Text = ""
    /// Whether the times are the service's live times rather than its timetable.
    realtime: Bool = false
}

/// A journey public transport offers from one place to another.
type Journey (title: dep) {
    dep: Text = ""
    arr: Text = ""
    duration_min: Int = 0
    transfers: Int = 0
    legs: List<Leg>
}

/// When a person's transit journeys leave, or when they must arrive.
type TransitTime {
    arrive_by: Bool = false
    /// Minutes from now, or before now for an arrival.
    offset_min: Int = 0
}

/// A point of interest standing in what the person looks at, from OpenStreetMap.
type Poi (title: name) {
    name: Text
    key: Text = ""
    category: Category = .other
    latitude: Float = 0.0
    longitude: Float = 0.0
    address: Text = ""
    hours: Text = ""
    website: Text = ""
    phone: Text = ""
    /// What kind of thing it is, in its own words ("museum", "italian").
    kind: Text = ""
}

/// One search a person made, newest first: what the panel offers before they type.
type Recent (title: query) {
    query: Text
    at: Time
}

/// An area of the map this device keeps for offline use: its bounds, the zoom levels whose tiles
/// it keeps, and how far saving it has come.
type OfflineArea (title: name) {
    name: Text
    north: Float = 0.0
    south: Float = 0.0
    east: Float = 0.0
    west: Float = 0.0
    least_zoom: Int = 10
    most_zoom: Int = 15
    /// The tiles the area holds, as the map counts them.
    tiles: Int = 0
    /// How many of them are kept on the device.
    done: Int = 0
    /// Whether the person stopped its download; the tiles kept so far stay on the device.
    stopped: Bool = false

    search {
        sort name a_z
    }
}

/// The workspace's map: the places it knows, the searches it made, the directions it found and
/// the areas this device keeps offline. People, agents and other apps read and change it through
/// this package's actions.
type Atlas {
    /// Every place the map knows: the ones the person kept, and the ones a search found.
    places: Collection<Place>
    /// The searches the person made, newest first.
    recents: Collection<Recent>
    /// The points of interest standing around what the map shows.
    pois: Collection<Poi>
    /// The routes directions found, in the order they were offered.
    routes: Collection<Route>
    /// Which of the routes the map draws.
    route_at: Int = 0
    /// The journeys public transport offers, from the last time it was asked.
    journeys: Collection<Journey>
    /// Which journey the map draws.
    journey_at: Int = 0
    /// When transit journeys leave, or when they must arrive.
    transit_time: TransitTime
    /// How directions travel.
    mode: TravelMode = .car
    /// The last search's words, so the panel says what it shows.
    query: Text = ""
    /// What the map is doing, or why what was asked did not happen.
    status: Text = ""
    /// The place the map opens on, and returns to.
    centre_latitude: Float = 0.0
    centre_longitude: Float = 0.0
    /// How close the map opens: the tile service's own zoom levels.
    centre_zoom: Float = 3.0
    /// Whether the traffic layer is on.
    traffic: Bool = false
    /// Whether the person is following a route: the live card says the turn ahead.
    navigating: Bool = false
    /// The areas this device keeps for offline use.
    offline: Collection<OfflineArea>

    // Live while a route is followed: the bar and the lock screen of every device show its
    // `live` view until the person ends it.
    live when navigating

    search {
        filter traffic
        sort query a_z
    }
}

root atlas: Atlas

/// Marks every place a search found as no longer found, and forgets the ones nobody kept.
// The map's data, kept as the person works with it.
action forget_found(a: Atlas) (icon: .x, label: "Forget the places a search found") {
    for place in a.places.filter(p -> p.found) {
        if place.saved {
            place.found = false
        } else { a.places.remove(place) }
    }
}

/// Remembers a search, newest first, at most eight.
action remember(a: Atlas, query: Text) uses time (icon: .history, label: "Remember a search") {
    for old in a.recents.filter(r -> r.query.lower() == query.lower()) {
        a.recents.remove(old)
    }
    a.recents.insert(Recent(query: query, at: now))
    if let held = a.recents.find(r -> r.query == query) { a.recents.move(held, to: 0) }
}

/// Adds the places a search found to the map, keeping one the person already kept as theirs.
action take_in(a: Atlas, places: List<Place>) (icon: .map_pin, label: "Take in places") {
    for found in places {
        if let known = a.places.find(p -> p.key == found.key and found.key != "") {
            known.name = found.name
            known.address = found.address
            known.category = found.category
            known.latitude = found.latitude
            known.longitude = found.longitude
            if not known.saved {
                known.found = found.found
            }
        } else { a.places.insert(found) }
    }
}

/// Takes the places another app or an agent found into the map.
action accept_places(a: Atlas, places: List<Place>) (
    icon: .map_pin,
    label: "Accept places",
    did: "took in places a search found",
) {
    for one in places {
        if let known = a.places.find(p -> p.key == one.key and one.key != "") {
            known.name = one.name
            known.address = one.address
            known.category = one.category
            known.latitude = one.latitude
            known.longitude = one.longitude
            if not known.saved {
                known.found = one.found
            }
        } else { a.places.insert(one) }
    }
}

/// The route the map draws, when directions found one.
fn shown_route(a: Atlas) -> Option<Route> {
    a.routes.at(a.route_at)
}

/// The journey the map draws, when public transport offered one.
fn shown_journey(a: Atlas) -> Option<Journey> {
    a.journeys.at(a.journey_at)
}

/// The place `name` or `key` names, when the map knows it.
fn known_place(a: Atlas, name: Text) -> Option<Place> {
    a.places.find(p -> p.name.lower() == name.lower() or p.key != "" and p.key == name)
}

/// How far something is, as a person reads it.
fn distance_text(metres: Float) -> Text {
    if metres < 950.0 {
        "{metres.round()} m"
    } else {
        let tenths = (metres / 100.0).round()
        if tenths % 10 == 0 { "{tenths / 10} km" } else { "{tenths / 10}.{tenths.abs() % 10} km" }
    }
}

/// How long a trip takes, as a person reads it.
fn duration_text(seconds: Float) -> Text {
    let minutes = (seconds / 60.0).round()
    if minutes < 60 {
        "{minutes} min"
    } else {
        let hours = minutes / 60
        let rest = minutes % 60
        if rest == 0 { "{hours} h" } else { "{hours} h {rest} min" }
    }
}

/// What a category is called.
fn category_name(category: Category) -> Text {
    match category {
        .landmark => "Landmark",
        .cafe => "Café",
        .restaurant => "Restaurant",
        .bar => "Bar",
        .museum => "Museum",
        .park => "Park",
        .transit => "Transit stop",
        .hotel => "Hotel",
        .shop => "Shop",
        .hospital => "Hospital",
        .other => "Place",
    }
}

/// What a travel mode is called.
fn mode_name(mode: TravelMode) -> Text {
    match mode {
        .car => "Drive",
        .transit => "Transit",
        .bike => "Cycle",
        .foot => "Walk",
    }
}

/// How directions travel, in the order the panel offers them.
fn modes() -> List<TravelMode> {
    [.car, .transit, .bike, .foot]
}

/// The OpenStreetMap class a place is, as a category.
fn category_of(class: Text, kind: Text) -> Category {
    if class == "amenity" and kind == "cafe" {
        .cafe
    } else if class == "amenity"
        and (kind == "restaurant" or kind == "fast_food" or kind == "food_court") {
        .restaurant
    } else if class == "amenity" and (kind == "bar" or kind == "pub" or kind == "biergarten") {
        .bar
    } else if class == "amenity" and (kind == "hospital" or kind == "clinic" or kind == "doctors") {
        .hospital
    } else if class == "tourism" and (kind == "museum" or kind == "gallery") {
        .museum
    } else if class == "tourism" and (kind == "hotel" or kind == "hostel" or kind == "guest_house") {
        .hotel
    } else if class == "tourism" or class == "historic" {
        .landmark
    } else if class == "leisure" and (kind == "park" or kind == "garden") {
        .park
    } else if class == "shop" {
        .shop
    } else if class == "railway"
        or class == "public_transport"
        or class == "highway" and kind == "bus_stop" {
        .transit
    } else { .other }
}

/// What a geocoder's answer holds: the places it found, or why it has none.
// Reading what a map service answers, into the values the map holds.
type Places {
    found: List<Place>
    why: Text
}

/// What a routing service's answer holds: the routes it found, or why it has none.
type Routes {
    found: List<Route>
    why: Text
}

/// What a transit service's answer holds: the journeys it found, or why it has none.
type Journeys {
    found: List<Journey>
    why: Text
}

/// What a map opened on a place holds: the points of interest around it, or why it has none.
type Pois {
    found: List<Poi>
    why: Text
}

/// The value at `key` of a JSON object.
fn field_of(value: json.Value, key: Text) -> Option<json.Value> {
    match value {
        .object(fields) => fields.get(key),
        .array(items) => .none,
        .string(text) => .none,
        .number(number) => .none,
        .bool(flag) => .none,
        .null => .none,
    }
}

/// The text at `key` of a JSON object, or empty.
fn text_of(value: json.Value, key: Text) -> Text {
    match field_of(value, key) {
        .some(held) => match held {
            .string(text) => text,
            .object(fields) => "",
            .array(items) => "",
            .number(number) => "",
            .bool(flag) => "",
            .null => "",
        },
        .none => "",
    }
}

/// The text a value holds when it is a string, or empty.
fn string_of(value: Option<json.Value>) -> Text {
    match value {
        .some(held) => match held {
            .string(text) => text,
            .object(fields) => "",
            .array(items) => "",
            .number(number) => "",
            .bool(flag) => "",
            .null => "",
        },
        .none => "",
    }
}

/// The number a value holds when it is a number or a number written as text, or zero.
fn number_of(value: Option<json.Value>) -> Float {
    match value {
        .some(held) => match held {
            .number(number) => number,
            .string(text) => text.to_float() ?? 0.0,
            .object(fields) => 0.0,
            .array(items) => 0.0,
            .bool(flag) => 0.0,
            .null => 0.0,
        },
        .none => 0.0,
    }
}

/// The number at `key` of a JSON object, or zero.
fn number_at(value: json.Value, key: Text) -> Float {
    number_of(field_of(value, key))
}

/// The array at `key` of a JSON object.
fn list_at(value: json.Value, key: Text) -> List<json.Value> {
    array_of(field_of(value, key))
}

/// The array a value holds when it is one, or nothing.
fn array_of(value: Option<json.Value>) -> List<json.Value> {
    match value {
        .some(held) => match held {
            .array(items) => items,
            .object(fields) => [],
            .string(text) => [],
            .number(number) => [],
            .bool(flag) => [],
            .null => [],
        },
        .none => [],
    }
}

/// The number at position `at` of a JSON array, or zero: a coordinate pair's longitude and
/// latitude, which JSON writes as an array of two numbers rather than as named fields.
fn number_in(value: json.Value, at: Int) -> Float {
    match value {
        .array(items) => number_of(items.at(at)),
        .object(fields) => 0.0,
        .string(text) => 0.0,
        .number(number) => 0.0,
        .bool(flag) => 0.0,
        .null => 0.0,
    }
}

/// The flag a value of `key` holds when it is a bool, or false.
fn flag_at(value: json.Value, key: Text) -> Bool {
    match field_of(value, key) {
        .some(held) => match held {
            .bool(flag) => flag,
            .object(fields) => false,
            .array(items) => false,
            .string(text) => false,
            .number(number) => false,
            .null => false,
        },
        .none => false,
    }
}

/// The id a map service names a thing by, as text: OpenStreetMap writes it as a number, and a
/// key written with it is the same on every device only when the number keeps its digits.
fn id_at(value: json.Value, key: Text) -> Text {
    match field_of(value, key) {
        .some(held) => match held {
            .number(number) => number_text(number),
            .string(text) => text,
            .object(fields) => "",
            .array(items) => "",
            .bool(flag) => "",
            .null => "",
        },
        .none => "",
    }
}

/// A number as text without the `.0` a whole one would otherwise carry: `11` for 11.0.
fn number_text(number: Float) -> Text {
    let whole = number.round()
    if number == whole.to_float() { "{whole}" } else { "{number}" }
}

/// The array of a value itself, when it is one.
fn items_of(value: json.Value) -> List<json.Value> {
    match value {
        .array(items) => items,
        .object(fields) => [],
        .string(text) => [],
        .number(number) => [],
        .bool(flag) => [],
        .null => [],
    }
}

/// What a failed request means for the person who asked.
fn connection_word(status: Int) -> Text {
    if status == 0 {
        "There is no connection to the map service. Check your connection and try again."
    } else if status == 429 {
        "The map service is limiting how often this device may ask. Try again in a moment."
    } else if status >= 500 {
        "The map service is having trouble. Try again in a moment."
    } else {
        "The map service did not accept that request. Try a different place or route."
    }
}

/// The name a geocoder's answer reads as: its own name, else the street and number, else the
/// first part of its display name that is not a house number.
fn place_name_of(value: json.Value) -> Text {
    let named = text_of(value, "name")
    if named != "" {
        return named
    }
    let address = field_of(value, "address")
    let road = string_of(if let held = address { field_of(held, "road") } else { .none })
    if road == "" {
        for part in text_of(value, "display_name").split(",") {
            let word = part.trim()
            if word != "" and (word.to_float() ?? 0.0) == 0.0 {
                return word
            }
        }
        return "Place"
    }
    let number = if let held = address {
        string_of(field_of(held, "house_number"))
    } else { "" }
    if number == "" { road } else { "{road} {number}" }
}

/// The place a geocoder's answer describes.
fn place_of(value: json.Value) -> Place {
    Place(
        name: place_name_of(value),
        key: "osm-{text_of(value, "osm_type")}-{id_at(value, "osm_id")}",
        category: category_of(text_of(value, "class"), text_of(value, "type")),
        address: text_of(value, "display_name"),
        latitude: number_of(field_of(value, "lat")),
        longitude: number_of(field_of(value, "lon")),
    )
}

/// The places a geocoder's answer holds, each read into a Place.
fn places_of(body: Text, status: Int) -> Places {
    match json.parse(body) {
        .ok(value) => Places(found: items_of(value).map(item -> place_of(item)), why: ""),
        .err(why) => Places(found: [], why: connection_word(status)),
    }
}

/// The turn a routing service's maneuver names.
fn maneuver_of(kind: Text, modifier: Text) -> Maneuver {
    if kind == "depart" {
        .depart
    } else if kind == "arrive" {
        .arrive
    } else if kind == "roundabout" or kind == "rotary" {
        .roundabout
    } else if kind == "merge" {
        .merging
    } else if kind == "uturn" or modifier == "uturn" {
        .uturn
    } else if modifier == "sharp left" {
        .sharp_left
    } else if modifier == "left" {
        .left
    } else if modifier == "slight left" {
        .slight_left
    } else if modifier == "sharp right" {
        .sharp_right
    } else if modifier == "right" {
        .right
    } else if modifier == "slight right" { .slight_right } else { .continue_on }
}

/// A turn of a route as a person reads it.
fn turn_words(kind: Text, modifier: Text, road: Text) -> Text {
    match maneuver_of(kind, modifier) {
        .depart => if road == "" { "Start" } else { "Head out on {road}" },
        .continue_on => if road == "" { "Continue" } else { "Continue on {road}" },
        .merging => if road == "" { "Continue" } else { "Continue on {road}" },
        .left => if road == "" { "Turn left" } else { "Turn left onto {road}" },
        .sharp_left => if road == "" { "Sharp left" } else { "Sharp left onto {road}" },
        .slight_left => if road == "" { "Slight left" } else { "Slight left onto {road}" },
        .right => if road == "" { "Turn right" } else { "Turn right onto {road}" },
        .sharp_right => if road == "" { "Sharp right" } else { "Sharp right onto {road}" },
        .slight_right => if road == "" { "Slight right" } else { "Slight right onto {road}" },
        .uturn => "Make a U-turn",
        .roundabout => if road == "" {
            "At the roundabout, take the exit"
        } else {
            "At the roundabout, take the exit onto {road}"
        },
        .arrive => "Arrive at your destination",
    }
}

/// The route a routing service's answer describes, as far, as long, its line and its turns.
fn route_of(value: json.Value) -> Route {
    Route(
        distance_m: number_at(value, "distance"),
        duration_s: number_at(value, "duration"),
        geometry: route_points(value),
        steps: route_steps(value),
    )
}

/// The routes a routing service's answer holds, each read into a Route, with its own reasons
/// when it offers none.
fn routes_of(body: Text, status: Int) -> Routes {
    if status < 200 or status >= 300 {
        return Routes(found: [], why: connection_word(status))
    }
    match json.parse(body) {
        .ok(value) => {
            let offered = list_at(value, "routes")
            if offered.is_empty() {
                Routes(found: [], why: "The routing service offered no route between those places.")
            } else {
                Routes(found: offered.map(item -> route_of(item)), why: "")
            }
        }
        .err(why) => Routes(
            found: [],
            why: "The routing service answered something that is not a route.",
        ),
    }
}

/// The line a route's answer draws: its coordinates as latitude then longitude in turn.
fn route_points(value: json.Value) -> List<Float> {
    let points: List<Float> = []
    if let shape = field_of(value, "geometry") {
        for pair in list_at(shape, "coordinates") {
            points = points + [number_in(pair, 1), number_in(pair, 0)]
        }
    }
    points
}

/// The turn ahead: the first step that asks the person to do something, which is the one after
/// the step the route starts on; where the route only says where it starts, that step.
fn turn_ahead(steps: List<Step>) -> Option<Step> {
    for step in steps {
        if step.maneuver != .depart {
            return .some(step)
        }
    }
    steps.at(0)
}

/// The turns of one leg of a route, as the service wrote them, in order.
fn route_steps(value: json.Value) -> List<Step> {
    let steps: List<Step> = []
    for leg in list_at(value, "legs") {
        for step in list_at(leg, "steps") {
            let maneuver = field_of(step, "maneuver")
            let kind = if let held = maneuver { text_of(held, "type") } else { "" }
            let modifier = if let held = maneuver { text_of(held, "modifier") } else { "" }
            steps = steps
                + [
                    Step(
                        instruction: turn_words(kind, modifier, text_of(step, "name")),
                        distance_m: number_at(step, "distance"),
                        maneuver: maneuver_of(kind, modifier),
                    ),
                ]
        }
    }
    steps
}

/// The transit mode a MOTIS mode names.
fn transit_mode_of(mode: Text) -> TransitMode {
    let upper = mode.upper()
    if upper == "WALK" {
        .walk
    } else if upper == "TRAM" or upper == "STREETCAR" {
        .tram
    } else if upper == "SUBWAY" or upper == "METRO" {
        .subway
    } else if upper == "RAIL" or upper == "REGIONAL_RAIL" or upper == "HIGHSPEED_RAIL" {
        .rail
    } else if upper == "FERRY" { .ferry } else { .bus }
}

/// A timestamp as the clock a person reads: the hour and minute of an ISO time.
fn clock_of(time: Text) -> Text {
    if time.len() >= 16 {
        "{time.slice(11, 13)}:{time.slice(14, 16)}"
    } else { time }
}

/// A colour a transit service gives, as six hex digits without the mark.
fn hex_of(colour: Text) -> Text {
    let bare = colour.trim()
    if bare.starts_with("#") {
        let digits = bare.slice(1, bare.len())
        if digits.len() == 6 { digits } else { "" }
    } else if bare.len() == 6 { bare } else { "" }
}

/// The journey a transit service's answer describes, with its legs.
fn journey_of(value: json.Value) -> Journey {
    Journey(
        dep: clock_of(text_of(value, "startTime")),
        arr: clock_of(text_of(value, "endTime")),
        duration_min: (number_at(value, "duration") / 60.0).round(),
        transfers: number_at(value, "transfers").round(),
        legs: legs_of(value),
    )
}

/// The journeys a transit service's answer holds, each read into a Journey.
fn journeys_of(body: Text, status: Int) -> Journeys {
    if status < 200 or status >= 300 {
        return Journeys(found: [], why: connection_word(status))
    }
    match json.parse(body) {
        .ok(value) => {
            let offered = list_at(value, "itineraries")
            if offered.is_empty() {
                Journeys(found: [], why: "No public transport goes there at this time.")
            } else {
                Journeys(found: offered.map(item -> journey_of(item)), why: "")
            }
        }
        .err(why) => Journeys(
            found: [],
            why: "The transit service answered something that is not a journey.",
        ),
    }
}

/// The legs of a journey, as the transit service wrote them, in order.
fn legs_of(value: json.Value) -> List<Leg> {
    let legs: List<Leg> = []
    for leg in list_at(value, "legs") {
        let from = field_of(leg, "from")
        let to = field_of(leg, "to")
        legs = legs
            + [
                Leg(
                    mode: transit_mode_of(text_of(leg, "mode")),
                    line: text_of(leg, "routeShortName"),
                    headsign: text_of(leg, "headsign"),
                    from_name: if let held = from { text_of(held, "name") } else { "" },
                    to_name: if let held = to { text_of(held, "name") } else { "" },
                    dep: clock_of(text_of(leg, "startTime")),
                    arr: clock_of(text_of(leg, "endTime")),
                    duration_min: (number_at(leg, "duration") / 60.0).round(),
                    num_stops: list_at(leg, "intermediateStops").len(),
                    colour: hex_of(text_of(leg, "routeColor")),
                    text_colour: hex_of(text_of(leg, "routeTextColor")),
                    agency: text_of(leg, "agencyName"),
                    realtime: flag_at(leg, "realTime"),
                ),
            ]
    }
    legs
}

/// One Overpass element as a point of interest, when it has a name and a place.
fn poi_of(value: json.Value) -> Option<Poi> {
    let tags = field_of(value, "tags")
    let name = if let held = tags { text_of(held, "name") } else { "" }
    if name == "" {
        return .none
    }
    let at = if number_at(value, "lat") != 0.0 or number_at(value, "lon") != 0.0 {
        value
    } else if let centre = field_of(value, "center") { centre } else { value }
    let latitude = number_at(at, "lat")
    let longitude = number_at(at, "lon")
    if latitude == 0.0 and longitude == 0.0 {
        return .none
    }
    let class = ""
    let kind = ""
    let address = ""
    if let held = tags {
        for candidate in ["amenity", "tourism", "shop", "leisure", "railway", "public_transport"] {
            if class == "" {
                let named = text_of(held, candidate)
                if named != "" {
                    class = candidate
                    kind = named
                }
            }
        }
        let street = text_of(held, "addr:street")
        let number = text_of(held, "addr:housenumber")
        let city = text_of(held, "addr:city")
        address = if street != "" and number != "" { "{street} {number}" } else { street }
        if city != "" {
            address = if address == "" { city } else { "{address}, {city}" }
        }
    }
    .some(
        Poi(
            name: name,
            key: "osm-{text_of(value, "type")}-{id_at(value, "id")}",
            category: category_of(class, kind),
            latitude: latitude,
            longitude: longitude,
            address: address,
            hours: if let held = tags { text_of(held, "opening_hours") } else { "" },
            website: if let held = tags { text_of(held, "website") } else { "" },
            phone: if let held = tags { text_of(held, "phone") } else { "" },
            kind: if let held = tags { text_of(held, "cuisine") } else { "" },
        ),
    )
}

/// The points of interest an Overpass answer holds, each read into a Poi.
fn pois_of(body: Text, status: Int) -> Pois {
    if status < 200 or status >= 300 {
        return Pois(found: [], why: connection_word(status))
    }
    match json.parse(body) {
        .ok(value) => {
            let found: List<Poi> = []
            for element in list_at(value, "elements") {
                if let poi = poi_of(element) {
                    found = found + [poi]
                }
            }
            Pois(found: found, why: "")
        }
        .err(why) => Pois(found: [], why: "The map service answered something that is not a place."),
    }
}

/// Searches the map for the places `query` names, and shows what it found.
// Searching and showing places.
action search(a: Atlas, query: Text) uses web, time (
    icon: .search,
    label: "Search maps",
    did: "changed the map",
) {
    let words = query.trim()
    if words == "" {
        return
    }
    a.query = words
    a.status = "Searching for {words}…"
    for place in a.places.filter(p -> p.found) {
        if place.saved {
            place.found = false
        } else { a.places.remove(place) }
    }
    let address = "https://nominatim.openstreetmap.org/search?format=jsonv2&limit=8&addressdetails=1&q={encode(words)}"
    let answer = web.get(address)
    let found = places_of(answer.text(), answer.status)
    if found.why != "" {
        a.status = found.why
        return
    }
    for place in found.found {
        if place.latitude != 0.0 or place.longitude != 0.0 {
            if let known = a.places.find(p -> p.key == place.key and place.key != "") {
                known.name = place.name
                known.address = place.address
                known.category = place.category
                known.latitude = place.latitude
                known.longitude = place.longitude
                known.found = true
            } else {
                place.found = true
                a.places.insert(place)
            }
        }
    }
    for old in a.recents.filter(r -> r.query.lower() == words.lower()) {
        a.recents.remove(old)
    }
    if a.recents.len() >= 8 {
        if let oldest = a.recents.at(7) { a.recents.remove(oldest) }
    }
    a.recents.insert(Recent(query: words, at: now))
    if let held = a.recents.find(r -> r.query == words) { a.recents.move(held, to: 0) }
    if found.found.is_empty() {
        a.status = "No results for “{words}”"
    } else {
        a.status = "{found.found.len()} results for “{words}”"
    }
}

/// Takes the last search's results away.
action clear_search(a: Atlas) (icon: .x) {
    for place in a.places.filter(p -> p.found) {
        if place.saved {
            place.found = false
        } else { a.places.remove(place) }
    }
    a.query = ""
    a.status = ""
}

/// Takes the searches the person made away.
action clear_recents(a: Atlas) (
    icon: .trash_2,
    label: "Clear search history",
    did: "cleared the recent searches",
) {
    for old in a.recents {
        a.recents.remove(old)
    }
}

/// Keeps a place in the person's places, or takes it out.
action save_place(p: Place, saved: Bool = not p.saved) (
    icon: .star,
    label: "Save place",
    on: p.saved,
) {
    p.saved = saved
}

/// Renames a saved place; the name a search brought is the place's until the person gives it
/// another.
action rename_place(p: Place, name: Text) (icon: .pencil, label: "Rename", did: "renamed a place") {
    require name.trim() != "" "A place needs a name"
    p.name = name.trim()
}

/// Shows the place `place` on the map: the map opens on it.
action open_place(a: Atlas, place: Text) (
    icon: .map_pin,
    label: "Open place",
    did: "changed the map",
) {
    if let known = known_place(a, place) {
        a.centre_latitude = known.latitude
        a.centre_longitude = known.longitude
        a.centre_zoom = 15.0
        a.status = known.name
    } else {
        a.status = "That place is no longer on the map."
    }
}

/// Shows a point on the map, as a link or another app names it.
action show_point(a: Atlas, latitude: Float, longitude: Float) (
    icon: .locate_fixed,
    label: "Show a point on the map",
    did: "showed a point on the map",
) {
    if latitude < -90.0 or latitude > 90.0 or longitude < -180.0 or longitude > 180.0 {
        a.status = "That point is not on Earth."
        return
    }
    a.centre_latitude = latitude
    a.centre_longitude = longitude
    a.centre_zoom = 15.0
    a.status = ""
}

/// Finds the device and shows where it is.
action locate(a: Atlas) (
    icon: .locate_fixed,
    label: "Use my location",
    did: "found where the device is",
) {
    a.status = "Locating you…"
    match engine.call("maps.locate", "") {
        .ok(answer) => match json.parse(answer) {
            .ok(value) => {
                let lat = number_of(field_of(value, "lat"))
                let lon = number_of(field_of(value, "lon"))
                if lat == 0.0 and lon == 0.0 {
                    a.status = text_of(value, "why")
                } else {
                    a.centre_latitude = lat
                    a.centre_longitude = lon
                    a.centre_zoom = 15.0
                    a.status = "Located"
                    take_in(
                        a,
                        [
                            Place(
                                name: "Current location",
                                key: "device",
                                latitude: lat,
                                longitude: lon,
                            ),
                        ],
                    )
                }
            }
            .err(why) => {
                a.status = "Location unavailable"
            }
        },
        .err(why) => {
            a.status = why
        }
    }
}

/// Zooms the map a level in or out.
action zoom_map(a: Atlas, direction: Float, zoom: Float = a.centre_zoom + direction) (
    icon: .plus,
    label: "Zoom the map",
) {
    a.centre_zoom = zoom.max(2.0).min(19.0)
}

/// How the map's directions travel.
action set_mode(a: Atlas, mode: TravelMode) (
    icon: .route,
    label: "Travel by {mode_name(mode)}",
    did: "changed the map",
) {
    a.mode = mode
}

/// When public transport journeys leave, or when they must arrive.
action set_transit_time(a: Atlas, arrive_by: Bool, offset_min: Int) (
    icon: .clock,
    did: "changed when transit journeys leave or arrive",
) {
    a.transit_time.arrive_by = arrive_by
    a.transit_time.offset_min = offset_min
}

/// Plans a route between two places the person named, by the mode directions travel, and shows it.
// Directions.
action directions(a: Atlas, from_words: Text, to_words: Text) uses web, time (
    icon: .route,
    label: "Get directions",
    did: "changed the map",
) {
    let end_words = to_words.trim()
    if end_words == "" {
        a.status = "Choose a destination to see the route."
        return
    }
    a.status = "Routing…"
    let from = match known_place(a, from_words.trim()) {
        .some(known) => known,
        .none => {
            let words = from_words.trim()
            if words == "" {
                Place(
                    name: "Map centre",
                    key: "centre",
                    latitude: a.centre_latitude,
                    longitude: a.centre_longitude,
                )
            } else {
                let answer = web.get(
                    "https://nominatim.openstreetmap.org/search?format=jsonv2&limit=1&q={encode(words)}",
                )
                let found = places_of(answer.text(), answer.status)
                if found.why != "" {
                    a.status = found.why
                    return
                }
                match found.found.first() {
                    .some(place) => place,
                    .none => {
                        a.status = "No place is called “{words}”."
                        return
                    }
                }
            }
        }
    }
    let to = match known_place(a, end_words) {
        .some(known) => known,
        .none => {
            let answer = web.get(
                "https://nominatim.openstreetmap.org/search?format=jsonv2&limit=1&q={encode(end_words)}",
            )
            let found = places_of(answer.text(), answer.status)
            if found.why != "" {
                a.status = found.why
                return
            }
            match found.found.first() {
                .some(place) => place,
                .none => {
                    a.status = "No place is called “{end_words}”."
                    return
                }
            }
        }
    }
    if a.mode == .transit {
        a.status = "Finding departures…"
        let transit_address = "https://api.transitous.org/api/v1/plan?numItineraries=4&fromPlace={from.latitude},{from.longitude}&toPlace={to.latitude},{to.longitude}"
        if a.transit_time.arrive_by {
            transit_address = "{transit_address}&arriveBy=true"
        }
        let trips_answer = web.get(transit_address)
        let trips = journeys_of(trips_answer.text(), trips_answer.status)
        if trips.why != "" {
            a.status = trips.why
            return
        }
        for old in a.journeys {
            a.journeys.remove(old)
        }
        for journey in trips.found {
            a.journeys.insert(journey)
        }
        a.journey_at = 0
        a.mode = .transit
        if let first = a.journeys.at(0) {
            a.status = "Departures to {to.name}: {first.dep}–{first.arr}, {first.duration_min} min"
        } else {
            a.status = "No transit options found"
        }
        return
    }
    let profile = match a.mode {
        .car => "driving",
        .bike => "cycling",
        .foot => "foot",
        .transit => "driving",
    }
    let address = "https://router.project-osrm.org/route/v1/{profile}/{from.longitude},{from.latitude};{to.longitude},{to.latitude}?overview=full&geometries=geojson&steps=true&alternatives=true"
    let answer = web.get(address)
    let tours = routes_of(answer.text(), answer.status)
    if tours.why != "" {
        a.status = tours.why
        return
    }
    for old in a.routes {
        a.routes.remove(old)
    }
    for route in tours.found {
        route.mode = a.mode
        route.from_name = from.name
        route.to_name = to.name
        route.from_latitude = from.latitude
        route.from_longitude = from.longitude
        route.to_latitude = to.latitude
        route.to_longitude = to.longitude
        a.routes.insert(route)
    }
    a.route_at = 0
    a.navigating = false
    if let first = a.routes.at(0) {
        a.status = "Route to {to.name}: {distance_text(first.distance_m)}, {duration_text(first.duration_s)}"
        a.centre_latitude = first.to_latitude
        a.centre_longitude = first.to_longitude
        a.centre_zoom = 13.0
    }
}

/// Finds the journeys public transport offers between two places.
action plan_transit(a: Atlas, from: Place, to: Place) uses web, time (
    icon: .bus,
    label: "Departures",
    did: "changed the map",
) {
    a.status = "Finding departures…"
    let address = "https://api.transitous.org/api/v1/plan?numItineraries=4&fromPlace={from.latitude},{from.longitude}&toPlace={to.latitude},{to.longitude}"
    if a.transit_time.arrive_by {
        address = "{address}&arriveBy=true"
    }
    let answer = web.get(address)
    let trips = journeys_of(answer.text(), answer.status)
    if trips.why != "" {
        a.status = trips.why
        return
    }
    for old in a.journeys {
        a.journeys.remove(old)
    }
    for journey in trips.found {
        a.journeys.insert(journey)
    }
    a.journey_at = 0
    a.mode = .transit
    if let first = a.journeys.at(0) {
        a.status = "Departures to {to.name}: {first.dep}–{first.arr}, {first.duration_min} min"
    } else {
        a.status = "No transit options found"
    }
}

/// Takes the routes directions found as the map's, and shows the first.
action accept_route(a: Atlas, routes: List<Route>, from_name: Text, to_name: Text) (
    icon: .route,
    did: "changed the map",
) {
    for old in a.routes {
        a.routes.remove(old)
    }
    for found in routes {
        found.from_name = from_name
        found.to_name = to_name
        a.routes.insert(found)
    }
    a.route_at = 0
    a.navigating = false
    if let first = a.routes.at(0) {
        a.status = "Route to {to_name}: {distance_text(first.distance_m)}, {duration_text(first.duration_s)}"
        a.centre_latitude = first.to_latitude
        a.centre_longitude = first.to_longitude
        a.centre_zoom = 13.0
    }
}

/// Shows the next route directions offered for the same journey.
action another_route(a: Atlas, at: Int = a.route_at + 1) (icon: .route, label: "Another route") {
    if a.routes.len() > 1 {
        a.route_at = if at < a.routes.len() { at } else { 0 }
        a.status = "Route {a.route_at + 1} of {a.routes.len()}"
    }
}

/// Takes the route away.
action clear_route(a: Atlas) (icon: .x, label: "Clear route", did: "cleared the route") {
    for old in a.routes {
        a.routes.remove(old)
    }
    a.route_at = 0
    a.navigating = false
    a.status = "The route is cleared."
}

/// Takes the journeys public transport offered as the map's, and shows the first.
action accept_transit(a: Atlas, journeys: List<Journey>, to_name: Text) (
    icon: .bus,
    did: "changed the map",
) {
    for old in a.journeys {
        a.journeys.remove(old)
    }
    for found in journeys {
        a.journeys.insert(found)
    }
    a.journey_at = 0
    a.mode = .transit
    match a.journeys.at(0) {
        .some(first) => {
            a.status = "Departures to {to_name}: {first.dep}–{first.arr}, {first.duration_min} min"
        }
        .none => {
            a.status = "No transit options found"
        }
    }
}

/// Shows the next journey public transport offered.
action another_journey(a: Atlas, at: Int = a.journey_at + 1) (
    icon: .bus,
    label: "Another journey",
) {
    if a.journeys.len() > 1 {
        a.journey_at = if at < a.journeys.len() { at } else { 0 }
        a.status = "Journey option {a.journey_at + 1}"
    }
}

/// Takes the transit journeys away.
action clear_transit(a: Atlas) (
    icon: .x,
    label: "Clear transit routes",
    did: "cleared the transit options",
) {
    for old in a.journeys {
        a.journeys.remove(old)
    }
    a.journey_at = 0
    a.status = "The transit options are cleared."
}

/// Follows the route the map draws: the live card says the turn ahead.
action start_navigation(a: Atlas) (icon: .navigation, label: "Start navigation") {
    if a.routes.is_empty() {
        a.status = "Plan a route first."
        return
    }
    a.navigating = true
}

/// Stops following the route.
action end_navigation(a: Atlas) (icon: .x, label: "End navigation") {
    a.navigating = false
}

/// Shows what stands around the map, from OpenStreetMap.
// What stands around the map.
action find_nearby(a: Atlas) uses web (
    icon: .compass,
    label: "What is nearby",
    did: "looked for what is nearby",
) {
    a.status = "Loading places…"
    let side = 0.01
    let box = "{a.centre_latitude - side},{a.centre_longitude - side},{a.centre_latitude + side},{a.centre_longitude + side}"
    let query = "[out:json][timeout:20];(nwr[\"amenity\"~\"^(restaurant|cafe|bar|pub|fast_food|hospital|clinic|cinema|theatre)$\"]({box});nwr[\"shop\"]({box});nwr[\"tourism\"~\"^(museum|gallery|attraction|viewpoint|hotel|hostel)$\"]({box});nwr[\"leisure\"~\"^(park|garden)$\"]({box});nwr[\"railway\"~\"^(station|halt|tram_stop)$\"]({box});nwr[\"highway\"=\"bus_stop\"]({box}););out center 200;"
    let answer = web.get("https://overpass-api.de/api/interpreter?data={encode(query)}")
    let found = pois_of(answer.text(), answer.status)
    if found.why != "" {
        a.status = found.why
        return
    }
    for old in a.pois {
        a.pois.remove(old)
    }
    for poi in found.found {
        a.pois.insert(poi)
    }
    if found.found.is_empty() {
        a.status = "No places found"
    } else {
        a.status = "{found.found.len()} places nearby"
    }
}

/// Shows a point of interest the map found, and opens the map on it.
action open_poi(a: Atlas, poi: Text) (icon: .map_pin, label: "Open place of interest") {
    if let found = a.pois.find(p -> p.key == poi or p.name == poi) {
        take_in(
            a,
            [
                Place(
                    name: found.name,
                    key: found.key,
                    category: found.category,
                    address: found.address,
                    latitude: found.latitude,
                    longitude: found.longitude,
                    found: true,
                ),
            ],
        )
        a.centre_latitude = found.latitude
        a.centre_longitude = found.longitude
        a.centre_zoom = 16.0
        a.status = found.name
    } else {
        a.status = "That place is no longer in view"
    }
}

/// Shows the traffic layer on the map, or takes it away.
action toggle_traffic(a: Atlas, traffic: Bool = not a.traffic) (
    icon: .route,
    label: "Show traffic",
    on: a.traffic,
    did: "turned the traffic layer on or off",
) {
    a.traffic = traffic
}

/// Saves what the map shows for offline use: its box and the zoom levels whose tiles it keeps.
// Offline maps.
action download_view(a: Atlas, least_zoom: Int = 10, most_zoom: Int = 15) (
    icon: .cloud_download,
    label: "Download this view",
    did: "saved what the map shows for offline use",
) {
    let name = if a.query == "" { "This view" } else { a.query }
    let north = a.centre_latitude + 0.02
    let south = a.centre_latitude - 0.02
    let east = a.centre_longitude + 0.03
    let west = a.centre_longitude - 0.03
    let area = OfflineArea(
        name: name,
        north: north,
        south: south,
        east: east,
        west: west,
        least_zoom: least_zoom,
        most_zoom: most_zoom,
        tiles: count_tiles(north, south, east, west, least_zoom, most_zoom),
    )
    for old in a.offline.filter(o -> o.name.lower() == name.lower()) {
        a.offline.remove(old)
    }
    a.offline.insert(area)
    a.status = "Downloading {area.tiles} tiles for {name}…"
}

/// Saves the area `query` names for offline use, with the zoom levels whose tiles it keeps.
action download_place(a: Atlas, query: Text, least_zoom: Int = 10, most_zoom: Int = 15) uses web (
    icon: .cloud_download,
    label: "Download {query}",
    did: "changed the map",
) {
    let address = "https://nominatim.openstreetmap.org/search?format=jsonv2&limit=1&q={encode(query)}"
    let answer = web.get(address)
    let found = places_of(answer.text(), answer.status)
    if found.why != "" {
        a.status = found.why
        return
    }
    if let first = found.found.first() {
        let name = first.name
        let north = first.latitude + 0.05
        let south = first.latitude - 0.05
        let east = first.longitude + 0.07
        let west = first.longitude - 0.07
        let area = OfflineArea(
            name: name,
            north: north,
            south: south,
            east: east,
            west: west,
            least_zoom: least_zoom,
            most_zoom: most_zoom,
            tiles: count_tiles(north, south, east, west, least_zoom, most_zoom),
        )
        for old in a.offline.filter(o -> o.name.lower() == name.lower()) {
            a.offline.remove(old)
        }
        a.offline.insert(area)
        a.status = "Downloading {area.tiles} tiles for {name}…"
    } else {
        a.status = "No place is called “{query}”."
    }
}

/// How many tiles an area holds at the zoom levels it keeps.
fn count_tiles(
    north: Float,
    south: Float,
    east: Float,
    west: Float,
    least_zoom: Int,
    most_zoom: Int,
) -> Int {
    if least_zoom > most_zoom {
        0
    } else {
        let columns = columns_at(least_zoom)
        let across = longitude_tile(east, columns) - longitude_tile(west, columns) + 1
        let down = latitude_tile(south, columns) - latitude_tile(north, columns) + 1
        across.max(1) * down.max(1)
            + count_tiles(north, south, east, west, least_zoom + 1, most_zoom)
    }
}

/// How many tiles the world is across at zoom level `z`.
fn columns_at(z: Int) -> Int {
    if z <= 0 { 1 } else { 2 * columns_at(z - 1) }
}

/// The tile column a longitude stands in.
fn longitude_tile(longitude: Float, columns: Int) -> Int {
    ((longitude + 180.0) / 360.0 * columns.to_float()).floor()
}

/// The tile row a latitude stands in.
fn latitude_tile(latitude: Float, columns: Int) -> Int {
    let clamped = latitude.max(-85.0511).min(85.0511)
    let sine = (clamped * 3.14159265 / 180.0).sin()
    let share = 0.5 - ((1.0 + sine) / (1.0 - sine)).ln() / (4.0 * 3.14159265)
    (share * columns.to_float()).floor()
}

/// Says how far saving an area has come, as the map reports it.
action note_saved(a: Atlas, area: Text, at: List<Int>) (did: "changed the map") {
    if let held = a.offline.find(o -> o.name == area and not o.stopped) {
        held.done = at.at(0) ?? 0
        let total = at.at(1) ?? 0
        if held.done >= total {
            held.tiles = total
            a.status = "Offline area ready ({total} tiles)"
        }
    }
}

/// Stops an area's download: the tiles kept so far stay available offline.
action stop_offline_area(a: Atlas, area: Text) (icon: .x, label: "Stop downloading") {
    if let held = a.offline.find(o -> o.name == area) {
        held.stopped = true
        a.status = "Stopped {area}; {held.done} tiles stay available offline"
    }
}

/// Starts a stopped area's download again, keeping what it already has.
action continue_offline_area(a: Atlas, area: Text) (
    icon: .cloud_download,
    label: "Download it again",
) {
    if let held = a.offline.find(o -> o.name == area) {
        held.stopped = false
        a.status = "Downloading the rest of {area}…"
    }
}

/// Takes an area this device saved away.
destructive action delete_offline_area(a: Atlas, area: Text) (
    icon: .trash_2,
    label: "Delete offline area",
    did: "changed the map",
) {
    if let held = a.offline.find(o -> o.name == area) {
        a.offline.remove(held)
        a.status = "Removed {area}"
    } else {
        a.status = "That offline area is not on this device"
    }
}

/// Takes every area this device saved away.
destructive action clear_offline_areas(a: Atlas) (
    icon: .trash_2,
    label: "Clear offline maps",
    did: "cleared the offline maps",
) {
    for held in a.offline {
        a.offline.remove(held)
    }
    a.status = "Cleared the offline maps"
}

/// Opens a `geo:` link: its search (`geo:0,0?q=cafe`), else its point (`geo:52.52,13.40`).
// Links and addresses.
action open_link(a: Atlas, link: Text) -> host.Resource uses web, time (
    icon: .map_pin,
    label: "Open link",
) {
    let rest = link.slice(4, link.len())
    if rest.contains("q=") {
        let asked = link.split("q=").at(1) ?? ""
        let words = decode(asked.split("&").at(0) ?? "").trim()
        if words != "" {
            a.query = words
            for place in a.places.filter(p -> p.found) {
                if place.saved {
                    place.found = false
                } else { a.places.remove(place) }
            }
            let answer = web.get(
                "https://nominatim.openstreetmap.org/search?format=jsonv2&limit=8&addressdetails=1&q={encode(words)}",
            )
            let found = places_of(answer.text(), answer.status)
            if found.why != "" {
                a.status = found.why
            } else {
                for place in found.found {
                    if place.latitude != 0.0 or place.longitude != 0.0 {
                        if let known = a.places.find(p -> p.key == place.key and place.key != "") {
                            known.name = place.name
                            known.address = place.address
                            known.category = place.category
                            known.latitude = place.latitude
                            known.longitude = place.longitude
                            if not known.saved {
                                known.found = true
                            }
                        } else {
                            place.found = true
                            a.places.insert(place)
                        }
                    }
                }
                for old in a.recents.filter(r -> r.query.lower() == words.lower()) {
                    a.recents.remove(old)
                }
                a.recents.insert(Recent(query: words, at: now))
                if let held = a.recents.find(r -> r.query == words) { a.recents.move(held, to: 0) }
            }
        }
    } else {
        let point = rest.split("?").at(0) ?? ""
        let latitude = (point.split(",").at(0) ?? "").to_float()
        let longitude = (point.split(",").at(1) ?? "").to_float()
        if let lat = latitude {
            if let lon = longitude {
                if lat < -90.0 or lat > 90.0 or lon < -180.0 or lon > 180.0 {
                    a.status = "That point is not on Earth."
                } else {
                    a.centre_latitude = lat
                    a.centre_longitude = lon
                    a.centre_zoom = 15.0
                    a.status = ""
                }
            }
        } else {
            a.status = "That point is not on Earth."
        }
    }
    a
}

/// The text a link writes its search with, read as the person typed it: `+` as a space and
/// `%20` as one.
fn decode(text: Text) -> Text {
    text.trim().replace("+", " ").replace("%20", " ")
}

/// The text a request's query is written with: the characters that would end it written so the
/// service reads them as part of it.
fn encode(text: Text) -> Text {
    text
        .trim()
        .replace("%", "%25")
        .replace("&", "%26")
        .replace("?", "%3F")
        .replace("#", "%23")
        .replace("+", "%2B")
        .replace(" ", "%20")
}

// The window.
default view map for Atlas

/// The map window: the map, its places beside it, and what a person does on the map.
view map(a: Atlas) {
    state chosen: Text = ""
    state sheet: Text = ""
    state show_places: Bool = true
    state least_zoom: Int = 10
    state most_zoom: Int = 15
    layers(fill) {
        split(side: .leading) {
            primary(min: 20rem) {
                if show_places {
                    places_side(a, chosen)
                }
            }
            secondary(min: 22rem) {
                layers(fill) {
                    map_pane(a, chosen)
                    map_controls(a, chosen, sheet, least_zoom, most_zoom)
                    if a.status != "" {
                        stack(at: .top, fill, padding: .related) {
                            row(gap: .tight, align: .center) {
                                ui.StatusLine(text: a.status)
                                ui.Button("Dismiss", icon: .x, icon_only: true, role: .plain) {
                                    on press: clear_search(a)
                                }
                            }
                        }
                    }
                    if a.navigating {
                        stack(at: .bottom, fill, padding: .related) {
                            turn_card(a)
                        }
                    }
                    if let picked = a.places.find(p -> p.name == chosen) {
                        stack(at: .bottom, fill, padding: .related) {
                            place_card(picked, chosen)
                        }
                    }
                    footer(a)
                }
            }
        }
        if sheet == "offline" {
            offline_sheet(a, sheet, least_zoom, most_zoom)
        } else if sheet == "info" {
            info_dialog(sheet)
        }
    }
}

/// The map itself: the world's imagery, its markers, the route, and the areas kept offline.
view map_pane(a: Atlas, chosen: State<Text>) {
    ui.Map(
        name: "Map",
        world: true,
        fill,
        latitude: a.centre_latitude,
        longitude: a.centre_longitude,
        zoom: a.centre_zoom,
        basemap: "https://tile.openstreetmap.org/%z/%x/%y.png",
        hint: "Search for a place to see it on the map",
    ) {
        on marker(name: Text): set(chosen, name).then(open_place(atlas, name))
        on saved(area: Text, at: List<Int>): note_saved(a, area, at)
        markers: {
            for place in a.places.filter(p -> p.saved or p.found) {
                ui.MapMarker(
                    name: place.name,
                    latitude: place.latitude,
                    longitude: place.longitude,
                    kind: if place.category == .landmark {
                        .landmark
                    } else if place.category == .cafe {
                        .cafe
                    } else if place.category == .restaurant {
                        .restaurant
                    } else if place.category == .bar {
                        .bar
                    } else if place.category == .museum {
                        .museum
                    } else if place.category == .park {
                        .park
                    } else if place.category == .transit {
                        .transit
                    } else if place.category == .hotel {
                        .hotel
                    } else if place.category == .shop {
                        .shop
                    } else if place.category == .hospital { .hospital } else { .other },
                    selected: place.name == chosen,
                    presence: lenses(place).find(l -> not l.mine)?.who,
                )
            }
            for place in a.pois {
                ui.MapMarker(
                    name: place.name,
                    latitude: place.latitude,
                    longitude: place.longitude,
                    kind: if place.category == .landmark {
                        .landmark
                    } else if place.category == .cafe {
                        .cafe
                    } else if place.category == .restaurant {
                        .restaurant
                    } else if place.category == .bar {
                        .bar
                    } else if place.category == .museum {
                        .museum
                    } else if place.category == .park {
                        .park
                    } else if place.category == .transit {
                        .transit
                    } else if place.category == .hotel {
                        .hotel
                    } else if place.category == .shop {
                        .shop
                    } else if place.category == .hospital { .hospital } else { .other },
                    presence: lenses(place).find(l -> not l.mine)?.who,
                )
            }
        }
        tracks: {
            if let route = shown_route(a) {
                ui.MapTrack(name: route.to_name, points: route.geometry, tone: .route, width: 4.0)
            }
        }
        areas: {
            for area in a.offline.filter(o -> not o.stopped) {
                ui.MapArea(
                    name: area.name,
                    north: area.north,
                    south: area.south,
                    east: area.east,
                    west: area.west,
                    least_zoom: area.least_zoom,
                    most_zoom: area.most_zoom,
                )
            }
        }
    }
}

/// The places beside the map: what a search found, what stands nearby, the places the person
/// kept, the searches they made, and directions.
view places_side(a: Atlas, chosen: State<Text>) {
    state place: Text = "found"
    state words: Text = ""
    ui.Docked {
        foot: {
            directions_panel(a)
        }
        stack(gap: .related, fill, padding: .related) {
            row(gap: .tight, align: .center) {
                ui.TextInput(bind: words, placeholder: "Search places", fill) {
                    on submit(text: Text): search(a, text)
                }
                ui.SearchButton {
                }
            }
            ui.ToggleGroup(
                selected: if place == "found" {
                    0
                } else if place == "nearby" { 1 } else if place == "saved" { 2 } else { 3 },
            ) {
                on change(at: Int): set(
                    place,
                    if at == 0 {
                        "found"
                    } else if at == 1 { "nearby" } else if at == 2 { "saved" } else { "recents" },
                )
                ui.Segment("Results")
                ui.Segment("Nearby")
                ui.Segment("Saved")
                ui.Segment("Recent")
            }
            if place == "found" {
                if a.query == "" {
                    ui.EmptyState("Search for a place or an address to see it here.", icon: .search)
                } else if a.places.filter(p -> p.found).is_empty() {
                    ui.EmptyState("No places found", icon: .search)
                } else {
                    stack(gap: .tight) {
                        for found in a.places.filter(p -> p.found) {
                            place_row(found, chosen)
                        }
                    }
                }
            } else if place == "nearby" {
                if a.pois.is_empty() {
                    ui.EmptyState(
                        "Nothing nearby yet",
                        message: "Look for what stands around the map.",
                        icon: .compass,
                    ) {
                        on act: find_nearby(a)
                    }
                } else {
                    stack(gap: .tight) {
                        for found in a.pois {
                            poi_row(found)
                        }
                    }
                }
            } else if place == "saved" {
                if a.places.filter(p -> p.saved).is_empty() {
                    ui.EmptyState(
                        "No saved places yet",
                        message: "Save a place to find it here.",
                        icon: .star,
                    )
                } else {
                    stack(gap: .tight) {
                        for kept in a.places.filter(p -> p.saved) {
                            ui.ListItem(
                                item: kept,
                                presence: lenses(kept).find(l -> not l.mine)?.who,
                            ) {
                                row: {
                                    place_body(kept, chosen)
                                }
                            }
                        }
                    }
                }
            } else if a.recents.is_empty() {
                ui.EmptyState("No recent searches yet", icon: .history)
            } else {
                stack(gap: .tight) {
                    row(distribute: .between, align: .center) {
                        ui.Text("Recent searches", role: .subheading)
                        ui.Button("Clear", role: .plain) {
                            on press: clear_recents(a)
                        }
                    }
                    for recent in a.recents {
                        ui.ListRow(title: recent.query, icon: .history) {
                            on activate: search(a, recent.query)
                        }
                    }
                }
            }
        }
    }
}

/// One place as a row: what it is, where it is, and what a person does with it.
view place_row(place: Place, chosen: State<Text>) {
    ui.ListRow(
        title: place.name,
        secondary: place.address,
        icon: place.category.icon(),
        meta: category_name(place.category),
        selected: place.name == chosen,
    ) {
        on activate: set(chosen, place.name).then(open_place(atlas, place.key))
        trailing: {
            place_acts(place)
        }
    }
}

/// What a place's row holds: its name and address, with what a person does with it beside.
view place_body(place: Place, chosen: State<Text>) {
    ui.ListRow(
        title: place.name,
        secondary: place.address,
        icon: place.category.icon(),
        meta: category_name(place.category),
        selected: place.name == chosen,
    ) {
        on activate: set(chosen, place.name).then(open_place(atlas, place.key))
        trailing: {
            place_acts(place)
        }
    }
}

/// What a person does with a place wherever it stands: keep it, or ask the way to it.
view place_acts(place: Place) {
    row(gap: .tight) {
        ui.Button(
            if place.saved { "Saved" } else { "Save" },
            icon: .star,
            icon_only: true,
            toggled: place.saved,
            role: .plain,
        ) {
            on press: save_place(place)
        }
        ui.Button("Directions to this place", icon: .route, icon_only: true, role: .plain) {
            on press: directions(atlas, from_words: "", to_words: place.name)
        }
        if place.saved {
            ui.Button("Rename this place", icon: .pencil, icon_only: true, role: .plain) {
                on press: rename_place(place)
            }
        }
    }
}

/// The place the person chose on the map, as a card over it: what it is, where it is, and what
/// they do next.
view place_card(place: Place, chosen: State<Text>) {
    ui.FloatingPanel(width: 340.0) {
        row(distribute: .between, align: .center) {
            row(gap: .related, align: .center) {
                ui.Icon(place.category.icon(), size: .large)
                stack(gap: .tight) {
                    ui.Text(place.name, role: .subheading)
                    if place.address != "" {
                        ui.Text(place.address, role: .caption, secondary: true, lines: 2)
                    }
                }
            }
            ui.Button("Close place details", icon: .x, icon_only: true, role: .plain) {
                on press: set(chosen, "")
            }
        }
        row(gap: .tight) {
            ui.Button(
                if place.saved { "Saved" } else { "Save" },
                icon: .star,
                toggled: place.saved,
            ) {
                on press: save_place(place)
            }
            ui.Button("Directions", icon: .route, role: .primary) {
                on press: directions(atlas, from_words: "", to_words: place.name)
            }
            if place.saved {
                ui.Button("Rename", icon: .pencil) {
                    on press: rename_place(place)
                }
            }
        }
    }
}

/// One point of interest as a row.
view poi_row(poi: Poi) {
    ui.ListRow(
        title: poi.name,
        secondary: if poi.address == "" { poi.kind } else { poi.address },
        icon: poi.category.icon(),
        meta: poi.hours,
    ) {
        on activate: open_poi(atlas, poi.key)
        trailing: {
            ui.Button("Directions to this place", icon: .route, icon_only: true, role: .plain) {
                on press: directions(atlas, from_words: "", to_words: poi.name)
            }
        }
    }
}

/// What a person controls on the map: the zoom, their place, what is nearby, offline maps and
/// the map's own information.
view map_controls(
    a: Atlas,
    chosen: State<Text>,
    sheet: State<Text>,
    least_zoom: State<Int>,
    most_zoom: State<Int>,
) {
    stack(at: .trailing, padding: .related, gap: .tight) {
        ui.Button("Zoom in", icon: .plus, icon_only: true) {
            on press: zoom_map(a, 1.0)
        }
        ui.Button("Zoom out", icon: .minus, icon_only: true) {
            on press: zoom_map(a, -1.0)
        }
        ui.Button("Use my location", icon: .locate_fixed, icon_only: true) {
            on press: locate(a)
        }
        ui.Button("What is nearby", icon: .compass, icon_only: true) {
            on press: find_nearby(a)
        }
        ui.Button("Show traffic", icon: .route, icon_only: true, toggled: a.traffic) {
            on press: toggle_traffic(a)
        }
        ui.Button(
            "Offline maps",
            icon: .cloud_download,
            icon_only: true,
            toggled: sheet == "offline",
        ) {
            on press: set(sheet, if sheet == "offline" { "" } else { "offline" })
        }
        ui.Button("Attribution", icon: .info, icon_only: true) {
            on press: set(sheet, "info")
        }
    }
}

/// What the map is: its credits and the zoom it shows.
view footer(a: Atlas) {
    stack(at: .bottom, fill, padding: .related) {
        row(distribute: .between, align: .center) {
            ui.Text("© OpenStreetMap contributors", role: .caption, secondary: true)
            if a.traffic {
                ui.Text("No traffic data", role: .caption, secondary: true)
            }
            ui.Text("Zoom {a.centre_zoom.round()}", role: .caption, secondary: true, numeric: true)
        }
    }
}

/// Directions: how the person travels, what the service found, its turns, and the journeys public
/// transport offers.
view directions_panel(a: Atlas) {
    state showing: Bool = false
    ui.DisclosureRow(title: "Directions", icon: .route, expanded: showing) {
        on toggle(open: Bool): set(showing, open)
        row(gap: .tight) {
            for mode in modes() {
                ui.Button(mode_name(mode), icon: .route, toggled: a.mode == mode, role: .plain) {
                    on press: set_mode(a, mode)
                }
            }
        }
        if let route = shown_route(a) {
            stack(gap: .tight) {
                ui.Text(
                    "{distance_text(route.distance_m)} · {duration_text(route.duration_s)}",
                    role: .body,
                )
                ui.Text("to {route.to_name}", secondary: true)
                if a.routes.len() > 1 {
                    row(gap: .tight, align: .center) {
                        ui.Button("Another route", icon: .route) {
                            on press: another_route(a)
                        }
                        ui.Text("Route {a.route_at + 1} of {a.routes.len()}", secondary: true)
                    }
                }
                row(gap: .tight) {
                    if a.navigating {
                        ui.Button("End navigation", icon: .x, role: .primary) {
                            on press: end_navigation(a)
                        }
                    } else {
                        ui.Button("Start navigation", icon: .navigation, role: .primary) {
                            on press: start_navigation(a)
                        }
                    }
                    ui.Button("Clear route", icon: .trash_2, role: .plain) {
                        on press: clear_route(a)
                    }
                }
                if not route.steps.is_empty() {
                    ui.Section(title: "Turns", collapsible: true) {
                        for step in route.steps {
                            ui.ListRow(
                                title: step.instruction,
                                meta: distance_text(step.distance_m),
                            )
                        }
                    }
                }
            }
        } else if a.mode == .transit {
            transit_panel(a)
        } else {
            ui.Text("Open a place and ask for directions to it.", secondary: true)
        }
    }
}

/// The journeys public transport offers, and when the person wants to travel.
view transit_panel(a: Atlas) {
    stack(gap: .tight) {
        row(gap: .tight, align: .center) {
            ui.Button("Leave at", toggled: not a.transit_time.arrive_by, role: .plain) {
                on press: set_transit_time(a, false, a.transit_time.offset_min)
            }
            ui.Button("Arrive by", toggled: a.transit_time.arrive_by, role: .plain) {
                on press: set_transit_time(a, true, a.transit_time.offset_min)
            }
            ui.Button("Earlier", icon: .minus, icon_only: true, role: .plain) {
                on press: set_transit_time(
                    a,
                    a.transit_time.arrive_by,
                    a.transit_time.offset_min - 30,
                )
            }
            ui.Text("{a.transit_time.offset_min.abs()} min", numeric: true)
            ui.Button("Later", icon: .plus, icon_only: true, role: .plain) {
                on press: set_transit_time(
                    a,
                    a.transit_time.arrive_by,
                    a.transit_time.offset_min + 30,
                )
            }
        }
        if let journey = shown_journey(a) {
            stack(gap: .related) {
                row(distribute: .between) {
                    ui.Text("{journey.dep}–{journey.arr}", role: .body)
                    ui.Text(
                        if journey.transfers == 0 {
                            "Direct"
                        } else { "{journey.transfers} changes" },
                        secondary: true,
                    )
                }
                for leg in journey.legs {
                    leg_row(leg)
                }
                if a.journeys.len() > 1 {
                    ui.Button("Another journey", icon: .bus) {
                        on press: another_journey(a)
                    }
                }
                ui.Button("Clear transit", icon: .trash_2, role: .plain) {
                    on press: clear_transit(a)
                }
            }
        } else {
            ui.Text("Choose a place to travel to", secondary: true)
        }
    }
}

/// One leg of a journey.
view leg_row(leg: Leg) {
    ui.ListRow(
        title: if leg.line == "" {
            "Walk to {leg.to_name}"
        } else { "{leg.line} towards {leg.headsign}" },
        secondary: "{leg.from_name} → {leg.to_name}",
        meta: "{leg.dep}–{leg.arr}",
        icon: if leg.mode == .walk { .navigation } else { .bus },
    )
}

/// The offline maps: save what the map shows or a place for later, and what this device kept.
view offline_sheet(a: Atlas, sheet: State<Text>, least_zoom: State<Int>, most_zoom: State<Int>) {
    state place: Text = ""
    ui.FloatingPanel(width: 380.0) {
        row(distribute: .between, align: .center) {
            ui.Text("Offline maps", role: .heading)
            ui.Button("Close offline maps", icon: .x, icon_only: true, role: .plain) {
                on press: set(sheet, "")
            }
        }
        row(gap: .tight, align: .center) {
            ui.Button("Fewer zoom levels", icon: .minus, icon_only: true, role: .plain) {
                on press: set(least_zoom, (least_zoom + 1).min(most_zoom))
            }
            ui.Text("Zoom levels {least_zoom}–{most_zoom}", numeric: true)
            ui.Button("More zoom levels", icon: .plus, icon_only: true, role: .plain) {
                on press: set(most_zoom, (most_zoom + 1).min(19))
            }
        }
        ui.Button("Download this view", icon: .cloud_download, role: .primary) {
            on press: download_view(a, least_zoom, most_zoom)
        }
        row(gap: .tight, align: .center) {
            ui.TextInput(bind: place, placeholder: "Search for a city or a place", fill)
            ui.Button("Download place", icon: .cloud_download) {
                on press: download_place(a, place, least_zoom, most_zoom)
            }
        }
        if a.offline.is_empty() {
            ui.EmptyState(
                "Search for a place, or download what the map shows now.",
                icon: .cloud_download,
                inline: true,
            )
        } else {
            stack(gap: .tight) {
                ui.Text("Downloaded areas", role: .subheading)
                for area in a.offline {
                    area_row(a, area)
                }
                ui.Button("Clear offline maps", icon: .trash_2, role: .plain) {
                    on press: clear_offline_areas(a)
                }
            }
        }
    }
}

/// One area this device keeps, and how far saving it has come.
view area_row(a: Atlas, area: OfflineArea) {
    stack(gap: .tight) {
        row(distribute: .between, align: .center) {
            ui.Text(area.name, role: .body)
            row(gap: .tight) {
                ui.Text(
                    "{area.done} of {area.tiles} tiles",
                    role: .caption,
                    numeric: true,
                    secondary: true,
                )
                if area.stopped {
                    ui.Button("Download it again", icon: .cloud_download) {
                        on press: continue_offline_area(a, area.name)
                    }
                } else if area.done < area.tiles {
                    ui.Button("Stop downloading", icon: .x) {
                        on press: stop_offline_area(a, area.name)
                    }
                }
                ui.Button("Delete downloaded area", icon: .trash_2, icon_only: true, role: .plain) {
                    on press: delete_offline_area(a, area.name)
                }
            }
        }
        if area.tiles > 0 {
            ui.ProgressBar(area.done.to_float() / area.tiles.to_float())
        }
        if area.stopped {
            ui.Text(
                "Stopped; what it kept stays available offline.",
                role: .caption,
                secondary: true,
            )
        }
    }
}

/// What the map's imagery is and who it belongs to.
view info_dialog(sheet: State<Text>) {
    ui.ModalDialog(title: "Attribution", open: true, primary: "Done") {
        on act_primary: set(sheet, "")
        on dismiss: set(sheet, "")
        body: {
            stack(gap: .related) {
                ui.Text("Map data and map tiles © OpenStreetMap contributors.", role: .body)
                ui.Text(
                    "The map data is © OpenStreetMap contributors, available under the Open Database License (ODbL). The map tiles are OpenStreetMap Standard tiles, used under the OpenStreetMap Foundation's tile usage policy.",
                    role: .caption,
                    secondary: true,
                )
            }
        }
    }
}

/// The card that says the turn ahead while the person follows a route.
view turn_card(a: Atlas) {
    if let route = shown_route(a) {
        ui.FloatingPanel(width: 320.0) {
            row(gap: .related, align: .center) {
                ui.Icon(.navigation, size: .large)
                stack(gap: .tight) {
                    if let step = route.steps.at(0) {
                        ui.Text(step.instruction, role: .subheading)
                        ui.Text("{distance_text(route.distance_m)} to go", secondary: true)
                    } else {
                        ui.Text("Continue on the route", role: .subheading)
                    }
                }
                ui.Button("End", icon: .x, role: .primary) {
                    on press: end_navigation(a)
                }
            }
        }
    }
}

/// What the shell shows while a person follows a route: the turn ahead and the way to end it.
view live(a: Atlas) {
    if let route = shown_route(a) {
        row(gap: .related, align: .center) {
            ui.Icon(.navigation)
            ui.Text(
                if let step = turn_ahead(route.steps) {
                    step.instruction
                } else { "Continue on the route" },
            )
            ui.Button("End", icon: .x, role: .plain) {
                on press: end_navigation(a)
            }
        }
    }
}

// A place.
default view place for Place

/// A place's own page: what it is, where it is, and what a person does with it.
view place(p: Place) {
    stack(padding: .group, gap: .related) {
        row(gap: .related, align: .center) {
            ui.Icon(p.category.icon(), size: .large)
            stack(gap: .tight) {
                ui.Text(p.name, role: .heading)
                if p.address != "" {
                    ui.Text(p.address, secondary: true)
                }
            }
        }
        ui.Facts {
            ui.Fact(text: category_name(p.category), icon: p.category.icon())
        }
        row(gap: .tight) {
            ui.Button(if p.saved { "Saved" } else { "Save" }, icon: .star, toggled: p.saved) {
                on press: save_place(p)
            }
            ui.Button("Directions", icon: .route) {
                on press: directions(atlas, from_words: "", to_words: p.name)
            }
            ui.Button("Open on the map", icon: .map) {
                on press: open_place(atlas, p.key)
            }
            if p.saved {
                ui.Button("Rename", icon: .pencil) {
                    on press: rename_place(p)
                }
            }
        }
    }
}
